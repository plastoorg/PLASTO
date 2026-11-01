//! «Настройки»: обновления компонентов, общие настройки и о программе.

use egui::{pos2, vec2, Rect, Sense, Ui};
use serde_json::{json, Value};

use super::{
    kit::{self, Pick, Tone},
    pages::glass_box,
    App, Op,
};

/// Строка с тумблером: подпись слева, тумблер справа. Возвращает нажатие.
fn switch_row(ui: &mut Ui, label: &str, on: bool) -> bool {
    let width = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    ui.painter().text(pos2(row.left(), row.center().y), egui::Align2::LEFT_CENTER, label, kit::regular(15.0), kit::INK);
    let sw = Rect::from_min_size(pos2(row.right() - 48.0, row.center().y - 14.0), vec2(48.0, 28.0));
    kit::switch_at(ui, sw, on, true, label).clicked()
}

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut Ui, rect: Rect) {
        // Слева обновления во всю высоту, справа общие и «О программе» под ними.
        let left = ((rect.width() - 16.0) * 0.54).round();
        let updates = Rect::from_min_size(rect.min, vec2(left, rect.height()));
        let general = Rect::from_min_max(pos2(updates.right() + 16.0, rect.top()), pos2(rect.right(), rect.top() + 340.0));
        let about = Rect::from_min_max(pos2(general.left(), general.bottom() + 16.0), rect.max);
        let gpu = self.gpu;
        glass_box(ui, 30, updates, gpu, vec2(24.0, 20.0), |ui| self.updates_card(ui));
        glass_box(ui, 31, general, gpu, vec2(24.0, 20.0), |ui| self.general_card(ui));
        glass_box(ui, 32, about, gpu, vec2(24.0, 20.0), |ui| self.about_card(ui));
    }

    fn general_card(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(12.0, 4.0);
        kit::text(ui, "Общие", kit::semibold(17.0), kit::INK);
        if switch_row(ui, "Запускать вместе с Windows", self.autostart) {
            self.autostart = !self.autostart;
            let on = self.autostart;
            self.set_config(json!({ "autostart": on }));
            self.spawn_autostart(on);
        }
        if switch_row(ui, "При запуске включать то, что было включено", self.auto_connect) {
            self.auto_connect = !self.auto_connect;
            let on = self.auto_connect;
            self.set_config(json!({ "autoConnect": on }));
        }
        if switch_row(ui, "Звуки", self.sound) {
            self.sound = !self.sound;
            let on = self.sound;
            self.set_config(json!({ "sound": on }));
        }
        let check_updates = self.cfg.get("checkUpdates").and_then(Value::as_bool).unwrap_or(true);
        if switch_row(ui, "Проверять обновления при запуске", check_updates) {
            self.set_config(json!({ "checkUpdates": !check_updates }));
        }
        if let Some(note) = &self.behavior_note {
            kit::hint(ui, note);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            kit::text(ui, "Канал обновлений", kit::regular(15.0), kit::INK);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let current = match self.cfg_str("updateChannel") {
                    Some("stable") => "stable",
                    Some("beta") => "beta",
                    _ if crate::selfupdate::beta_by_default() => "beta",
                    _ => "stable",
                };
                let items = [Pick::Item { value: "stable", label: "Стабильный" }, Pick::Item { value: "beta", label: "Бета" }];
                if let Some(v) = kit::select(ui, "update-channel", 180.0, current, &items).filter(|v| v != current) {
                    self.set_config(json!({ "updateChannel": v }));
                    // Другой канал — другая последняя версия: проверка заново.
                    self.upd.app = None;
                    self.upd.checking += 1;
                    self.call(Op::AppCheck, "app:check", Value::Null);
                }
            });
        });
        kit::hint(ui, "В бете — предварительные версии, в том числе автосборки со свежим zapret, которые ещё никто не проверял вручную.");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            kit::text(ui, "При закрытии окна", kit::regular(15.0), kit::INK);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let items = [Pick::Item { value: "ask", label: "Спрашивать" }, Pick::Item { value: "tray", label: "Свернуть в трей" }, Pick::Item { value: "quit", label: "Выходить" }];
                if let Some(v) = kit::select(ui, "close-action", 180.0, &self.close_action, &items) {
                    self.close_action = v.clone();
                    self.set_config(json!({ "closeAction": v }));
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            kit::text(ui, "Фон", kit::regular(15.0), kit::INK);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let current = if self.cfg_str("background") == Some("photo") { "photo" } else { "video" };
                let items = [Pick::Item { value: "video", label: "Видео" }, Pick::Item { value: "photo", label: "Картинка" }];
                if let Some(v) = kit::select(ui, "background", 180.0, current, &items).filter(|v| v != current) {
                    self.set_config(json!({ "background": v }));
                }
            });
        });
        if self.cfg_str("background") != Some("photo") {
            kit::hint(ui, if self.gpu { "Видео идёт, пока окно в фокусе; без фокуса стоит на месте." } else { "Без видеокарты фон — картинка." });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            kit::text(ui, "Telegram-прокси", kit::regular(15.0), kit::INK);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let current = crate::tgws::Flavor::parse(self.cfg_str("tgwsFlavor")).as_str().to_owned();
                let items = [
                    Pick::Item { value: "builtin", label: "Встроенный" },
                    Pick::Item { value: "rust", label: "Rust, скачанный" },
                    Pick::Item { value: "original", label: "Python (Flowseal)" },
                ];
                if let Some(v) = kit::select(ui, "tgws-flavor", 180.0, &current, &items).filter(|v| *v != current) {
                    self.switch_tgws(v);
                }
            });
        });
        let note = match crate::tgws::Flavor::parse(self.cfg_str("tgwsFlavor")) {
            crate::tgws::Flavor::Builtin => "Работает внутри PLASTO и обновляется вместе с ним.",
            crate::tgws::Flavor::Rust => "Отдельная программа без окна: скачивается с GitHub и обновляется сама по себе.",
            crate::tgws::Flavor::Original => "Отдельная программа со своим значком в трее; две её копии не уживаются.",
        };
        kit::hint(ui, note);
    }

    /// Новая версия прокси записывается в настройки бэкендом; работающий
    /// прокси перезапускается на ней, а проверка обновлений идёт заново.
    pub(super) fn switch_tgws(&mut self, flavor: String) {
        self.cfg.insert("tgwsFlavor".into(), json!(flavor));
        self.tgws_busy = true;
        self.upd.tgws = None;
        self.call(Op::TgwsToggle, "tgws:flavor", json!({ "flavor": flavor }));
        self.call(Op::TgwsCheck, "tgws:check", Value::Null);
    }

    fn about_card(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(12.0, 10.0);
        ui.horizontal(|ui| {
            kit::text(ui, "О программе", kit::semibold(17.0), kit::INK);
            kit::text(ui, &format!("PLASTO {}, движок zapret1", self.version), kit::regular(14.0), kit::INK_3);
        });
        if !self.elevated {
            kit::pill(ui, "нет прав администратора", Tone::Bad);
            kit::hint(ui, "WinDivert загружает драйвер в ядро, без прав администратора обход не запустится.");
        }
        kit::hint(ui, "Обход — zapret (bol-van), стратегии — Flowseal. Антивирус может помечать WinDivert как RiskTool: это ожидаемо, добавьте папку в исключения.");
    }
}
