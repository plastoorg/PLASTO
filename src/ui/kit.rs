//! Новый вид: токены из макета и компоненты на них.
//!
//! Цвета, размеры и состояния — из утверждённого макета (`index.html`):
//! 1 CSS-пиксель равен одной точке egui. У каждого элемента есть наведение,
//! нажатие (сдвиг на пиксель), недоступность (45%) и рамка фокуса с клавиатуры.


use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use super::{
    icons::{self, Icon},
    theme,
};

/* ---------- токены ------------------------------------------------------ */

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color32 {
    // Color32 хранит премультиплицированные значения.
    let k = (a * 255.0) as u32;
    Color32::from_rgba_premultiplied(((r as u32 * k) / 255) as u8, ((g as u32 * k) / 255) as u8, ((b as u32 * k) / 255) as u8, k as u8)
}

pub const INK: Color32 = Color32::from_rgb(0xF4, 0xF1, 0xEC);
pub const INK_STRONG: Color32 = Color32::from_rgb(0xFB, 0xF8, 0xF3);
pub const INK_2: Color32 = rgba(244, 241, 236, 0.76);
pub const INK_3: Color32 = rgba(244, 241, 236, 0.62);
pub const INK_4: Color32 = rgba(244, 241, 236, 0.48);
pub const FILL_1: Color32 = rgba(244, 241, 236, 0.06);
pub const FILL_2: Color32 = rgba(244, 241, 236, 0.10);
pub const FILL_3: Color32 = rgba(244, 241, 236, 0.16);
pub const HAIR: Color32 = rgba(244, 241, 236, 0.12);
pub const HAIR_2: Color32 = rgba(244, 241, 236, 0.22);
pub const WELL: Color32 = rgba(12, 10, 12, 0.32);
pub const TRACK: Color32 = rgba(244, 241, 236, 0.12);
pub const SHADOW: Color32 = rgba(12, 10, 12, 0.34);
pub const PAPER: Color32 = Color32::from_rgb(0xF7, 0xF3, 0xEC);
pub const PAPER_HOVER: Color32 = Color32::from_rgb(0xFF, 0xFC, 0xF6);
pub const ON_PAPER: Color32 = Color32::from_rgb(0x16, 0x13, 0x16);
pub const OK: Color32 = Color32::from_rgb(0x6F, 0xD3, 0x9A);
pub const OK_INK: Color32 = Color32::from_rgb(0x0C, 0x26, 0x17);
pub const OK_SOFT: Color32 = rgba(111, 211, 154, 0.16);
pub const OK_TEXT: Color32 = Color32::from_rgb(0x9B, 0xE6, 0xBA);
pub const WARN: Color32 = Color32::from_rgb(0xF2, 0xB6, 0x57);
pub const WARN_SOFT: Color32 = rgba(242, 182, 87, 0.16);
pub const WARN_TEXT: Color32 = Color32::from_rgb(0xF6, 0xCB, 0x86);
pub const BAD: Color32 = Color32::from_rgb(0xF0, 0x7A, 0x6A);
#[allow(dead_code)] // крестик ошибки в мастере первого запуска
pub const BAD_STRONG: Color32 = Color32::from_rgb(0xA9, 0x34, 0x21);
pub const BAD_SOFT: Color32 = rgba(240, 122, 106, 0.18);
pub const BAD_TEXT: Color32 = Color32::from_rgb(0xF7, 0xA6, 0x9A);
pub const IDLE: Color32 = Color32::from_rgb(0x9A, 0x96, 0x92);
pub const UP: Color32 = Color32::from_rgb(0x8E, 0xC9, 0xF0);
pub const PING: Color32 = Color32::from_rgb(0xC9, 0xB6, 0xF2);
pub const CLOSE: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
pub const FOCUS: Color32 = INK_STRONG;

pub fn regular(size: f32) -> FontId {
    theme::regular(size)
}

pub fn semibold(size: f32) -> FontId {
    theme::semibold(size)
}

/// Тон состояния: точки, плашки, цифры.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Ok,
    Off,
    Warn,
    Bad,
}

impl Tone {
    pub fn dot(self) -> Color32 {
        match self {
            Tone::Ok => OK,
            Tone::Off => IDLE,
            Tone::Warn => WARN,
            Tone::Bad => BAD,
        }
    }

    fn soft(self) -> (Color32, Color32) {
        match self {
            Tone::Ok => (OK_SOFT, OK_TEXT),
            Tone::Off => (FILL_2, INK_2),
            Tone::Warn => (WARN_SOFT, WARN_TEXT),
            Tone::Bad => (BAD_SOFT, BAD_TEXT),
        }
    }
}

/* ---------- общее ------------------------------------------------------- */

/// Взаимодействие с учётом доступности: недоступный элемент только наводится.
fn interact(ui: &mut Ui, rect: Rect, id: egui::Id, enabled: bool) -> Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp.on_hover_cursor(CursorIcon::NotAllowed)
    }
}

/// Нажатый элемент сдвигается на пиксель вниз, как `:active` в макете.
fn pressed(resp: &Response, rect: Rect) -> Rect {
    if resp.is_pointer_button_down_on() {
        rect.translate(vec2(0.0, 1.0))
    } else {
        rect
    }
}

/// Рамка фокуса с клавиатуры: 2 точки, с отступом, сразу, без анимации.
pub fn focus_ring(p: &Painter, resp: &Response, rect: Rect, radius: f32) {
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0_f32, FOCUS), StrokeKind::Outside);
    }
}

fn fade(c: Color32, enabled: bool) -> Color32 {
    if enabled {
        c
    } else {
        c.gamma_multiply(0.45)
    }
}

/// Текст одной строкой.
#[allow(dead_code)] // для страниц
pub fn text(ui: &mut Ui, s: &str, font: FontId, color: Color32) -> Response {
    let g = ui.painter().layout_no_wrap(s.to_owned(), font, color);
    let (rect, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(rect.min, g, color);
    resp
}

/// Текст с переносом по ширине `width`.
#[allow(dead_code)] // для страниц
pub fn paragraph(ui: &mut Ui, s: &str, font: FontId, color: Color32, width: f32) -> Response {
    let g = ui.painter().layout(s.to_owned(), font, color, width);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
    ui.painter().galley(rect.min, g, color);
    resp
}

/* ---------- кнопки ------------------------------------------------------ */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Светлая, одна на экран: главное действие.
    Primary,
    /// Обычная: полупрозрачная с тонкой кромкой.
    Secondary,
}

/// Кнопка: подпись, необязательный значок, вид, размер и доступность.
/// Обычная — высотой 40, как `.btn`; `big` — главная, 52 (кнопка обхода).
pub struct Btn<'a> {
    label: &'a str,
    icon: Option<Icon>,
    kind: Kind,
    big: bool,
    enabled: bool,
}

impl<'a> Btn<'a> {
    pub fn primary(label: &'a str) -> Self {
        Btn { label, icon: None, kind: Kind::Primary, big: false, enabled: true }
    }

    pub fn secondary(label: &'a str) -> Self {
        Btn { label, icon: None, kind: Kind::Secondary, big: false, enabled: true }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn big(mut self) -> Self {
        self.big = true;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn font(&self) -> FontId {
        if self.big {
            semibold(17.0)
        } else if self.kind == Kind::Primary {
            semibold(15.0)
        } else {
            regular(15.0)
        }
    }

    /// Ширина по содержимому.
    pub fn width(&self, ui: &Ui) -> f32 {
        let (icon, pad) = if self.big { (22.0 + 12.0, 28.0) } else { (18.0 + 8.0, 16.0) };
        measure(ui, self.label, self.font()).x + if self.icon.is_some() { icon } else { 0.0 } + pad * 2.0
    }

    /// Кнопка в потоке раскладки.
    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, _) = ui.allocate_exact_size(vec2(self.width(ui), if self.big { 52.0 } else { 40.0 }), Sense::hover());
        let label = self.label;
        self.show_at(ui, rect, label)
    }

    /// Кнопка в заданном прямоугольнике (по правому краю панели, по ширине колонки).
    pub fn show_at(self, ui: &mut Ui, rect: Rect, id: impl std::hash::Hash) -> Response {
        let resp = interact(ui, rect, ui.id().with(("button", id)), self.enabled);
        let color = fade(if self.kind == Kind::Primary { ON_PAPER } else { INK }, self.enabled);
        let g = ui.painter().layout_no_wrap(self.label.to_owned(), self.font(), color);
        let p = ui.painter();
        let r = pressed(&resp, rect);
        let radius = if self.big { r.height() / 2.0 } else { 12.0 };
        let hover = resp.hovered() && self.enabled;
        match self.kind {
            Kind::Primary => {
                if self.big {
                    p.add(egui::Shadow { offset: [0, 8], blur: 20, spread: 0, color: SHADOW }.as_shape(r, radius as u8));
                }
                p.rect_filled(r, radius, fade(if hover { PAPER_HOVER } else { PAPER }, self.enabled));
            }
            Kind::Secondary => {
                p.rect_filled(r, radius, if hover { FILL_3 } else { FILL_2 });
                p.rect_stroke(r, radius, Stroke::new(1.0_f32, HAIR), StrokeKind::Inside);
            }
        }
        let (icon_size, gap) = if self.big { (22.0, 12.0) } else { (18.0, 8.0) };
        let content = g.size().x + self.icon.map_or(0.0, |_| icon_size + gap);
        let mut x = r.center().x - content / 2.0;
        if let Some(i) = self.icon {
            icons::paint(p, pos2(x + icon_size / 2.0, r.center().y), i, icon_size, color);
            x += icon_size + gap;
        }
        p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, color);
        focus_ring(p, &resp, r, radius);
        resp
    }
}

/// Кнопка-значок 32×32 (обновить пинг): зона нажатия шире на 6 точек.
/// `busy` — значок крутится, нажать нельзя.
pub fn icon_button(ui: &mut Ui, icon: Icon, hint: &str, busy: bool) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
    let resp = ui.interact(rect.expand(6.0), ui.id().with(("icon", hint)), if busy { Sense::hover() } else { Sense::click() }).on_hover_text(hint);
    let resp = if busy { resp.on_hover_cursor(CursorIcon::Progress) } else { resp.on_hover_cursor(CursorIcon::PointingHand) };
    let p = ui.painter();
    if resp.hovered() && !busy {
        p.rect_filled(rect, 8.0, FILL_2);
    }
    let color = if resp.hovered() { INK } else { INK_2 };
    if busy {
        // Вращение: тот же значок, повёрнутый по времени (0,9 с на оборот).
        let t = ui.ctx().input(|i| i.time) as f32;
        let angle = (t / 0.9).fract() * std::f32::consts::TAU;
        icons::paint_turned(p, rect.center(), icon, 18.0, color, angle);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    } else {
        icons::paint(p, rect.center(), icon, 18.0, color);
    }
    focus_ring(p, &resp, rect, 8.0);
    resp
}

/* ---------- тумблер, флажок, плашка, точка ------------------------------ */

/// Тумблер 48×28: зелёная дорожка, белая ручка едет 0,24 с.
#[cfg_attr(not(debug_assertions), allow(dead_code))] // в потоке раскладки — пока только витрина
pub fn switch(ui: &mut Ui, on: bool, enabled: bool, hint: &str) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(48.0, 28.0), Sense::hover());
    switch_at(ui, rect, on, enabled, hint)
}

pub fn switch_at(ui: &mut Ui, rect: Rect, on: bool, enabled: bool, hint: &str) -> Response {
    let resp = interact(ui, rect, ui.id().with(("switch", hint)), enabled).on_hover_text(hint);
    let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.24);
    let p = ui.painter();
    let track = if t > 0.5 { OK } else { WELL };
    p.rect_filled(rect, 14.0, fade(track.lerp_to_gamma(OK, t), enabled));
    p.rect_stroke(rect, 14.0, Stroke::new(1.0_f32, if on { OK } else { HAIR_2 }), StrokeKind::Inside);
    let knob = pos2(rect.left() + 2.0 + 11.0 + 20.0 * t, rect.center().y);
    p.circle_filled(knob + vec2(0.0, 1.5), 11.0, SHADOW);
    p.circle_filled(knob, 11.0, fade(PAPER, enabled));
    focus_ring(p, &resp, rect, 14.0);
    resp
}

/// Спиннер на ручке заблокированного тумблера: переключение идёт, а не зависло.
pub fn switch_spinner(ui: &Ui, rect: Rect, on: bool) {
    let knob = pos2(rect.left() + 2.0 + 11.0 + if on { 20.0 } else { 0.0 }, rect.center().y);
    egui::Spinner::new().size(14.0).color(INK_2).paint_at(ui, Rect::from_center_size(knob, vec2(14.0, 14.0)));
}

/// Флажок 20×20 с подписью справа; вся строка нажимается.
pub fn checkbox(ui: &mut Ui, on: bool, label: &str, enabled: bool) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), regular(15.0), INK);
    let (rect, _) = ui.allocate_exact_size(vec2(20.0 + 12.0 + g.size().x, 24.0f32.max(g.size().y)), Sense::hover());
    let resp = interact(ui, rect, ui.id().with(("check", label)), enabled);
    let p = ui.painter();
    let bx = Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(20.0, 20.0));
    if on {
        p.rect_filled(bx, 4.0, fade(OK, enabled));
        icons::paint(p, bx.center(), Icon::Check, 14.0, OK_INK);
    } else {
        p.rect_stroke(bx, 4.0, Stroke::new(2.0_f32, HAIR_2), StrokeKind::Inside);
    }
    p.galley(pos2(bx.right() + 12.0, rect.center().y - g.size().y / 2.0), g, fade(INK, enabled));
    focus_ring(p, &resp, bx, 4.0);
    resp
}

/// Плашка состояния: «включено», «нет ответа», «доступна 1.9.4».
pub fn pill(ui: &mut Ui, label: &str, tone: Tone) -> Response {
    let (bg, fg) = tone.soft();
    let g = ui.painter().layout_no_wrap(label.to_owned(), regular(13.0), fg);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(24.0, 8.0), Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, bg);
    ui.painter().galley(rect.min + vec2(12.0, 4.0), g, fg);
    resp
}

pub fn dot(p: &Painter, c: Pos2, tone: Tone, radius: f32) {
    p.circle_filled(c, radius, tone.dot());
}

/* ---------- переключатели ----------------------------------------------- */

/// Сегментный переключатель: подложка-колодец, выбранный сегмент светлее.
/// Возвращает номер нажатого сегмента.
pub fn segmented(ui: &mut Ui, id: &str, options: &[&str], selected: usize) -> Option<usize> {
    let gs: Vec<_> = options.iter().map(|o| ui.painter().layout_no_wrap((*o).to_owned(), regular(14.0), INK)).collect();
    let widths: Vec<f32> = gs.iter().map(|g| g.size().x + 32.0).collect();
    let total = widths.iter().sum::<f32>() + 4.0 * (options.len() as f32 - 1.0) + 8.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 40.0), Sense::hover());
    ui.painter().rect_filled(rect, 12.0, WELL);
    let mut x = rect.left() + 4.0;
    let mut clicked = None;
    for (i, (g, w)) in gs.into_iter().zip(widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(w, 32.0));
        let resp = interact(ui, r, ui.id().with((id, i)), true);
        let on = i == selected;
        let p = ui.painter();
        if on {
            p.rect_filled(r, 8.0, FILL_3);
        }
        let color = if on { INK_STRONG } else if resp.hovered() { INK } else { INK_2 };
        let g = if on { p.layout_no_wrap(options[i].to_owned(), semibold(14.0), color) } else { g };
        p.galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, color);
        focus_ring(p, &resp, r, 8.0);
        if resp.clicked() && !on {
            clicked = Some(i);
        }
        x = r.right() + 4.0;
    }
    clicked
}

/// Фильтр-«таблетка» для журнала: выбранный подсвечен, без белой кнопки.
pub fn filter(ui: &mut Ui, label: &str, on: bool) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), regular(14.0), INK);
    let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 24.0, 32.0), Sense::hover());
    let resp = interact(ui, rect, ui.id().with(("filter", label)), true);
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 16.0, FILL_3);
    } else {
        if resp.hovered() {
            p.rect_filled(rect, 16.0, FILL_1);
        }
        p.rect_stroke(rect, 16.0, Stroke::new(1.0_f32, HAIR), StrokeKind::Inside);
    }
    let color = if on { INK_STRONG } else if resp.hovered() { INK } else { INK_2 };
    p.galley(pos2(rect.center().x - g.size().x / 2.0, rect.center().y - g.size().y / 2.0), g, color);
    focus_ring(p, &resp, rect, 16.0);
    resp
}

/* ---------- прогресс, шкала, графики ------------------------------------ */

/// Полоса прогресса высотой 6.
pub fn progress(ui: &mut Ui, frac: f32, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, TRACK);
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.0 {
        p.rect_filled(Rect::from_min_size(rect.min, vec2(w, rect.height())), 3.0, OK);
    }
    resp
}

/// Полоска прогресса в заданном прямоугольнике (внутри строки, без раскладки).
pub fn paint_bar(p: &Painter, rect: Rect, frac: f32) {
    p.rect_filled(rect, rect.height() / 2.0, TRACK);
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.0 {
        p.rect_filled(Rect::from_min_size(rect.min, vec2(w, rect.height())), rect.height() / 2.0, OK);
    }
}

/// Доля шкалы для скорости: логарифм, 0–1000 Мбит/с.
pub fn gauge_frac(mbps: f64) -> f32 {
    ((1.0 + mbps.max(0.0)).log10() / 1001f64.log10()).min(1.0) as f32
}

/// Шкала скорости: дуга 270°, заполненная часть — градиентом от тёмного к
/// светлому вдоль диагонали (чем выше скорость, тем светлее), как в макете.
/// `rect` — квадрат 280×240 в масштабе макета.
pub fn gauge(p: &Painter, rect: Rect, frac: f32, upload: bool) {
    let k = rect.width() / 280.0;
    let c = rect.min + vec2(140.0, 132.0) * k;
    let r = 108.0 * k;
    let w = 14.0 * k;
    let point = |f: f32, rad: f32| {
        let a = (135.0 + 270.0 * f).to_radians();
        c + vec2(a.cos(), a.sin()) * rad
    };
    // Дорожка.
    let n = 96;
    let track: Vec<Pos2> = (0..=n).map(|i| point(i as f32 / n as f32, r)).collect();
    p.add(Shape::line(track, Stroke::new(w, TRACK)));
    p.circle_filled(point(0.0, r), w / 2.0, TRACK);
    p.circle_filled(point(1.0, r), w / 2.0, TRACK);
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    // Градиент вдоль линии из (30,225) в (250,35), три ступени — у загрузки.
    let (a, b) = (rect.min + vec2(30.0, 225.0) * k, rect.min + vec2(250.0, 35.0) * k);
    let stops: &[(f32, Color32)] = if upload {
        &[(0.0, Color32::from_rgb(0x4E, 0x86, 0xCB)), (1.0, Color32::from_rgb(0xB9, 0xED, 0xFF))]
    } else {
        &[(0.0, Color32::from_rgb(0x37, 0xBF, 0x8C)), (0.55, Color32::from_rgb(0x79, 0xE8, 0xA4)), (1.0, Color32::from_rgb(0xCC, 0xFF, 0xE5))]
    };
    let color_at = |q: Pos2| {
        let d = b - a;
        let t = ((q - a).dot(d) / d.length_sq()).clamp(0.0, 1.0);
        let i = stops.iter().position(|(s, _)| *s >= t).unwrap_or(stops.len() - 1).max(1);
        let (s0, c0) = stops[i - 1];
        let (s1, c1) = stops[i];
        c0.lerp_to_gamma(c1, ((t - s0) / (s1 - s0).max(1e-6)).clamp(0.0, 1.0))
    };
    let steps = ((n as f32) * frac).ceil().max(2.0) as usize;
    let mut mesh = Mesh::default();
    for i in 0..=steps {
        let f = frac * i as f32 / steps as f32;
        let (inner, outer) = (point(f, r - w / 2.0), point(f, r + w / 2.0));
        let col = color_at(point(f, r));
        mesh.colored_vertex(inner, col);
        mesh.colored_vertex(outer, col);
        if i > 0 {
            let j = (i as u32) * 2;
            mesh.add_triangle(j - 2, j - 1, j);
            mesh.add_triangle(j - 1, j + 1, j);
        }
    }
    p.add(mesh);
    p.circle_filled(point(0.0, r), w / 2.0, color_at(point(0.0, r)));
    p.circle_filled(point(frac, r), w / 2.0, color_at(point(frac, r)));
}

/// Подписи шкалы: 0, 10, 100, 1000 — у внутреннего края дуги.
pub fn gauge_ticks(p: &Painter, rect: Rect) {
    let k = rect.width() / 280.0;
    let c = rect.min + vec2(140.0, 132.0) * k;
    for v in [0.0, 10.0, 100.0, 1000.0] {
        let a = (135.0 + 270.0 * gauge_frac(v)).to_radians();
        let q = c + vec2(a.cos(), a.sin()) * (108.0 - 30.0) * k;
        p.text(q, Align2::CENTER_CENTER, format!("{v}"), regular(12.0 * k), INK_4);
    }
}

/// Маленький график: площадь с градиентом сверху вниз и линия, набирающая
/// яркость слева направо. Пустой ряд — пунктир.
pub fn sparkline(p: &Painter, rect: Rect, series: &[f64], color: Color32) {
    if series.len() < 2 {
        let y = rect.bottom() - 1.0;
        let mut x = rect.left();
        while x < rect.right() {
            p.line_segment([pos2(x, y), pos2((x + 3.0).min(rect.right()), y)], Stroke::new(1.0_f32, color.gamma_multiply(0.35)));
            x += 6.0;
        }
        return;
    }
    let max = series.iter().cloned().fold(0.0, f64::max).max(1e-9) * 1.15;
    let pts: Vec<Pos2> = series
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(rect.left() + rect.width() * i as f32 / (series.len() - 1) as f32, rect.bottom() - 2.0 - (*v / max) as f32 * (rect.height() - 4.0)))
        .collect();
    let mut mesh = Mesh::default();
    for (i, q) in pts.iter().enumerate() {
        let top = color.gamma_multiply(0.35 * (1.0 - (q.y - rect.top()) / rect.height()));
        mesh.colored_vertex(*q, top);
        mesh.colored_vertex(pos2(q.x, rect.bottom()), Color32::TRANSPARENT);
        if i > 0 {
            let j = i as u32 * 2;
            mesh.add_triangle(j - 2, j - 1, j);
            mesh.add_triangle(j - 1, j + 1, j);
        }
    }
    p.add(mesh);
    for (i, w) in pts.windows(2).enumerate() {
        let t = i as f32 / (pts.len() - 1) as f32;
        let alpha = (0.45 + 0.55 * (t / 0.55)).min(1.0);
        p.line_segment([w[0], w[1]], Stroke::new(1.6_f32, color.gamma_multiply(alpha)));
    }
}

/// Размер текста без рисования — для раскладки по правому краю.
pub fn measure(ui: &Ui, s: &str, font: FontId) -> Vec2 {
    ui.painter().layout_no_wrap(s.to_owned(), font, INK).size()
}

/* ---------- стиль стандартных виджетов ---------------------------------- */

/// Непрозрачная поверхность для всплывающих списков и шапки таблиц.
pub const SURFACE_SOLID: Color32 = Color32::from_rgb(0x24, 0x1F, 0x22);

/// Тёмная тема для того, что рисует сам egui: поля ввода, прокрутка,
/// всплывающие списки, подсказки.
pub fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut v = egui::Visuals::dark();
        let r = egui::CornerRadius::same(12);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = r;
            w.bg_stroke = Stroke::new(1.0_f32, HAIR);
            w.fg_stroke = Stroke::new(1.0_f32, INK);
            w.expansion = 0.0;
        }
        v.widgets.inactive.weak_bg_fill = FILL_2;
        v.widgets.inactive.bg_fill = FILL_2;
        v.widgets.hovered.weak_bg_fill = FILL_3;
        v.widgets.hovered.bg_fill = FILL_3;
        v.widgets.active.weak_bg_fill = FILL_3;
        v.widgets.active.bg_fill = FILL_3;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, HAIR_2);
        v.selection.bg_fill = OK.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0_f32, INK_STRONG);
        v.extreme_bg_color = Color32::from_rgba_unmultiplied(12, 10, 12, 140);
        v.text_edit_bg_color = Some(Color32::from_rgba_unmultiplied(12, 10, 12, 140));
        v.window_fill = SURFACE_SOLID;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_stroke = Stroke::new(1.0_f32, HAIR);
        v.window_corner_radius = r;
        v.menu_corner_radius = r;
        v.popup_shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: SHADOW };
        v.override_text_color = Some(INK);
        v.text_cursor.stroke = Stroke::new(2.0_f32, INK_STRONG);
        style.visuals = v;
        style.spacing.item_spacing = vec2(12.0, 12.0);
        style.spacing.button_padding = vec2(12.0, 8.0);
        style.spacing.interact_size.y = 40.0;
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        style.spacing.scroll.bar_width = 6.0;
        style.interaction.selectable_labels = false;
        style.text_styles.insert(egui::TextStyle::Body, regular(15.0));
        style.text_styles.insert(egui::TextStyle::Button, regular(15.0));
        style.text_styles.insert(egui::TextStyle::Monospace, regular(13.0));
        style.text_styles.insert(egui::TextStyle::Small, regular(13.0));
    });
}

/* ---------- поля, списки, подписи --------------------------------------- */

/// Подпись над полем: 13, приглушённая, обычный регистр (без разрядки).
pub fn label(ui: &mut Ui, s: &str) {
    let g = ui.painter().layout_no_wrap(s.to_owned(), semibold(13.0), INK_3);
    let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(rect.min, g, INK_3);
}

/// Пояснение под полем: 13, приглушённое, с переносом по ширине колонки.
pub fn hint(ui: &mut Ui, s: &str) {
    let width = ui.available_width();
    let g = ui.painter().layout(s.to_owned(), regular(13.0), INK_3, width);
    let (rect, _) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
    ui.painter().galley(rect.min, g, INK_3);
}

/// Пункт выпадающего списка: заголовок группы или значение.
pub enum Pick<'a> {
    Group(&'a str),
    Item { value: &'a str, label: &'a str },
}

/// Выпадающий список: поле 40 с шевроном, список — тёмная панель под ним.
/// Возвращает выбранное значение.
pub fn select(ui: &mut Ui, id: &str, width: f32, current: &str, items: &[Pick]) -> Option<String> {
    let shown = items
        .iter()
        .find_map(|p| match p {
            Pick::Item { value, label } if *value == current => Some(*label),
            _ => None,
        })
        .unwrap_or("—");
    let (rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    let resp = interact(ui, rect, ui.id().with(("select", id)), true);
    let p = ui.painter();
    p.rect_filled(rect, 12.0, if resp.hovered() { FILL_2 } else { WELL });
    p.rect_stroke(rect, 12.0, Stroke::new(1.0_f32, if resp.hovered() { HAIR_2 } else { HAIR }), StrokeKind::Inside);
    let text = super::theme::elided(ui, shown, regular(15.0), INK, rect.width() - 12.0 - 40.0);
    p.galley(pos2(rect.left() + 12.0, rect.center().y - text.size().y / 2.0), text, INK);
    icons::paint(p, pos2(rect.right() - 20.0, rect.center().y), Icon::ChevronDown, 16.0, INK_2);
    focus_ring(p, &resp, rect, 12.0);
    let mut picked = None;
    egui::Popup::from_toggle_button_response(&resp)
        .width(width)
        .gap(4.0)
        .frame(egui::Frame::new().fill(SURFACE_SOLID).stroke(Stroke::new(1.0_f32, HAIR)).corner_radius(12).inner_margin(6).shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: SHADOW }))
        .show(|ui| {
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                for item in items {
                    match item {
                        Pick::Group(t) => {
                            ui.add_space(6.0);
                            let g = ui.painter().layout_no_wrap((*t).to_owned(), semibold(13.0), INK_4);
                            let (r, _) = ui.allocate_exact_size(vec2(width - 12.0, g.size().y + 6.0), Sense::hover());
                            ui.painter().galley(pos2(r.left() + 10.0, r.top()), g, INK_4);
                        }
                        Pick::Item { value, label } => {
                            let (r, _) = ui.allocate_exact_size(vec2(width - 12.0, 32.0), Sense::hover());
                            let row = ui.interact(r, ui.id().with(("pick", *value)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                            let on = *value == current;
                            if row.hovered() || on {
                                ui.painter().rect_filled(r, 8.0, if on { FILL_3 } else { FILL_2 });
                            }
                            let g = super::theme::elided(ui, label, regular(15.0), INK, r.width() - 20.0);
                            ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, if on { INK_STRONG } else { INK });
                            if row.clicked() {
                                picked = Some((*value).to_owned());
                                ui.close();
                            }
                        }
                    }
                }
            });
        });
    picked.filter(|v| v != current)
}

/// Поле ввода в тёмном колодце: одна строка или несколько (`rows` > 1).
pub fn input(ui: &mut Ui, text: &mut String, placeholder: &str, width: f32, rows: usize) -> Response {
    let edit = if rows > 1 { egui::TextEdit::multiline(text).desired_rows(rows) } else { egui::TextEdit::singleline(text) };
    ui.add(edit.hint_text(egui::RichText::new(placeholder).color(INK_4)).font(regular(15.0)).margin(vec2(12.0, 10.0)).desired_width(width))
}

/// Раскрывающийся блок «Для опытных»: строка со стрелкой, состояние помнит egui.
pub fn disclosure(ui: &mut Ui, id: &str, title: &str) -> bool {
    let key = ui.id().with(("disclosure", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    let resp = interact(ui, rect, key.with("btn"), true);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 8.0, FILL_1);
    }
    let t = ui.ctx().animate_bool_with_time(key.with("t"), open, 0.24);
    icons::paint_turned(p, pos2(rect.left() + 16.0, rect.center().y), Icon::ChevronDown, 16.0, INK_3, -std::f32::consts::FRAC_PI_2 * (1.0 - t));
    let g = p.layout_no_wrap(title.to_owned(), semibold(14.0), INK_2);
    p.galley(pos2(rect.left() + 32.0, rect.center().y - g.size().y / 2.0), g, INK_2);
    focus_ring(p, &resp, rect, 8.0);
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(key, open));
    }
    open
}

/// Строка «название — значение» с линией сверху, как `.kv` в макете.
pub fn kv(ui: &mut Ui, key: &str, value: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    ui.painter().line_segment([pos2(left, top), pos2(left + width, top)], Stroke::new(1.0_f32, HAIR));
    ui.allocate_ui_with_layout(vec2(width, 48.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(48.0);
        let g = ui.painter().layout_no_wrap(key.to_owned(), regular(15.0), INK_2);
        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, INK_2);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), value);
    });
}

/// Тонкая линия-разделитель на всю ширину.
pub fn hairline(ui: &mut Ui) {
    let width = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0_f32, HAIR));
}
