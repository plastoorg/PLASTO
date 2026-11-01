//! Обновления: PLASTO, zapret и Telegram-прокси. Проверка ничего не ставит;
//! обновление и перезапуск — только по кнопке.

use egui::{pos2, vec2, Ui};
use serde_json::Value;

use super::{
    kit::{self, Btn, Tone},
    pages::plate,
    s, App, Boot, Op,
};

/// Что известно про обновления и что сейчас качается.
#[derive(Default)]
pub struct Updates {
    /// Сколько проверок ещё не ответило.
    pub checking: u8,
    pub app: Option<Value>,
    pub zapret: Option<Value>,
    pub tgws: Option<Value>,
    /// Что качается: «app», «zapret» или «tgws».
    pub busy: Option<&'static str>,
    pub app_frac: f32,
    /// Скачанный exe новой версии: ждёт перезапуска.
    pub app_file: Option<String>,
    /// zapret обновился, пока обход работал на прежней версии.
    pub zapret_restart: bool,
    /// Ошибка последнего действия: что и почему.
    pub error: Option<(&'static str, String)>,
}

impl Updates {
    pub fn any(&self) -> bool {
        [&self.app, &self.zapret, &self.tgws].iter().any(|r| r.as_ref().is_some_and(|v| v["hasUpdate"] == true)) || self.app_file.is_some() || self.zapret_restart
    }
}

/// Строка компонента: имя с версией, подпись, справа — состояние.
fn component(ui: &mut Ui, name: &str, version: &str, sub: &str, right: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    ui.painter().line_segment([pos2(left, top), pos2(left + width, top)], egui::Stroke::new(1.0_f32, kit::HAIR));
    ui.allocate_ui_with_layout(vec2(width, 60.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(60.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                kit::text(ui, name, kit::semibold(15.0), kit::INK);
                kit::text(ui, version, kit::regular(15.0), kit::INK_3);
            });
            kit::text(ui, sub, kit::regular(13.0), kit::INK_3);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

/// Справа в строке: «Обновить до …», «последняя» или «не проверено».
/// Возвращает нажатие на «Обновить».
fn state(ui: &mut Ui, check: Option<&Value>, busy: bool, enabled: bool) -> bool {
    let Some(v) = check else { return false };
    if busy {
        kit::text(ui, "качаю…", kit::regular(13.0), kit::INK_3);
        false
    } else if v["hasUpdate"] == true {
        Btn::primary(&format!("Обновить до {}", s(&v["latest"]))).enabled(enabled).show(ui).clicked()
    } else if let Some(e) = v["error"].as_str() {
        kit::pill(ui, "не проверено", Tone::Off).on_hover_text(e);
        false
    } else if v["latest"].is_string() {
        kit::pill(ui, "последняя", Tone::Ok);
        false
    } else {
        false
    }
}

impl App {
    /// Проверить всё сразу; ничего не скачивает.
    pub(super) fn check_updates(&mut self) {
        if self.upd.checking > 0 {
            return;
        }
        self.upd.checking = 3;
        self.upd.error = None;
        self.call(Op::AppCheck, "app:check", Value::Null);
        self.call(Op::ZapretCheck, "zapret:check", Value::Null);
        self.call(Op::TgwsCheck, "tgws:check", Value::Null);
    }

    pub(super) fn on_update_reply(&mut self, op: &Op, v: Value) {
        let failed = |v: &Value| v["error"].as_str().map(str::to_owned);
        match op {
            Op::AppCheck => {
                self.upd.checking = self.upd.checking.saturating_sub(1);
                self.upd.app = Some(v);
            }
            Op::ZapretCheck => {
                self.upd.checking = self.upd.checking.saturating_sub(1);
                self.upd.zapret = Some(v);
            }
            Op::TgwsCheck => {
                self.upd.checking = self.upd.checking.saturating_sub(1);
                self.upd.tgws = Some(v);
            }
            Op::AppDownload => {
                self.upd.busy = None;
                match failed(&v) {
                    Some(e) => self.upd.error = Some(("PLASTO", e)),
                    None => self.upd.app_file = v["file"].as_str().map(str::to_owned),
                }
            }
            Op::TgwsUpdate => {
                self.upd.busy = None;
                match failed(&v) {
                    Some(e) => self.upd.error = Some(("Telegram-прокси", e)),
                    None => {
                        if let Some(t) = self.upd.tgws.as_mut() {
                            t["hasUpdate"] = false.into();
                            t["builtin"] = false.into();
                            t["installed"] = v["tag"].clone();
                        }
                        // Вместо встроенного теперь скачанная Rust-версия.
                        if let Some(flavor) = v["flavor"].as_str() {
                            self.cfg.insert("tgwsFlavor".into(), flavor.into());
                        }
                        self.call(Op::TgwsStatus, "tgws:status", Value::Null);
                    }
                }
            }
            Op::EngineRestart => {
                if let Some(e) = failed(&v) {
                    self.upd.error = Some(("zapret", e));
                }
            }
            _ => {}
        }
    }

    /// zapret обновился (кнопкой «Обновить»): обход надо перезапустить, если работает.
    pub(super) fn zapret_updated(&mut self) {
        if let Some(z) = self.upd.zapret.as_mut() {
            z["hasUpdate"] = false.into();
        }
        if self.state == "on" || self.state == "timeout" {
            self.upd.zapret_restart = true;
        }
    }

    pub(super) fn updates_card(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
        let mut check = false;
        ui.horizontal(|ui| {
            kit::text(ui, "Обновления", kit::semibold(17.0), kit::INK);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.upd.checking > 0 { "Проверяю…" } else { "Проверить обновления" };
                check = Btn::secondary(label).enabled(self.upd.checking == 0 && self.upd.busy.is_none() && self.boot.is_none()).show(ui).clicked();
            });
        });
        if check {
            self.check_updates();
        }
        let idle = self.upd.busy.is_none() && self.boot.is_none();

        // PLASTO
        let version = self.version.clone();
        let app = self.upd.app.clone();
        let mut get_app = false;
        let ready = self.upd.app_file.is_some();
        component(ui, "PLASTO", &version, "это приложение", |ui| {
            if ready {
                kit::pill(ui, "скачано", Tone::Ok);
            } else {
                get_app = state(ui, app.as_ref(), self.upd.busy == Some("app"), idle);
            }
        });
        if get_app {
            self.upd.busy = Some("app");
            self.upd.app_frac = 0.0;
            self.call(Op::AppDownload, "app:download", Value::Null);
        }
        if self.upd.busy == Some("app") {
            let width = ui.available_width();
            kit::progress(ui, self.upd.app_frac, width);
        }
        if let Some(file) = self.upd.app_file.clone() {
            kit::hint(ui, "Новая версия скачана и сверена с GitHub. PLASTO закроется и сразу откроется обновлённым; то, что было включено, включится снова.");
            if Btn::primary("Перезапустить PLASTO").show(ui).clicked() {
                self.restart_updated(&file);
            }
        }

        // zapret
        let (badge, installed) = self.ver_badge.clone();
        let badge = badge.strip_prefix("zapret ").unwrap_or(&badge).to_owned();
        let zapret = self.upd.zapret.clone();
        let mut get_zapret = false;
        let zapret_busy = self.upd.busy == Some("zapret") || self.boot.is_some();
        component(ui, "zapret", if installed { &badge } else { "" }, "Flowseal zapret-discord-youtube, обход для YouTube и Discord", |ui| {
            if !installed {
                kit::pill(ui, "не установлен", Tone::Bad);
            } else {
                get_zapret = state(ui, zapret.as_ref(), zapret_busy, idle);
            }
        });
        if get_zapret {
            self.upd.busy = Some("zapret");
            self.boot = Some(Boot { title: format!("Обновляю до {}", s(&zapret.as_ref().map_or(Value::Null, |z| z["latest"].clone()))), sub: String::new(), frac: 0.0 });
            self.call(Op::ZapretInstall { auto: false }, "zapret:install", Value::Null);
        }
        if let Some(boot) = &self.boot {
            let width = ui.available_width();
            kit::text(ui, &format!("{}: {}", boot.title, boot.sub), kit::regular(13.0), kit::INK_3);
            kit::progress(ui, boot.frac, width);
        }
        if self.upd.zapret_restart {
            kit::hint(ui, "zapret обновлён, но обход ещё работает на прежней версии. Перезапуск займёт пару секунд.");
            ui.horizontal(|ui| {
                if Btn::primary("Перезапустить обход").show(ui).clicked() {
                    self.upd.zapret_restart = false;
                    self.mark_user_action();
                    self.call(Op::EngineRestart, "engine:restart", Value::Null);
                }
                if Btn::secondary("Позже").show(ui).clicked() {
                    self.upd.zapret_restart = false;
                }
            });
        }

        // Telegram-прокси
        let tg = s(&self.tgws["installed"]["tag"]).to_owned();
        let tg_sub = if tg.is_empty() { format!("{}, скачается при первом включении", self.tgws_name()) } else { self.tgws_name().to_owned() };
        let tgws = self.upd.tgws.clone().filter(|_| !tg.is_empty());
        let mut get_tg = false;
        component(ui, "Telegram-прокси", &tg, &tg_sub, |ui| {
            get_tg = state(ui, tgws.as_ref(), self.upd.busy == Some("tgws"), idle);
        });
        if get_tg {
            self.upd.busy = Some("tgws");
            self.call(Op::TgwsUpdate, "tgws:update", Value::Null);
        }
        // Встроенный новее не станет без нового PLASTO; скачанный, которого
        // встроенный догнал, можно сменить обратно на встроенный.
        if tgws.as_ref().is_some_and(|t| t["builtin"] == true && t["hasUpdate"] == true) {
            kit::hint(ui, "Встроенный прокси обновится вместе с PLASTO. Кнопка скачает новую версию отдельной программой; вернуться на встроенный можно в настройках.");
        }
        if tgws.as_ref().is_some_and(|t| t["builtinCaughtUp"] == true) {
            kit::hint(ui, &format!("Во встроенном прокси PLASTO уже версия v{} — отдельная программа больше не нужна.", crate::tgws::BUILTIN));
            if Btn::secondary("Перейти на встроенный").enabled(!self.tgws_busy).show(ui).clicked() {
                self.switch_tgws("builtin".into());
            }
        }

        if let Some((what, e)) = &self.upd.error {
            kit::hint(ui, &format!("{what}: {e}"));
        }

        kit::hairline(ui);
        ui.add_space(4.0);
        let mut open = false;
        let mut pick = false;
        kit::text(ui, "Папка компонентов PLASTO", kit::regular(13.0), kit::INK_3);
        plate(ui, &self.components_dir, false);
        ui.horizontal(|ui| {
            open = Btn::secondary("Открыть").show(ui).clicked();
            pick = Btn::secondary("Изменить…").show(ui).clicked();
        });
        if open {
            self.call(Op::Ignore, "components:openDir", Value::Null);
        }
        if pick {
            self.components_note = None;
            self.call(Op::Components, "components:pickDir", Value::Null);
        }
        match &self.components_note {
            Some(note) => kit::hint(ui, note),
            None => kit::hint(ui, "Сюда скачиваются zapret и Telegram-прокси. При смене папки уже скачанное переедет в новую."),
        }
    }

    /// Подменить exe скачанным и выйти: новая версия стартует сама.
    fn restart_updated(&mut self, file: &str) {
        match crate::selfupdate::install(std::path::Path::new(file)) {
            Ok(()) => self.quit(),
            Err(e) => {
                self.upd.app_file = None;
                self.upd.error = Some(("PLASTO", e));
            }
        }
    }
}
