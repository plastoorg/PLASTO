//! Окно и цикл событий: winit → egui → видеокарта (wgpu) или, если её нет,
//! растеризация на CPU → `UpdateLayeredWindow`.
//!
//! Кадр рисуется только когда есть повод: ввод, событие бэкенда, анимация.
//! Пока окно спрятано в трей, кадров нет вовсе — работает только логика
//! (`App::tick`), чтобы ответы и события не копились. Видеокарта в трее
//! отдаётся целиком, вместе с контекстом egui: при показе всё строится заново.

use std::{
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use egui::{ViewportCommand, ViewportId, ViewportInfo};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::json;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Icon, Window, WindowId, WindowLevel},
};

use super::{gpu::Gpu, theme, App, WinCmd, BASE_H, BASE_W, MAX_H, MAX_W};
use crate::{
    api,
    host::{Event, Host},
    platform,
};

pub struct Options {
    pub size: [f32; 2],
    pub pos: Option<[f32; 2]>,
    pub on_top: bool,
    pub autostart: bool,
    /// Запущен новой версией после самообновления.
    pub after_update: bool,
    pub start_hidden: bool,
}

/// Разбудить цикл: «перерисуй к такому-то моменту».
struct Wake(Instant);

/// Куда уходит кадр.
enum Output {
    /// Видеокарта. `None` — окно спрятано и устройство отдано: поднимем при показе.
    Gpu(Option<Box<Gpu>>),
    /// CPU и `UpdateLayeredWindow`: видеокарты нет, драйвер не поднялся или
    /// вывод на CPU задан явно (`PLASTO_RENDERER=cpu`).
    Cpu {
        #[cfg(windows)]
        surface: platform::Layered,
        renderer: Box<EguiSoftwareRender>,
    },
}

/// Пока окно рисует видеокарта, под интерфейсом сплошной фон: окно непрозрачное.
const CLEAR: [f32; 4] = [0.010, 0.008, 0.008, 1.0];

struct Shell {
    ctx: egui::Context,
    app: App,
    host: Host,
    opts: Options,
    proxy: EventLoopProxy<Wake>,
    window: Option<Arc<Window>>,
    state: Option<egui_winit::State>,
    out: Output,
    info: ViewportInfo,
    visible: bool,
    dirty: bool,
    next: Option<Instant>,
    quit: bool,
}

pub fn run(host: Host, events: mpsc::Receiver<Event>, opts: Options) -> Result<(), String> {
    let event_loop = EventLoop::<Wake>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let ctx = new_context(&proxy);
    // Бэкенд и трей будят цикл сами, мимо egui: пока окно в трее, проходов
    // egui нет, и его собственный колбэк перерисовки молчал бы.
    let wake_proxy = Mutex::new(event_loop.create_proxy());
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Ok(p) = wake_proxy.lock() {
            let _ = p.send_event(Wake(Instant::now()));
        }
    });
    let app = App::new(&ctx, host.clone(), events, opts.start_hidden, opts.after_update, wake);
    let mut shell = Shell {
        ctx,
        app,
        host,
        visible: false,
        opts,
        proxy,
        window: None,
        state: None,
        out: Output::Gpu(None),
        info: ViewportInfo::default(),
        dirty: true,
        next: None,
        quit: false,
    };
    event_loop.run_app(&mut shell).map_err(|e| e.to_string())
}

/// Контекст egui, который будит цикл, когда сам просит перерисовку.
fn new_context(proxy: &EventLoopProxy<Wake>) -> egui::Context {
    let ctx = egui::Context::default();
    let proxy = proxy.clone();
    ctx.set_request_repaint_callback(move |info| {
        let at = Instant::now().checked_add(info.delay).unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        let _ = proxy.send_event(Wake(at));
    });
    ctx
}

#[cfg(windows)]
fn hwnd_of(window: &Window) -> Result<isize, String> {
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Ok(h.hwnd.get()),
        _ => Err("нет HWND окна".into()),
    }
}

#[cfg(windows)]
fn cpu_output(hwnd: isize) -> Output {
    Output::Cpu { surface: platform::Layered::new(hwnd), renderer: Box::new(EguiSoftwareRender::new(ColorFieldOrder::Bgra)) }
}

fn to_winit_dir(d: egui::ResizeDirection) -> winit::window::ResizeDirection {
    use egui::ResizeDirection as E;
    use winit::window::ResizeDirection as W;
    match d {
        E::North => W::North,
        E::South => W::South,
        E::East => W::East,
        E::West => W::West,
        E::NorthEast => W::NorthEast,
        E::NorthWest => W::NorthWest,
        E::SouthEast => W::SouthEast,
        E::SouthWest => W::SouthWest,
    }
}

impl Shell {
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let [w, h] = self.opts.size;
        let mut attrs = Window::default_attributes()
            .with_title("PLASTO")
            .with_inner_size(LogicalSize::new(w, h))
            .with_min_inner_size(LogicalSize::new(BASE_W, BASE_H))
            .with_max_inner_size(LogicalSize::new(MAX_W, MAX_H))
            .with_decorations(false)
            .with_resizable(true)
            // Показываем, когда первый кадр уже готов: иначе мелькнёт пустое окно.
            .with_visible(false);
        // Позиция из конфига — только если она на одном из нынешних мониторов.
        let pos = self.opts.pos.filter(|[x, y]| {
            event_loop.available_monitors().any(|m| {
                let sf = m.scale_factor();
                let p = m.position().to_logical::<f64>(sf);
                let s = m.size().to_logical::<f64>(sf);
                (*x as f64) >= p.x - 40.0 && (*x as f64) < p.x + s.width - 40.0 && (*y as f64) >= p.y - 10.0 && (*y as f64) < p.y + s.height - 40.0
            })
        });
        match pos {
            Some([x, y]) => attrs = attrs.with_position(LogicalPosition::new(x, y)),
            None => {
                if let Some(m) = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next()) {
                    let sf = m.scale_factor();
                    let p = m.position().to_logical::<f64>(sf);
                    let s = m.size().to_logical::<f64>(sf);
                    attrs = attrs.with_position(LogicalPosition::new(p.x + (s.width - w as f64) / 2.0, p.y + (s.height - h as f64) / 2.0));
                }
            }
        }
        if self.opts.on_top {
            attrs = attrs.with_window_level(WindowLevel::AlwaysOnTop);
        }
        // Значок окна — из ресурса exe (assets/plasto.rc), тот же, что в трее.
        #[cfg(windows)]
        {
            use winit::platform::windows::IconExtWindows;
            if let Ok(icon) = Icon::from_resource(1, None) {
                attrs = attrs.with_window_icon(Some(icon));
            }
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        #[cfg(windows)]
        {
            let hwnd = hwnd_of(&window)?;
            let cpu = std::env::var("PLASTO_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
            self.out = if cpu {
                cpu_output(hwnd)
            } else {
                platform::round_corners(hwnd);
                Output::Gpu(None)
            };
        }
        self.state = Some(egui_winit::State::new(self.ctx.clone(), ViewportId::ROOT, event_loop, Some(window.scale_factor() as f32), None, None));
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, &window, true);
        self.window = Some(window);
        Ok(())
    }

    /// Поднять видеокарту к показу окна. Не вышло — дальше рисуем на CPU.
    fn ensure_gpu(&mut self) {
        let Output::Gpu(None) = self.out else { return };
        let Some(window) = self.window.clone() else { return };
        match Gpu::new(&self.ctx, Arc::clone(&window)) {
            Ok(gpu) => {
                if let Some(state) = self.state.as_mut() {
                    state.set_max_texture_side(gpu.max_texture_side().unwrap_or(8192));
                }
                self.out = Output::Gpu(Some(Box::new(gpu)));
            }
            Err(e) => {
                eprintln!("[plasto] видеокарта недоступна, рисую на CPU: {e}");
                #[cfg(windows)]
                if let Ok(hwnd) = hwnd_of(&window) {
                    self.out = cpu_output(hwnd);
                }
            }
        }
    }

    /// Новый контекст egui взамен отданного вместе с видеокартой: при показе он
    /// заново соберёт и отправит атлас шрифтов, а в трее не держит кэшей.
    fn renew_context(&mut self) {
        let Some(window) = self.window.as_ref() else { return };
        self.ctx = new_context(&self.proxy);
        theme::install_fonts(&self.ctx);
        super::kit::install_style(&self.ctx);
        self.state = Some(egui_winit::State::new(self.ctx.clone(), ViewportId::ROOT, window.as_ref(), Some(window.scale_factor() as f32), None, None));
        self.info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, window, true);
    }

    /// Один проход egui. `present: false` — только чтобы egui отдал
    /// освобождённые текстуры (после ухода в трей), без вывода на экран.
    fn paint(&mut self, present: bool) {
        let (Some(window), Some(state)) = (self.window.as_ref(), self.state.as_mut()) else { return };
        let mut raw = state.take_egui_input(window);
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, window, false);
        raw.viewports.insert(ViewportId::ROOT, self.info.clone());
        let app = &mut self.app;
        app.set_gpu(matches!(self.out, Output::Gpu(Some(_))));
        let out = self.ctx.run_ui(raw, |ui| {
            app.tick(ui.ctx());
            app.ui(ui);
        });
        state.handle_platform_output(window, out.platform_output);
        let (commands, delay) = out
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| (v.commands.clone(), v.repaint_delay))
            .unwrap_or((Vec::new(), Duration::MAX));
        // egui ждёт, что цикл сам выполнит запрошенную в проходе перерисовку.
        if present {
            if delay.is_zero() {
                self.dirty = true;
            } else if let Some(at) = Instant::now().checked_add(delay).filter(|_| delay < Duration::from_secs(3600)) {
                self.next = Some(self.next.map_or(at, |n| n.min(at)));
            }
        }
        let size = window.inner_size();
        let (w, h) = (size.width as usize, size.height as usize);
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        #[cfg(windows)]
        if let Output::Cpu { surface, renderer } = &mut self.out {
            if w > 0 && h > 0 {
                if let Some(pixels) = surface.pixels(w, h) {
                    pixels.fill([0; 4]);
                    renderer.render(&mut BufferMutRef::new(pixels, w, h), &prims, &out.textures_delta, out.pixels_per_point);
                }
            }
        }
        for cmd in commands {
            match cmd {
                ViewportCommand::StartDrag => {
                    let _ = window.drag_window();
                }
                ViewportCommand::BeginResize(dir) => {
                    let _ = window.drag_resize_window(to_winit_dir(dir));
                }
                ViewportCommand::WindowLevel(level) => window.set_window_level(match level {
                    egui::WindowLevel::AlwaysOnTop => WindowLevel::AlwaysOnTop,
                    egui::WindowLevel::AlwaysOnBottom => WindowLevel::AlwaysOnBottom,
                    egui::WindowLevel::Normal => WindowLevel::Normal,
                }),
                _ => {}
            }
        }
        // Вывод — после команд окна: смена уровня окна сбрасывает его стиль.
        if present && w > 0 && h > 0 {
            match &mut self.out {
                Output::Gpu(Some(gpu)) => gpu.paint((size.width, size.height), out.pixels_per_point, CLEAR, &prims, &out.textures_delta),
                #[cfg(windows)]
                Output::Cpu { surface, .. } => {
                    surface.present();
                }
                Output::Gpu(None) => {}
            }
        }
    }

    fn save_geometry(&self) {
        let Some(window) = &self.window else { return };
        let sf = window.scale_factor();
        let size = window.inner_size().to_logical::<f64>(sf);
        let mut patch = json!({ "w": size.width.round(), "h": size.height.round() });
        if let Ok(pos) = window.outer_position() {
            let pos = pos.to_logical::<f64>(sf);
            patch["x"] = json!(pos.x.round());
            patch["y"] = json!(pos.y.round());
        }
        api::call(&self.host, "config:set", patch);
    }

    fn show(&mut self) {
        if self.visible {
            if let Some(w) = &self.window {
                w.set_minimized(false);
                w.focus_window();
            }
            return;
        }
        self.visible = true;
        self.ensure_gpu();
        if let Some(w) = &self.window {
            w.set_visible(true);
            w.set_minimized(false);
        }
        self.paint(true);
        if let Some(w) = &self.window {
            w.focus_window();
        }
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.save_geometry();
        self.visible = false;
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
        match &mut self.out {
            // Устройство, поверхность и текстуры — целиком; контекст egui с ними.
            Output::Gpu(gpu) => {
                *gpu = None;
                self.renew_context();
            }
            #[cfg(windows)]
            Output::Cpu { .. } => {
                // Проход без вывода: egui отдаст освобождённые фоновые кадры.
                self.paint(false);
                if let Output::Cpu { surface, .. } = &mut self.out {
                    surface.release();
                }
            }
        }
        platform::trim_memory();
    }

    fn apply_commands(&mut self) {
        for cmd in self.app.take_window_cmds() {
            match cmd {
                WinCmd::Show => self.show(),
                WinCmd::Hide => self.hide(),
                WinCmd::Minimize => {
                    if let Some(w) = &self.window {
                        w.set_minimized(true);
                    }
                }
                WinCmd::Quit => {
                    if self.visible {
                        self.save_geometry();
                    }
                    self.quit = true;
                }
            }
        }
    }

    fn frame(&mut self) {
        // Свёрнутому окну рисовать некуда: как в трее, идёт только логика.
        // Развернётся — Resized сам нарисует кадр и снова запустит анимацию.
        let minimized = self.window.as_ref().and_then(|w| w.is_minimized()).unwrap_or(false);
        self.app.set_minimized(minimized);
        if self.visible && !minimized {
            self.paint(true);
        } else {
            self.app.tick(&self.ctx);
        }
        self.apply_commands();
    }
}

impl ApplicationHandler<Wake> for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.create_window(event_loop) {
            eprintln!("[plasto] окно не создано: {e}");
            event_loop.exit();
            return;
        }
        if self.app.is_hidden() {
            // Запуск из автозагрузки: сразу в трей, кадр нарисуем при показе.
            self.app.tick(&self.ctx);
            self.apply_commands();
        } else {
            self.show();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let response = {
            let (Some(window), Some(state)) = (self.window.as_ref(), self.state.as_mut()) else { return };
            state.on_window_event(window, &event)
        };
        match event {
            WindowEvent::CloseRequested => {
                // Alt+F4 и закрытие из панели задач — как кнопка «закрыть».
                self.app.close_pressed();
                self.dirty = true;
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                // Сразу, а не в about_to_wait: при растягивании за край Windows
                // крутит свой модальный цикл, и кадр должен успевать за рамкой.
                if self.visible {
                    self.paint(true);
                }
            }
            _ => {
                if response.repaint {
                    self.dirty = true;
                }
            }
        }
        if self.quit {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, Wake(at): Wake) {
        if at <= Instant::now() {
            self.dirty = true;
        } else {
            self.next = Some(self.next.map_or(at, |n| n.min(at)));
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.next.is_some_and(|t| t <= now) {
            self.next = None;
            self.dirty = true;
        }
        if self.dirty {
            self.dirty = false;
            self.frame();
        }
        if self.quit {
            self.app.shutdown();
            event_loop.exit();
            return;
        }
        // Проход сам попросил ещё один (анимация, второй кадр после ввода) —
        // крутим цикл сразу, а не ждём следующего события ОС.
        event_loop.set_control_flow(match self.next {
            _ if self.dirty => ControlFlow::Poll,
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.app.shutdown();
    }
}
