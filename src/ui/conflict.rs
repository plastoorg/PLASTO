//! «Уже работает другой zapret» и «порт Telegram-прокси занят»: вместо
//! ошибки PLASTO показывает, что мешает, и предлагает это убрать.

use egui::{vec2, Color32, Sense, Ui};
use serde_json::{json, Value};

use super::{
    glass,
    kit::{self, Btn},
    s, u, App, Op,
};

/// Что повторить, когда помеха убрана.
#[derive(Clone, Copy, Debug)]
pub enum Retry {
    Power,
    Toggle,
    Tests,
}

pub enum Kind {
    /// Другой zapret: службы и процессы winws.
    Winws { found: Value, retry: Retry, disable: bool },
    /// Другой tg-ws-proxy: две копии не уживаются, порт тут не поможет.
    Tgws { owner: Value },
    /// Порт прокси занят чужой программой.
    Port { owner: Value, port: u16, free: Option<u16> },
}

pub struct Conflict {
    pub kind: Kind,
    pub busy: bool,
    pub error: Option<String>,
}

impl App {
    /// Ответ с `conflict` открывает окно и возвращает `true`: ошибку
    /// тогда показывать не надо.
    pub(super) fn take_conflict(&mut self, v: &Value, retry: Retry) -> bool {
        let kind = match s(&v["conflict"]) {
            "winws" => {
                let found = v["found"].clone();
                // Отключать автозапуск предлагаем сразу, если служба с ним есть.
                let disable = found["services"].as_array().is_some_and(|a| a.iter().any(|x| x["auto"] == true));
                Kind::Winws { found, retry, disable }
            }
            "tgws" => Kind::Tgws { owner: v["owner"].clone() },
            "port" => Kind::Port { owner: v["owner"].clone(), port: u(&v["port"]) as u16, free: v["free"].as_u64().map(|p| p as u16) },
            _ => return false,
        };
        self.conflict = Some(Conflict { kind, busy: false, error: None });
        true
    }

    /// Помеха убрана (или не убралась): повторить или показать почему.
    pub(super) fn on_conflict_reply(&mut self, op: &Op, v: Value) {
        let Some(c) = self.conflict.as_mut() else { return };
        c.busy = false;
        if let Some(e) = v["error"].as_str() {
            let retry = match &c.kind {
                Kind::Winws { retry, .. } => *retry,
                _ => Retry::Toggle,
            };
            // Вместо одной помехи нашлась другая — окно покажет её.
            if !v["conflict"].is_string() {
                c.error = Some(super::capitalize(e));
            } else {
                self.take_conflict(&v, retry);
            }
            return;
        }
        let kind = std::mem::replace(&mut c.kind, Kind::Tgws { owner: Value::Null });
        self.conflict = None;
        match (op, kind) {
            (Op::ConflictWinws, Kind::Winws { retry, .. }) => match retry {
                Retry::Power => self.power(),
                Retry::Toggle => self.toggle(),
                Retry::Tests => self.start_tests(None),
            },
            (Op::TgwsResolve, Kind::Port { .. }) => {
                let port = v["port"].clone();
                self.cfg.insert("tgPort".into(), port.clone());
                self.paint_tgws(v);
                self.flash(format!("Telegram-прокси на порту {port}. Откройте ссылку в Telegram заново: старая ведёт на прежний порт"));
            }
            // Прокси PLASTO мог занять порт остановленного: Telegram уже
            // настроен на него.
            (Op::TgwsResolve, _) => {
                let before = self.cfg.get("tgPort").and_then(Value::as_u64).unwrap_or(u64::from(crate::tgws::DEFAULT_PORT));
                if let Some(port) = v["port"].as_u64().filter(|p| *p != before) {
                    self.cfg.insert("tgPort".into(), port.into());
                    self.flash(format!("Telegram-прокси PLASTO на порту {port}, как был прежний: Telegram переподключится сам"));
                }
                self.paint_tgws(v);
            }
            _ => {}
        }
    }

    pub(super) fn conflict_ui(&mut self, ui: &mut Ui) {
        let Some(c) = self.conflict.as_ref() else { return };
        let screen = ui.max_rect();
        ui.interact(screen, ui.id().with("conflict-modal"), Sense::click());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(120));

        // Строки: что именно мешает.
        let mut rows: Vec<(String, String)> = Vec::new();
        let (title, detail) = match &c.kind {
            Kind::Winws { found, .. } => {
                for svc in found["services"].as_array().into_iter().flatten() {
                    let mut sub = vec![if svc["running"] == true { "работает" } else { "остановлена" }];
                    if svc["auto"] == true {
                        sub.push("запускается вместе с Windows");
                    }
                    let name = if svc["title"].is_string() && svc["title"] != svc["name"] { format!("Служба «{}» ({})", s(&svc["title"]), s(&svc["name"])) } else { format!("Служба «{}»", s(&svc["name"])) };
                    rows.push((name, sub.join(", ")));
                }
                for p in found["processes"].as_array().into_iter().flatten() {
                    rows.push((format!("{}, процесс {}", s(&p["name"]), u(&p["pid"])), s(&p["path"]).to_owned()));
                }
                ("Уже работает другой zapret", "Два zapret одновременно не работают: они делят один драйвер. PLASTO может остановить чужой и включить свой.")
            }
            Kind::Tgws { owner } => {
                rows.push((format!("{}, процесс {}", s(&owner["name"]), u(&owner["pid"])), "запущен не из PLASTO".into()));
                ("Telegram-прокси уже запущен", "Это tg-ws-proxy, запущенный отдельно. Две его копии не уживаются, поэтому другой порт тут не поможет: остановите его, и PLASTO запустит свой.")
            }
            Kind::Port { owner, port, .. } => {
                if owner.is_object() {
                    rows.push((format!("{}, процесс {}", s(&owner["name"]), u(&owner["pid"])), format!("слушает порт {port}")));
                }
                ("Порт Telegram-прокси занят", "Порт уже слушает другая программа. Можно взять другой порт: PLASTO его запомнит, а ссылку в Telegram нужно будет открыть заново.")
            }
        };
        let rows: Vec<_> = rows.into_iter().take(4).collect();
        let checkbox = matches!(&c.kind, Kind::Winws { found, .. } if found["services"].as_array().is_some_and(|a| !a.is_empty()));
        let height = 150.0 + rows.len() as f32 * 48.0 + if checkbox { 40.0 } else { 0.0 } + if c.error.is_some() { 40.0 } else { 0.0 };
        let panel = egui::Rect::from_center_size(screen.center(), vec2(560.0, height));
        glass::panel(ui, 91, panel, 16.0, self.gpu);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink2(vec2(28.0, 24.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing = vec2(12.0, 10.0);
        kit::text(ui, title, kit::semibold(17.0), kit::INK);
        kit::hint(ui, detail);
        for (name, sub) in &rows {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                kit::text(ui, name, kit::regular(15.0), kit::INK);
                let w = ui.available_width();
                let g = super::theme::elided(ui, sub, kit::regular(13.0), kit::INK_3, w);
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, kit::INK_3);
            });
        }

        let busy = c.busy;
        let mut action: Option<(Op, &'static str, Value)> = None;
        let mut cancel = false;
        let mut toggle_disable = false;
        match &c.kind {
            Kind::Winws { disable, .. } => {
                if checkbox && kit::checkbox(ui, *disable, "Отключить автозапуск службы, чтобы она не вернулась после перезагрузки", !busy).clicked() {
                    toggle_disable = true;
                }
                if let Some(e) = &c.error {
                    kit::hint(ui, e);
                }
                ui.horizontal(|ui| {
                    let label = if busy { "Останавливаю…" } else { "Остановить и включить PLASTO" };
                    if Btn::primary(label).enabled(!busy).show(ui).clicked() {
                        action = Some((Op::ConflictWinws, "conflict:stopWinws", json!({ "disable": *disable })));
                    }
                    cancel = Btn::secondary("Отмена").enabled(!busy).show(ui).clicked();
                });
            }
            Kind::Tgws { owner } => {
                if let Some(e) = &c.error {
                    kit::hint(ui, e);
                }
                ui.horizontal(|ui| {
                    let label = if busy { "Останавливаю…" } else { "Остановить его и запустить из PLASTO" };
                    if Btn::primary(label).enabled(!busy).show(ui).clicked() {
                        action = Some((Op::TgwsResolve, "tgws:stopOther", json!({ "pid": owner["pid"] })));
                    }
                    cancel = Btn::secondary("Отмена").enabled(!busy).show(ui).clicked();
                });
            }
            Kind::Port { owner, free, .. } => {
                if let Some(e) = &c.error {
                    kit::hint(ui, e);
                }
                ui.horizontal(|ui| {
                    if let Some(free) = free {
                        if Btn::primary(&format!("Взять порт {free}")).enabled(!busy).show(ui).clicked() {
                            action = Some((Op::TgwsResolve, "tgws:usePort", json!({ "port": free })));
                        }
                    }
                    // Системные процессы останавливать не предлагаем.
                    let stoppable = owner.is_object() && owner["system"] != true;
                    if stoppable && Btn::secondary(&format!("Остановить {}", s(&owner["name"]))).enabled(!busy).show(ui).clicked() {
                        action = Some((Op::TgwsResolve, "tgws:stopOther", json!({ "pid": owner["pid"] })));
                    }
                    cancel = Btn::secondary("Отмена").enabled(!busy).show(ui).clicked();
                });
            }
        }

        if let Some(c) = self.conflict.as_mut() {
            if toggle_disable {
                if let Kind::Winws { disable, .. } = &mut c.kind {
                    *disable = !*disable;
                }
            }
            if let Some((op, name, payload)) = action {
                c.busy = true;
                c.error = None;
                if matches!(op, Op::TgwsResolve) {
                    self.tgws_busy = true;
                }
                self.call(op, name, payload);
            } else if cancel {
                self.conflict = None;
            }
        }
    }
}

/// Только в отладочной сборке (`PLASTO_CONFLICT=winws|tgws|port`): окно
/// конфликта с подставленными данными.
#[cfg(debug_assertions)]
impl App {
    pub(super) fn debug_conflict(&mut self) {
        let v = match std::env::var("PLASTO_CONFLICT").as_deref() {
            Ok("winws") => json!({ "conflict": "winws", "found": {
                "services": [{ "name": "zapret", "title": "zapret", "running": true, "auto": true, "path": r"C:\zapret\bin\winws.exe --wf-tcp=80,443" }],
                "processes": [{ "pid": 7412, "name": "winws.exe", "path": r"C:\Users\user\Downloads\zapret-discord-youtube-1.9.4\bin\winws.exe" }] } }),
            Ok("tgws") => json!({ "conflict": "tgws", "owner": { "pid": 5120, "name": "TgWsProxy_windows.exe", "tgws": true, "system": false } }),
            Ok("port") => json!({ "conflict": "port", "port": 1443, "free": 1444, "owner": { "pid": 3388, "name": "nginx.exe", "tgws": false, "system": false } }),
            _ => return,
        };
        self.take_conflict(&v, Retry::Power);
    }
}
