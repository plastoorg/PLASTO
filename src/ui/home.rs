//! Окно нового вида: сцена во всё окно, затемнение краёв, шапка и главная.
//!
//! Сцена, стекло и главная уже новые; разделы, которых ещё нет, открывают
//! прежнюю панель настроек на нужной вкладке.

use std::time::Instant;

use egui::{pos2, vec2, Color32, Mesh, Rect, Sense, Ui, ViewportCommand};

use super::{
    glass,
    icons::{self, Icon},
    kit::{self, Tone},
    scene::Screen,
    state_def, App, DiagTab, Mech, Page,
};

/// Вертикальный градиент по точкам `(y от верха rect, цвет)`.
fn vertical_gradient(rect: Rect, stops: &[(f32, Color32)]) -> Mesh {
    let mut mesh = Mesh::default();
    for &(y, c) in stops {
        let y = rect.top() + y;
        mesh.colored_vertex(pos2(rect.left(), y), c);
        mesh.colored_vertex(pos2(rect.right(), y), c);
    }
    for k in 0..stops.len().saturating_sub(1) as u32 {
        let a = k * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    mesh
}

/// Затемнение краёв, чтобы текст читался на любом кадре: сверху голубоватое
/// (продолжение неба) с лёгким переливом под шапкой, снизу — плотнее.
fn paint_scrims(ui: &Ui, rect: Rect) {
    let p = ui.painter();
    let clear = Color32::TRANSPARENT;
    let top = Color32::from_rgba_unmultiplied(20, 45, 64, 66);
    let sheen = Color32::from_rgba_unmultiplied(184, 211, 226, 28);
    p.add(vertical_gradient(Rect::from_min_size(rect.min, vec2(rect.width(), 104.0)), &[(0.0, top), (104.0, clear)]));
    p.add(vertical_gradient(Rect::from_min_size(rect.min, vec2(rect.width(), 60.0)), &[(0.0, sheen), (60.0, clear)]));
    let h = 232.0;
    let band = Rect::from_min_size(pos2(rect.left(), rect.bottom() - h), vec2(rect.width(), h));
    let deep = Color32::from_rgba_unmultiplied(5, 13, 9, 194);
    let mid = Color32::from_rgba_unmultiplied(5, 13, 9, 77);
    p.add(vertical_gradient(band, &[(0.0, clear), (h - 140.0, mid), (h, deep)]));
}

/// Время работы как в макете: «0:05:09».
pub fn uptime(since: Instant) -> String {
    let t = since.elapsed().as_secs();
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

impl App {
    pub(super) fn window_ui(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let screen = self.first_screen().unwrap_or_else(|| match &self.boot {
            Some(boot) => Screen::Boot(boot.frac),
            None if self.tests.running => Screen::Boot(self.tests.frac),
            None => Screen::from_state(&self.state),
        });
        #[cfg(debug_assertions)]
        let screen = debug_screen().unwrap_or(screen);
        #[cfg(debug_assertions)]
        if ui.ctx().cumulative_pass_nr() < 2 {
            debug_page(self);
        }
        if !self.hidden {
            self.scene.paint(ui, rect, screen, self.gpu, self.cfg_str("background") != Some("photo"));
        }
        paint_scrims(ui, rect);
        #[cfg(debug_assertions)]
        if std::env::var("PLASTO_KIT").is_ok() {
            kit_gallery(ui, rect, self.gpu);
            self.header_ui(ui, rect);
            return;
        }
        if self.page == Page::Home {
            if self.first.is_some() {
                self.first_panel(ui, rect);
            } else {
                self.services_panel(ui, rect);
            }
            self.home_ui(ui, rect);
        } else {
            // За страницами сцена остаётся видна: слабое размытие и тень.
            glass::veil(ui, rect, self.gpu);
            self.page_ui(ui, rect);
        }
        self.header_ui(ui, rect);
    }

    fn interactive(&self) -> bool {
        self.close_dialog.is_none()
    }

    /// Шапка как в концепте: логотип, разделитель, разделы текстом (активный —
    /// белым); справа плашка, только если с обходом что-то не так, «свернуть»
    /// и «закрыть». Пустое место шапки тянет окно.
    fn header_ui(&mut self, ui: &mut Ui, rect: Rect) {
        let head = Rect::from_min_size(rect.min, vec2(rect.width(), 60.0));
        let interactive = self.interactive();
        if interactive {
            let drag = ui.interact(head, ui.id().with("drag"), Sense::click_and_drag());
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            }
        }
        let p = ui.painter().clone();
        let cy = head.center().y;
        let mut job = egui::text::LayoutJob::simple_singleline("PLASTO".into(), kit::semibold(14.0), kit::INK);
        job.sections[0].format.extra_letter_spacing = 2.5;
        let logo = p.layout_job(job);
        let x = rect.left() + 24.0;
        p.galley(pos2(x, cy - logo.size().y / 2.0), logo.clone(), kit::INK);
        let divider_x = x + logo.size().x + 24.0;
        p.line_segment([pos2(divider_x, cy - 9.0), pos2(divider_x, cy + 9.0)], egui::Stroke::new(1.0_f32, kit::HAIR_2));

        let mut nx = divider_x + 8.0;
        for (label, page) in [("Главная", Page::Home), ("Сервисы", Page::Services), ("Диагностика", Page::Diag), ("Настройки", Page::Settings)] {
            let active = self.page == page;
            let font = if active { kit::semibold(15.0) } else { kit::regular(15.0) };
            let g = p.layout_no_wrap(label.to_owned(), font, kit::INK);
            let r = Rect::from_min_size(pos2(nx, cy - 18.0), vec2(g.size().x + 24.0, 36.0));
            let resp = ui.interact(r, ui.id().with(("nav", label)), if interactive { Sense::click() } else { Sense::hover() }).on_hover_cursor(egui::CursorIcon::PointingHand);
            let color = if active { kit::INK_STRONG } else if resp.hovered() { kit::INK } else { kit::INK_3 };
            let gw = g.size().x;
            p.galley(pos2(r.left() + 12.0, cy - g.size().y / 2.0), g, color);
            // Есть обновление: маленькая точка у «Настроек», без всплывашек.
            if page == Page::Settings && self.upd.any() {
                kit::dot(&p, pos2(r.left() + 12.0 + gw + 5.0, cy - 7.0), Tone::Ok, 3.0);
            }
            kit::focus_ring(&p, &resp, r, 8.0);
            if resp.clicked() && !active {
                self.go(page);
            }
            nx = r.right() + 4.0;
        }

        let close = Rect::from_center_size(pos2(rect.right() - 12.0 - 22.0, cy), vec2(44.0, 36.0));
        let minimize = close.translate(vec2(-48.0, 0.0));
        // Плашка в шапке — только когда с обходом что-то не так, и не на главной.
        let alert = match self.state.as_str() {
            "timeout" => Some(Tone::Warn),
            "error" | "critical" => Some(Tone::Bad),
            _ => None,
        };
        if let (Some(tone), false) = (alert, self.page == Page::Home) {
            let (bg, fg) = if tone == Tone::Warn { (kit::WARN_SOFT, kit::WARN_TEXT) } else { (kit::BAD_SOFT, kit::BAD_TEXT) };
            let title = state_def(&self.state).title;
            let g = p.layout_no_wrap(title.to_owned(), kit::semibold(13.0), fg);
            let chip = Rect::from_min_size(pos2(minimize.left() - 12.0 - g.size().x - 36.0, cy - 16.0), vec2(g.size().x + 36.0, 32.0));
            let resp = ui.interact(chip, ui.id().with("alert"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Открыть главную");
            p.rect_filled(chip, 16.0, bg);
            kit::dot(&p, pos2(chip.left() + 16.0, cy), tone, 4.0);
            p.galley(pos2(chip.left() + 28.0, cy - g.size().y / 2.0), g, fg);
            if resp.clicked() {
                self.go(Page::Home);
            }
        }
        for (r, icon, label, is_close) in [(minimize, Icon::Minus, "Свернуть", false), (close, Icon::Cross, "Закрыть", true)] {
            let resp = ui.interact(r, ui.id().with(label), if interactive { Sense::click() } else { Sense::hover() }).on_hover_text(label);
            let hover = resp.hovered() && interactive;
            if hover {
                p.rect_filled(r, 8.0, if is_close { kit::CLOSE } else { kit::FILL_2 });
            }
            icons::paint(&p, r.center(), icon, 16.0, if hover { kit::INK_STRONG } else { kit::INK_2 });
            if resp.clicked() {
                if is_close {
                    self.close_pressed();
                } else {
                    self.minimize();
                }
            }
        }
    }

    /// Панель «Что работает» справа сверху: по строке на механизм обхода —
    /// значок, название, подпись, состояние и тумблер. Строка открывает
    /// настройки механизма на странице «Сервисы».
    fn services_panel(&mut self, ui: &mut Ui, rect: Rect) {
        let mechs = [Mech::Zapret, Mech::Telegram, Mech::Ai, Mech::Dns];
        let row_h = 64.0;
        let panel = Rect::from_min_size(pos2(rect.right() - 24.0 - 440.0, rect.top() + 76.0), vec2(440.0, 16.0 + 40.0 + row_h * mechs.len() as f32 + 12.0));
        glass::panel(ui, 1, panel, 16.0, self.gpu);
        let interactive = self.interactive();
        let p = ui.painter().clone();
        let head = p.layout_no_wrap("Что работает".to_owned(), kit::semibold(17.0), kit::INK);
        let head_y = panel.top() + 16.0 + 20.0 - 4.0;
        p.galley(pos2(panel.left() + 24.0, head_y - head.size().y / 2.0), head, kit::INK);
        // Справа в заголовке: когда меряли пинг, и кнопка «перемерить».
        let any_on = mechs.iter().any(|m| self.mech_info(*m).on);
        if any_on {
            let refresh = Rect::from_center_size(pos2(panel.right() - 12.0 - 12.0 - 16.0, head_y), vec2(32.0, 32.0));
            let mut cell = ui.new_child(egui::UiBuilder::new().id_salt("ping-refresh").max_rect(refresh));
            if kit::icon_button(&mut cell, Icon::Refresh, "Обновить пинг", self.ping.busy).clicked() && interactive {
                self.run_ping();
            }
            let when = if self.ping.busy { "проверяю…".to_owned() } else { self.ping.at.map(|at| format!("проверено {}", super::health::ago(at))).unwrap_or_default() };
            if !when.is_empty() {
                let g = p.layout_no_wrap(when, kit::regular(13.0), kit::INK_3);
                p.galley(pos2(refresh.left() - 8.0 - g.size().x, head_y - g.size().y / 2.0), g, kit::INK_3);
                if let Some(at) = self.ping.at {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(60 - at.elapsed().as_secs() % 60));
                }
            }
        }
        let mut y = panel.top() + 16.0 + 40.0;
        for mech in mechs {
            let info = self.mech_info(mech);
            let row = Rect::from_min_size(pos2(panel.left() + 12.0, y), vec2(panel.width() - 24.0, row_h));
            let resp = ui.interact(row, ui.id().with(("row", mech as u8)), if interactive { Sense::click() } else { Sense::hover() }).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() && interactive {
                p.rect_filled(row, 12.0, kit::FILL_1);
            }
            icons::paint(&p, pos2(row.left() + 24.0, row.center().y), info.icon, 22.0, kit::INK_2);
            let name_g = p.layout_no_wrap(info.name.to_owned(), kit::semibold(15.0), kit::INK);
            let sub_g = super::theme::elided(ui, &info.sub, kit::regular(13.0), kit::INK_3, 200.0);
            let block = name_g.size().y + 4.0 + sub_g.size().y;
            let top = row.center().y - block / 2.0;
            p.galley(pos2(row.left() + 48.0, top), name_g, kit::INK);
            p.galley(pos2(row.left() + 48.0, top + block - sub_g.size().y), sub_g, kit::INK);
            let sw = Rect::from_min_size(pos2(row.right() - 12.0 - 48.0, row.center().y - 14.0), vec2(48.0, 28.0));
            // Включённое показывает пинг: «142 мс» или «нет ответа»; ошибки zapret — как есть.
            // Пока тумблер ждёт, вместо пинга — что сейчас делается.
            let ping = if info.on && info.tone != Tone::Bad && !info.busy { self.ping_text(mech) } else { None };
            let (text, tone, tip) = match ping {
                Some(p) => p,
                None if info.on && self.ping.busy && !info.busy => ("…".to_owned(), Tone::Off, String::new()),
                None => (info.status.to_owned(), info.tone, String::new()),
            };
            let color = match tone {
                Tone::Off => kit::INK_3,
                Tone::Warn => kit::WARN_TEXT,
                Tone::Bad => kit::BAD_TEXT,
                Tone::Ok => kit::INK,
            };
            let st = p.layout_no_wrap(text, kit::regular(14.0), color);
            let sx = sw.left() - 16.0 - st.size().x;
            let st_rect = Rect::from_min_size(pos2(sx, row.center().y - st.size().y / 2.0), st.size());
            p.galley(st_rect.min, st, kit::INK);
            if tone != Tone::Off {
                kit::dot(&p, pos2(sx - 12.0, row.center().y), tone, 4.0);
            }
            if !tip.is_empty() && ui.rect_contains_pointer(st_rect.expand(6.0)) {
                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), ui.id().with(("ping-tip", mech as u8)), egui::PopupAnchor::Pointer).show(|ui| {
                    ui.label(tip);
                });
            }
            let switched = kit::switch_at(ui, sw, info.on, interactive && !info.busy, info.name).clicked();
            if info.busy {
                kit::switch_spinner(ui, sw, info.on);
            }
            if switched {
                self.mech_switch(mech);
            } else if resp.clicked() {
                self.open_mech(mech);
            }
            y += row_h;
        }
    }

    /// Главная: состояние слева, под монитором; кнопка обхода справа, под
    /// панелью, слева от неё — время работы или следующий шаг при проблеме.
    fn home_ui(&mut self, ui: &mut Ui, rect: Rect) {
        let p = ui.painter().clone();
        let def = state_def(&self.state);
        let (title, sub) = if self.first.is_some() {
            self.first_title()
        } else {
            match &self.boot {
                Some(boot) => (boot.title.clone(), boot.sub.clone()),
                None if self.state == "testing" => {
                    let why = if self.ping.auto_running { "Сайты не открывались — ищу рабочую стратегию" } else { "Перебираю стратегии" };
                    let live = &self.tests.live;
                    let now = if live.label.is_empty() { String::new() } else { format!(" · {} ({} из {})", live.label, live.index + 1, live.count.max(1)) };
                    (def.title.to_owned(), format!("{why}{now} · {}%", (self.tests.frac * 100.0) as u32))
                }
                None => {
                    let sub = match (&self.flash, &self.reason) {
                        (Some((text, _)), _) => text.clone(),
                        (None, Some(reason)) => super::capitalize(reason),
                        (None, None) => def.sub.to_owned(),
                    };
                    // Пинг YouTube и Discord прошёл — это уже «работает».
                    let verified = self.state == "on" && self.ping.mechs.get("zapret").is_some_and(|z| z["ms"].is_u64());
                    if verified && self.flash.is_none() && self.reason.as_deref().is_none_or(|r| r.starts_with("процесс запущен") || r.starts_with("сайты")) {
                        ("Обход работает".to_owned(), "YouTube и Discord открываются".to_owned())
                    } else {
                        (def.title.to_owned(), sub)
                    }
                }
            }
        };

        // Слева: точка состояния под левой стенкой корпуса, заголовок и подпись.
        let left = rect.left() + 136.0;
        let sub_g = p.layout(sub, kit::regular(16.0), kit::INK_2, 440.0);
        let mut job = egui::text::LayoutJob::simple_singleline(title, kit::semibold(34.0), kit::INK);
        job.sections[0].format.extra_letter_spacing = -0.4;
        let title_g = p.layout_job(job);
        let sub_top = rect.bottom() - 32.0 - sub_g.size().y;
        p.galley(pos2(left + 28.0, sub_top), sub_g, kit::INK);
        let title_top = sub_top - 8.0 - title_g.size().y;
        p.galley(pos2(left + 28.0, title_top), title_g.clone(), kit::INK);
        let tone = match (&self.first, self.state.as_str()) {
            (Some(f), _) if f.failed.is_some() => Tone::Bad,
            (Some(f), _) if f.done => Tone::Ok,
            (Some(_), _) => Tone::Off,
            (None, "on") => Tone::Ok,
            (None, "timeout") => Tone::Warn,
            (None, "error" | "critical") => Tone::Bad,
            _ => Tone::Off,
        };
        kit::dot(&p, pos2(left + 7.0, title_top + title_g.size().y / 2.0), tone, 7.0);

        if self.first.is_some() {
            self.first_actions(ui, rect);
            return;
        }
        if self.boot.is_some() {
            return;
        }
        let label = match self.state.as_str() {
            "testing" => "Остановить подбор",
            "on" | "timeout" => "Выключить обход",
            "error" => "Попробовать снова",
            "critical" => "Исправить",
            _ => "Включить обход",
        };
        let interactive = self.interactive();
        let power = kit::Btn::primary(label).icon(if self.state == "testing" { Icon::Cross } else { Icon::Power }).big().enabled(interactive);
        let w = power.width(ui).max(240.0);
        let button = Rect::from_min_size(pos2(rect.right() - 24.0 - w, rect.bottom() - 32.0 - 52.0), vec2(w, 52.0));
        let busy = self.toggle_waiting || self.toggle_busy.is_some_and(|t| Instant::now() < t);
        let resp = power.show_at(ui, button, "power");
        let resp = if busy { resp.on_hover_cursor(egui::CursorIcon::Progress) } else { resp };

        // Слева от кнопки: время работы, а при проблеме — следующий шаг.
        // Если идёт мониторинг стабильности, его строка встаёт под ними, а
        // пара центрируется по кнопке; нажатие открывает его вкладку.
        let side = Rect::from_min_max(pos2(button.left() - 16.0 - 260.0, button.top()), pos2(button.left() - 16.0, button.bottom()));
        let stab = self.stab_home().map(|(text, tone)| (p.layout_no_wrap(text, kit::regular(13.0), kit::INK_2), tone));
        let next = match self.state.as_str() {
            // Кнопка только открывает вкладку; подбор запускается там.
            "timeout" => Some(("Подобрать стратегию", Icon::Gauge, DiagTab::Tests)),
            "testing" => Some(("Ход подбора", Icon::Gauge, DiagTab::Tests)),
            "error" => Some(("Открыть журнал", Icon::List, DiagTab::Log)),
            _ => None,
        };
        let since = self.on_since.filter(|_| self.state == "on");
        let uptime_g = since.map(|since| p.layout_no_wrap(format!("Время работы {}", uptime(since)), kit::regular(14.0), kit::INK_3));
        let top_h = match (&uptime_g, next) {
            (Some(g), _) => g.size().y,
            (None, Some(_)) => 40.0,
            _ => 0.0,
        };
        let gap = if top_h > 0.0 { 6.0 } else { 0.0 };
        let total = top_h + stab.as_ref().map_or(0.0, |(g, _)| gap + g.size().y);
        let top = side.center().y - total / 2.0;
        if let Some(g) = uptime_g {
            p.galley(pos2(side.right() - g.size().x, top), g, kit::INK);
        }
        if let Some(since) = since {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000 - (since.elapsed().as_millis() % 1000) as u64));
        }
        if let Some((text, icon, tab)) = next {
            let next = kit::Btn::secondary(text).icon(icon).enabled(interactive);
            let r = Rect::from_min_size(pos2(side.right() - next.width(ui), top), vec2(next.width(ui), 40.0));
            if next.show_at(ui, r, "next-step").clicked() {
                self.open_diag(tab);
            }
        }
        if let Some((g, tone)) = stab {
            // Цветная точка — только когда есть о чём предупредить.
            let dot = matches!(tone, Tone::Warn | Tone::Bad);
            let w = g.size().x + if dot { 12.0 } else { 0.0 };
            let r = Rect::from_min_size(pos2(side.right() - w, top + top_h + gap), vec2(w, g.size().y));
            let resp = ui.interact(r, ui.id().with("stab-home"), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if dot {
                kit::dot(&p, pos2(r.left() + 4.0, r.center().y), tone, 4.0);
            }
            let color = if resp.hovered() { kit::INK } else { kit::INK_2 };
            p.galley(pos2(r.right() - g.size().x, r.top()), g, color);
            if resp.clicked() {
                self.open_diag(DiagTab::Stability);
            }
        }

        let keys = ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space));
        let typing = ui.ctx().egui_wants_keyboard_input();
        if interactive && (resp.clicked() || (keys && !typing && !ui.ctx().memory(|m| m.focused().is_some()))) {
            self.power();
        }
    }
}

/// Отладочная настройка: переменная окружения или аргумент `--ИМЯ=значение`.
/// Аргумент нужен потому, что переменные не переходят через запрос UAC.
#[cfg(debug_assertions)]
fn dev(name: &str) -> Option<String> {
    let flag = format!("--{name}=");
    std::env::var(name).ok().or_else(|| std::env::args().find_map(|a| a.strip_prefix(&flag).map(str::to_owned)))
}

/// Только в отладочной сборке: `PLASTO_PAGE=services|diag|settings|first`
/// и `PLASTO_TAB=speed|stability|system|tests|log` открывают нужный раздел при запуске.
#[cfg(debug_assertions)]
fn debug_page(app: &mut App) {
    match dev("PLASTO_PAGE").as_deref() {
        Some("services") => app.go(Page::Services),
        Some("diag") => app.go(Page::Diag),
        Some("settings") => app.go(Page::Settings),
        Some("first") => app.begin_first(super::first::First { waiting: true, ..Default::default() }),
        Some("first-fail") => app.first = Some(super::first::First { failed: Some("GitHub не отвечает: возможно, он недоступен у вашего провайдера".into()), ..Default::default() }),
        Some("first-done") => app.first = Some(super::first::First { done: true, ..Default::default() }),
        _ => {}
    }
    match dev("PLASTO_TAB").as_deref() {
        Some("system") => app.diag_tab = DiagTab::System,
        Some("tests") => app.diag_tab = DiagTab::Tests,
        Some("log") => app.diag_tab = DiagTab::Log,
        Some("speed") => app.diag_tab = DiagTab::Speed,
        Some("stability") => app.diag_tab = DiagTab::Stability,
        _ => {}
    }
    match dev("PLASTO_MECH").as_deref() {
        Some("tg") => app.mech = Mech::Telegram,
        Some("ai") => app.mech = Mech::Ai,
        Some("dns") => app.mech = Mech::Dns,
        _ => {}
    }
}

/// Только в отладочной сборке: `PLASTO_SCREEN=error` и т. п. показывает
/// экран монитора в нужном состоянии, чтобы проверить сцену глазами.
#[cfg(debug_assertions)]
fn debug_screen() -> Option<Screen> {
    let v = dev("PLASTO_SCREEN")?;
    Some(match v.as_str() {
        "boot" => Screen::Boot(0.64),
        "fail" => Screen::Fail,
        other => Screen::from_state(other),
    })
}

/// Только в отладочной сборке (`PLASTO_KIT=1`): все компоненты на стекле,
/// чтобы сверить их с макетом глазами.
#[cfg(debug_assertions)]
fn kit_gallery(ui: &mut Ui, rect: Rect, gpu: bool) {
    let panel = Rect::from_min_max(pos2(rect.left() + 24.0, rect.top() + 76.0), pos2(rect.right() - 24.0, rect.bottom() - 24.0));
    glass::panel(ui, 2, panel, 16.0, gpu);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink2(vec2(28.0, 24.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing = vec2(12.0, 16.0);
    ui.horizontal(|ui| {
        kit::Btn::primary("Включить обход").icon(Icon::Power).big().show(ui);
        kit::Btn::primary("Подобрать лучшую").show(ui);
        kit::Btn::secondary("Проверить обновления").show(ui);
        kit::Btn::secondary("Недоступна").enabled(false).show(ui);
        kit::icon_button(ui, Icon::Refresh, "Обновить", false);
        kit::icon_button(ui, Icon::Refresh, "Обновляю", true);
    });
    ui.horizontal(|ui| {
        kit::switch(ui, true, true, "вкл");
        kit::switch(ui, false, true, "выкл");
        kit::switch(ui, true, false, "недоступен");
        kit::checkbox(ui, true, "ChatGPT", true);
        kit::checkbox(ui, false, "Copilot", true);
        kit::pill(ui, "включено", Tone::Ok);
        kit::pill(ui, "выключено", Tone::Off);
        kit::pill(ui, "доступна 1.9.4", Tone::Warn);
        kit::pill(ui, "запущена", Tone::Bad);
    });
    ui.horizontal(|ui| {
        kit::segmented(ui, "seg", &["Скорость", "Система", "Подбор стратегии", "Журнал"], 1);
        kit::filter(ui, "Всё", true);
        kit::filter(ui, "Обход", false);
        kit::filter(ui, "Ошибки", false);
    });
    ui.horizontal(|ui| {
        for icon in [Icon::Shield, Icon::Plane, Icon::Spark, Icon::Globe, Icon::Refresh, Icon::Power, Icon::ChevronRight, Icon::ChevronDown, Icon::Play, Icon::Stop, Icon::List, Icon::Gauge, Icon::Check] {
            let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            icons::paint(ui.painter(), r.center(), icon, 22.0, kit::INK_2);
        }
        kit::progress(ui, 0.62, 240.0);
    });
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(224.0, 192.0), Sense::hover());
        kit::gauge(ui.painter(), r, kit::gauge_frac(604.0), false);
        kit::gauge_ticks(ui.painter(), r);
        let (r, _) = ui.allocate_exact_size(vec2(224.0, 192.0), Sense::hover());
        kit::gauge(ui.painter(), r, kit::gauge_frac(131.0), true);
        ui.vertical(|ui| {
            let series = [120.0, 260.0, 420.0, 540.0, 590.0, 604.0, 598.0, 610.0, 602.0, 606.0];
            for (s, c) in [(&series[..], kit::OK), (&[60.0, 140.0, 230.0, 280.0, 300.0, 296.0, 301.0][..], kit::UP), (&[6.0, 5.0, 7.0, 6.0, 21.0, 24.0, 19.0][..], kit::PING), (&[][..], kit::OK)] {
                let (r, _) = ui.allocate_exact_size(vec2(160.0, 30.0), Sense::hover());
                kit::sparkline(ui.painter(), r, s, c);
            }
        });
    });
}
