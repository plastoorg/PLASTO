//! Разделы нового окна: переходы между ними и то, что у страниц общее —
//! стеклянная панель с прокруткой, строка механизма обхода, текст состояния.

use egui::{pos2, vec2, Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder};
use serde_json::{json, Value};

use super::{
    glass,
    icons::Icon,
    kit::{self, Tone},
    theme::{self, Status},
    App, DiagTab, Mech, Op, Page,
};

/// Область страниц: под шапкой, с полями 24.
pub fn content_rect(window: Rect) -> Rect {
    Rect::from_min_max(pos2(window.left() + 24.0, window.top() + 76.0), pos2(window.right() - 24.0, window.bottom() - 24.0))
}

/// Стеклянная панель с содержимым и прокруткой, если оно не помещается.
pub fn glass_box(ui: &mut Ui, id: u64, rect: Rect, gpu: bool, pad: egui::Vec2, add: impl FnOnce(&mut Ui)) {
    glass::panel(ui, id, rect, 16.0, gpu);
    let inner = rect.shrink2(pad);
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)).id_salt(("glass-box", id)));
    ScrollArea::vertical().id_salt(("glass-scroll", id)).auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_width(inner.width() - 10.0);
        add(ui);
    });
}

/// Заголовок страницы или подробностей: 26, полужирный.
pub fn title(ui: &mut Ui, text: &str, sub: &str) {
    let g = ui.painter().layout_no_wrap(text.to_owned(), kit::semibold(26.0), kit::INK);
    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, kit::INK);
    if !sub.is_empty() {
        ui.add_space(-6.0);
        let width = ui.available_width();
        let g = ui.painter().layout(sub.to_owned(), kit::regular(15.0), kit::INK_2, width);
        let (r, _) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
        ui.painter().galley(r.min, g, kit::INK_2);
    }
}

/// Текст состояния из бэкенда: `**` в нём — выделение, здесь просто снимается.
pub fn status_text(status: &Status) -> (String, Tone) {
    let tone = match status.tone {
        theme::Tone::Ok => Tone::Ok,
        theme::Tone::Bad => Tone::Bad,
        theme::Tone::Warn => Tone::Warn,
        _ => Tone::Off,
    };
    (status.text.replace("**", "").replace('`', ""), tone)
}

/// Строка состояния: точка тона и текст с переносом.
pub fn status_line(ui: &mut Ui, status: &Status) {
    let (text, tone) = status_text(status);
    let width = ui.available_width();
    let color = match tone {
        Tone::Ok => kit::OK_TEXT,
        Tone::Bad => kit::BAD_TEXT,
        Tone::Warn => kit::WARN_TEXT,
        Tone::Off => kit::INK_2,
    };
    // Отступ под точку — только когда она есть.
    let indent = if tone == Tone::Off { 0.0 } else { 20.0 };
    let g = ui.painter().layout(text, kit::regular(14.0), color, width - indent);
    let (r, _) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
    if tone != Tone::Off {
        kit::dot(ui.painter(), pos2(r.left() + 4.0, r.top() + 9.0), tone, 4.0);
    }
    ui.painter().galley(pos2(r.left() + indent, r.top()), g, color);
}

/// Моноширинная плашка в колодце: путь, команда, адрес.
pub fn plate(ui: &mut Ui, text: &str, wrap: bool) {
    let width = ui.available_width();
    let inner = width - 24.0;
    let g = if wrap { ui.painter().layout(text.to_owned(), kit::regular(13.0), kit::INK_2, inner) } else { theme::elided(ui, text, kit::regular(13.0), kit::INK_2, inner) };
    let (r, _) = ui.allocate_exact_size(vec2(width, g.size().y + 20.0), Sense::hover());
    ui.painter().rect_filled(r, 12.0, kit::WELL);
    ui.painter().rect_stroke(r, 12.0, egui::Stroke::new(1.0_f32, kit::HAIR), egui::StrokeKind::Inside);
    ui.painter().galley(pos2(r.left() + 12.0, r.top() + 10.0), g, kit::INK_2);
}

/// Что показать о механизме обхода в строке: значок, имя, подпись, состояние.
pub struct MechInfo {
    pub icon: Icon,
    pub name: &'static str,
    pub sub: String,
    pub status: &'static str,
    pub tone: Tone,
    pub on: bool,
    /// Тумблер ждёт: `status` и `sub` тогда говорят, что именно происходит.
    pub busy: bool,
}

impl App {
    pub(super) fn mech_info(&self, mech: Mech) -> MechInfo {
        let on_off = |on: bool| if on { ("включено", Tone::Ok) } else { ("выключено", Tone::Off) };
        match mech {
            Mech::Zapret => {
                let label = self.cfg_str("strategy").map(|f| self.label_of(f)).unwrap_or_default();
                let on = self.state == "on" || self.state == "timeout";
                let (status, tone) = match self.state.as_str() {
                    _ if self.toggle_waiting => (if on { "выключаю…" } else { "запускаю…" }, Tone::Off),
                    "on" => ("работает", Tone::Ok),
                    "timeout" => ("нет ответа", Tone::Warn),
                    "testing" => ("подбор", Tone::Warn),
                    "error" | "critical" => ("ошибка", Tone::Bad),
                    _ => ("выключено", Tone::Off),
                };
                let sub = match () {
                    _ if self.toggle_waiting && on => "останавливаю обход".into(),
                    _ if self.toggle_waiting => "запускаю обход и проверяю сайты".into(),
                    _ if label.is_empty() => "стратегия не выбрана".into(),
                    _ => format!("стратегия {label}"),
                };
                MechInfo { icon: Icon::Shield, name: "YouTube и Discord", sub, status, tone, on, busy: self.toggle_waiting || self.boot.is_some() || self.tests.running }
            }
            Mech::Telegram => {
                let on = self.tgws["running"] == true || self.tgws["external"] == true;
                let (status, tone, sub) = match (self.tgws_busy, on) {
                    (true, true) => ("выключаю…", Tone::Off, "останавливаю прокси"),
                    (true, false) => ("запускаю…", Tone::Off, "запускаю прокси"),
                    (false, true) => ("работает", Tone::Ok, "локальный прокси"),
                    (false, false) => ("выключено", Tone::Off, "локальный прокси"),
                };
                MechInfo { icon: Icon::Plane, name: "Telegram", sub: sub.into(), status, tone, on, busy: self.tgws_busy }
            }
            Mech::Ai => {
                // Список обхода работает вместе с zapret, прокси — пока блок PLASTO в hosts.
                let zapret = self.state == "on" || self.state == "timeout";
                let on = zapret && !self.ai.bypass.is_empty() || self.ai.hosts_live;
                let (status, tone, sub) = match () {
                    _ if self.ai.busy_apply => ("применяю…", Tone::Off, "применяю настройки"),
                    _ if self.tests.running => ("ждёт подбора", Tone::Off, "идёт подбор стратегии"),
                    _ => (on_off(on).0, on_off(on).1, "ChatGPT, Claude, Gemini"),
                };
                MechInfo { icon: Icon::Spark, name: "ИИ-сервисы", sub: sub.into(), status, tone, on, busy: self.ai.busy_apply || self.tests.running }
            }
            Mech::Dns => {
                let on = self.dns.has_backup;
                let broken = self.dns.active.as_ref().and_then(|id| self.dns.health.get(id)).is_some_and(|h| matches!(h["state"].as_str(), Some("down" | "noSpoof")));
                let (status, tone) = if on && broken { ("не отвечает", Tone::Warn) } else { on_off(on) };
                let name = self.dns.presets.iter().find(|p| p["id"] == self.dns.selected.as_str()).and_then(|p| p["name"].as_str()).unwrap_or("защищённый DNS");
                let (status, tone, sub) = match self.dns.doing {
                    Some((status, sub)) if self.dns.busy_apply => (status, Tone::Off, sub.to_owned()),
                    _ => (status, tone, name.to_owned()),
                };
                MechInfo { icon: Icon::Globe, name: "DNS", sub, status, tone, on, busy: self.dns.busy_apply }
            }
        }
    }

    /// Тумблер механизма: zapret, Telegram и DNS переключаются сразу (DNS
    /// включает выбранный пресет, выключается возвратом как было); ИИ
    /// включается со своими настройками — их открывает страница «Сервисы».
    pub(super) fn mech_switch(&mut self, mech: Mech) {
        match mech {
            Mech::Zapret => self.toggle(),
            Mech::Telegram => {
                self.tgws_busy = true;
                self.call(Op::TgwsToggle, "tgws:toggle", Value::Null);
            }
            Mech::Dns if self.dns.has_backup => self.dns_restore(),
            Mech::Dns if !self.dns.selected.is_empty() => self.dns_apply(("включаю…", "выставляю адреса и проверяю")),
            Mech::Ai | Mech::Dns => self.open_mech(mech),
        }
    }

    /// Выставляет выбранный пресет; `doing` — что показать, пока он применяется.
    pub(super) fn dns_apply(&mut self, doing: (&'static str, &'static str)) {
        self.dns.busy_apply = true;
        self.dns.doing = Some(doing);
        self.call(Op::DnsApply { auto: false }, "dns:apply", json!(self.dns.selected));
    }

    pub(super) fn dns_restore(&mut self) {
        self.dns.busy_apply = true;
        self.dns.doing = Some(("выключаю…", "возвращаю прежние настройки"));
        self.call(Op::DnsRestore, "dns:restore", Value::Null);
    }

    pub(super) fn open_mech(&mut self, mech: Mech) {
        self.mech = mech;
        self.go(Page::Services);
    }

    pub(super) fn open_diag(&mut self, tab: DiagTab) {
        self.diag_tab = tab;
        self.go(Page::Diag);
    }

    /// Переход в раздел: заодно подтянуть то, что разделу нужно.
    pub(super) fn go(&mut self, page: Page) {
        self.page = page;
        match page {
            Page::Services if !self.ai.loaded => {
                self.call(Op::AiList, "ai:list", Value::Null);
                self.call(Op::DnsList, "dns:list", Value::Null);
            }
            _ => {}
        }
    }

    /// Разделы, кроме главной: каждый рисует себя в области страниц.
    pub(super) fn page_ui(&mut self, ui: &mut Ui, window: Rect) {
        let rect = content_rect(window);
        match self.page {
            Page::Home => {}
            Page::Services => self.services_page(ui, rect),
            Page::Diag => self.diag_page(ui, rect),
            Page::Settings => self.settings_page(ui, rect),
        }
    }
}
