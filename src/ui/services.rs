//! «Сервисы»: слева механизмы обхода с тумблерами, справа — настройки
//! выбранного. Новый механизм — ещё одна строка слева, навигация не растёт.

use egui::{pos2, vec2, Rect, Sense, Ui};
use serde_json::{json, Value};

use super::{
    icons,
    kit::{self, Btn, Pick, Tone},
    pages::{glass_box, plate, status_line, title},
    s, App, DiagTab, Mech, Op,
};

/// Группы стратегий в выпадающем списке — как в сборке Flowseal.
const GROUPS: [(&str, &str); 5] = [("BASE", "Базовые"), ("ALT", "ALT"), ("FAKE", "FAKE"), ("BAT", "BAT"), ("LUA", "Lua")];

impl App {
    pub(super) fn services_page(&mut self, ui: &mut Ui, rect: Rect) {
        let list = Rect::from_min_size(rect.min, vec2(340.0, rect.height()));
        let detail = Rect::from_min_max(pos2(list.right() + 16.0, rect.top()), rect.max);

        // Слева: механизмы. Строка выбирает, тумблер включает.
        crate::ui::glass::panel(ui, 10, list, 16.0, self.gpu);
        let mut y = list.top() + 12.0;
        for mech in [Mech::Zapret, Mech::Telegram, Mech::Ai, Mech::Dns] {
            let info = self.mech_info(mech);
            let row = Rect::from_min_size(pos2(list.left() + 12.0, y), vec2(list.width() - 24.0, 64.0));
            let resp = ui.interact(row, ui.id().with(("svc-row", mech as u8)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let p = ui.painter().clone();
            if self.mech == mech {
                p.rect_filled(row, 12.0, kit::FILL_2);
            } else if resp.hovered() {
                p.rect_filled(row, 12.0, kit::FILL_1);
            }
            icons::paint(&p, pos2(row.left() + 24.0, row.center().y), info.icon, 22.0, kit::INK_2);
            let name = p.layout_no_wrap(info.name.to_owned(), kit::semibold(15.0), kit::INK);
            p.galley(pos2(row.left() + 48.0, row.center().y - name.size().y - 2.0), name, kit::INK);
            let pill_w = row.width() - 48.0 - 12.0 - 48.0 - 12.0;
            let mut pill_ui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(row.left() + 48.0, row.center().y + 3.0), vec2(pill_w, 24.0))));
            // Как на главной: включено, но проверка не прошла — так и сказать.
            let warn = if info.on && !info.busy { self.ping_text(mech).filter(|(_, tone, _)| *tone == Tone::Warn) } else { None };
            let pill = match warn {
                Some((text, tone, _)) => kit::pill(&mut pill_ui, &text, tone),
                None => kit::pill(&mut pill_ui, info.status, info.tone),
            };
            // Пока тумблер ждёт, рядом с плашкой — что именно делается.
            if info.busy {
                let g = super::theme::elided(ui, &info.sub, kit::regular(13.0), kit::INK_3, pill_w - pill.rect.width() - 8.0);
                p.galley(pos2(pill.rect.right() + 8.0, pill.rect.center().y - g.size().y / 2.0), g, kit::INK_3);
            }
            let sw = Rect::from_min_size(pos2(row.right() - 12.0 - 48.0, row.center().y - 14.0), vec2(48.0, 28.0));
            let switched = kit::switch_at(ui, sw, info.on, !info.busy, info.name).clicked();
            if info.busy {
                kit::switch_spinner(ui, sw, info.on);
            }
            if switched {
                self.mech = mech;
                self.mech_switch(mech);
            } else if resp.clicked() {
                self.mech = mech;
            }
            y += 64.0 + 4.0;
        }

        // Справа: настройки выбранного механизма.
        let gpu = self.gpu;
        glass_box(ui, 11, detail, gpu, vec2(28.0, 24.0), |ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
            match self.mech {
                Mech::Zapret => self.zapret_detail(ui),
                Mech::Telegram => self.telegram_detail(ui),
                Mech::Ai => self.ai_detail(ui),
                Mech::Dns => self.dns_detail(ui),
            }
        });
    }

    /* ---------- YouTube и Discord: zapret ------------------------------- */

    fn zapret_detail(&mut self, ui: &mut Ui) {
        title(ui, "YouTube и Discord", "Обход через zapret (winws). Сайты из списков идут через выбранную стратегию.");
        ui.add_space(8.0);

        kit::label(ui, "Стратегия");
        if self.strategies.is_empty() {
            kit::hint(ui, "Компоненты zapret не установлены — стратегий пока нет.");
            if Btn::primary("Установить компоненты").show(ui).clicked() {
                self.call(Op::ZapretInstall { auto: false }, "zapret:install", Value::Null);
            }
        } else {
            let auto = self.cfg_str("strategyMode").unwrap_or("auto") == "auto";
            if let Some(i) = kit::segmented(ui, "strategy-mode", &["Автоматически", "Вручную"], if auto { 0 } else { 1 }) {
                self.set_config(json!({ "strategyMode": if i == 0 { "auto" } else { "manual" } }));
            }
            let current = self.label_of(&self.sel_strategy);
            if auto {
                ui.horizontal(|ui| {
                    let text_w = ui.available_width() - Btn::secondary("Подобрать заново").width(ui) - 16.0;
                    ui.vertical(|ui| {
                        ui.set_width(text_w);
                        let g = ui.painter().layout_no_wrap(format!("Сейчас: {current}"), kit::semibold(15.0), kit::INK);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, kit::INK);
                        kit::hint(ui, "Подобрана при первом запуске. Если сайты перестанут открываться, PLASTO подберёт новую сам.");
                    });
                    if Btn::secondary("Подобрать заново").show(ui).clicked() {
                        self.open_diag(DiagTab::Tests);
                    }
                });
            } else {
                let known: Vec<&str> = GROUPS.iter().map(|(g, _)| *g).collect();
                let mut items = Vec::new();
                for (g, name) in GROUPS.iter().copied().chain(std::iter::once(("", "Другие"))) {
                    let group: Vec<_> = self.strategies.iter().filter(|p| if g.is_empty() { !known.contains(&p.group.as_str()) } else { p.group == g }).collect();
                    if group.is_empty() {
                        continue;
                    }
                    items.push(Pick::Group(name));
                    items.extend(group.into_iter().map(|p| Pick::Item { value: p.file.as_str(), label: p.label.as_str() }));
                }
                let mut picked = None;
                let mut find_best = false;
                ui.horizontal(|ui| {
                    let w = ui.available_width() - Btn::primary("Подобрать лучшую").width(ui) - 12.0;
                    picked = kit::select(ui, "strategy", w, &self.sel_strategy, &items);
                    find_best = Btn::primary("Подобрать лучшую").show(ui).clicked();
                });
                if let Some(file) = picked {
                    self.save_strategy(file, true);
                }
                if find_best {
                    self.open_diag(DiagTab::Tests);
                }
                kit::hint(ui, "Стратегия не меняется сама. Подбор прогонит все и выберет ту, при которой открывается больше сайтов. Смена на ходу перезапускает обход.");
            }
        }
        ui.add_space(8.0);

        kit::label(ui, "Игровой фильтр");
        let modes = ["off", "tcp", "udp", "all"];
        let at = modes.iter().position(|m| *m == self.game_filter).unwrap_or(0);
        if let Some(i) = kit::segmented(ui, "game-filter", &["Выкл", "TCP", "UDP", "Все"], at) {
            let v = modes[i].to_owned();
            self.game_filter = v.clone();
            self.cfg.insert("gameFilter".into(), json!(v));
            self.call(Op::GameFilter, "engine:gameFilter", json!(v));
        }
        kit::hint(ui, "Расширяет обход на порты от 1024 до 65535. Нужно для некоторых онлайн-игр, но может задеть то, чего вы не ждали: включайте, только если игра не работает.");
        ui.add_space(8.0);

        kit::label(ui, "Свои домены");
        let width = ui.available_width();
        kit::input(ui, &mut self.ai.extra, "example.com, media.example.org", width, 3);
        ui.horizontal(|ui| {
            let label = if self.ai.busy_apply { "Сохраняю…" } else { "Сохранить домены" };
            if Btn::secondary(label).enabled(!self.ai.busy_apply).show(ui).clicked() {
                self.apply_ai();
            }
        });
        kit::hint(ui, "Через пробел, запятую или с новой строки. Запись накрывает и поддомены: openai.com включает api.openai.com. Если обход запущен, он перезапустится.");
        ui.add_space(8.0);

        if kit::disclosure(ui, "zapret-adv", "Для опытных") {
            kit::label(ui, "Движок");
            let width = ui.available_width();
            let items = [
                Pick::Item { value: "", label: "Определять автоматически" },
                Pick::Item { value: "zapret1", label: "zapret 1: winws.exe, пресеты .bat" },
                Pick::Item { value: "zapret2", label: "zapret 2: winws2.exe, стратегии на Lua" },
            ];
            if let Some(v) = kit::select(ui, "engine", width, &self.engine_mode, &items) {
                let payload = if v.is_empty() { Value::Null } else { json!(v) };
                self.call(Op::EngineMode(v), "engine:setMode", payload);
            }
            kit::hint(ui, &self.engine_hint.0);
            if let Some((text, _bad)) = self.preview.clone() {
                kit::label(ui, "Команда запуска");
                plate(ui, &text, true);
            }
            ui.horizontal(|ui| {
                if Btn::secondary("Открыть файл списка").show(ui).clicked() {
                    self.call(Op::Ignore, "ai:openList", Value::Null);
                }
                let label = if self.check_busy { "Проверяю…" } else { "Проверить соединение" };
                if Btn::secondary(label).enabled(!self.check_busy).show(ui).clicked() {
                    self.check_busy = true;
                    self.call(Op::Check, "engine:check", Value::Null);
                }
            });
            if let Some(note) = &self.probes_note {
                kit::hint(ui, note);
            }
            for (name, ok, text) in self.probes.clone() {
                kit::kv(ui, &name, |ui| {
                    kit::pill(ui, &text, if ok { Tone::Ok } else { Tone::Bad });
                });
            }
        }
    }

    /// Списки ИИ и свои домены пишутся одним вызовом: это один файл обхода.
    pub(super) fn apply_ai(&mut self) {
        self.ai.busy_apply = true;
        let payload = json!({
            "bypass": self.ai.bypass.iter().collect::<Vec<_>>(),
            "proxy": self.ai.proxy.iter().collect::<Vec<_>>(),
            "blocklist": self.ai.blocklist,
            "extra": self.ai.extra,
            "entry": self.ai.entry.trim(),
        });
        self.call(Op::AiApply, "ai:apply", payload);
    }

    /* ---------- Telegram ------------------------------------------------- */

    fn telegram_detail(&mut self, ui: &mut Ui) {
        title(ui, "Telegram", &format!("Локальный MTProto-прокси {}. Telegram подключается к нему как к обычному прокси. Версию прокси можно сменить в настройках.", self.tgws_name()));
        ui.add_space(8.0);
        let info = self.mech_info(Mech::Telegram);
        kit::kv(ui, "Состояние", |ui| {
            kit::pill(ui, info.status, info.tone);
        });
        let addr = format!("{}:{}", if self.tgws["host"].is_string() { s(&self.tgws["host"]) } else { "127.0.0.1" }, match (self.tgws["running"] == true, &self.tgws["port"], self.cfg.get("tgPort")) {
            (true, port, _) if port.is_number() => port.to_string(),
            (_, _, Some(port)) if port.is_number() => port.to_string(),
            _ => "1443".into(),
        });
        kit::kv(ui, "Адрес", |ui| {
            kit::text(ui, &addr, kit::regular(14.0), kit::INK);
        });
        kit::hairline(ui);
        ui.add_space(8.0);
        status_line(ui, &self.tgws_status);
        kit::label(ui, "Подключить Telegram");
        let has_link = self.tgws["link"].is_string();
        ui.horizontal(|ui| {
            if Btn::primary("Открыть в Telegram").enabled(has_link).show(ui).clicked() {
                self.call(Op::TgwsLink, "tgws:openLink", Value::Null);
            }
            if Btn::secondary("Скопировать ссылку").enabled(has_link).show(ui).clicked() {
                ui.ctx().copy_text(s(&self.tgws["link"]).to_owned());
                self.copied = Some((std::time::Instant::now(), true));
            }
        });
        let copied = self.copied.is_some_and(|(t, _)| t.elapsed().as_millis() < 1600);
        kit::hint(ui, if copied { "Ссылка скопирована." } else if has_link { "Достаточно сделать это один раз: Telegram запомнит прокси." } else { "Ссылка появится, когда прокси запустится." });
    }

    /* ---------- ИИ-сервисы ---------------------------------------------- */

    fn ai_detail(&mut self, ui: &mut Ui) {
        title(ui, "ИИ-сервисы", "Для сервисов, которые закрыты с двух сторон: провайдер режет сайт по имени (это снимает обход), а сам сервис не пускает из России (это снимает прокси через hosts). Обычно нужны обе галки.");
        ui.add_space(8.0);
        if !self.ai.loaded {
            kit::hint(ui, "Загружаю список…");
            return;
        }
        // Шапка колонок галок.
        let width = ui.available_width();
        let (head, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
        for (text, x) in [("обход", head.right() - 104.0), ("прокси", head.right() - 32.0)] {
            ui.painter().text(pos2(x, head.center().y), egui::Align2::CENTER_CENTER, text, kit::semibold(13.0), kit::INK_3);
        }
        let services = self.ai.services.clone();
        for svc in &services {
            let id = s(&svc["id"]).to_owned();
            // Свой прокси отдаёт то, что в нём настроено: каталог ему не указ.
            let proxyable = svc["proxyable"] != 0 || !self.ai.entry.trim().is_empty();
            let (row, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
            let p = ui.painter().clone();
            p.line_segment([row.left_top(), row.right_top()], egui::Stroke::new(1.0_f32, kit::HAIR));
            let (tag, tone) = match s(&svc["block"]) {
                "isp" => ("провайдер", Tone::Off),
                "geo" => ("по стране", Tone::Warn),
                "both" => ("и то и то", Tone::Warn),
                _ => ("", Tone::Off),
            };
            let name = super::theme::elided(ui, s(&svc["name"]), kit::regular(15.0), kit::INK, width - 260.0);
            let name_w = name.size().x;
            p.galley(pos2(row.left(), row.center().y - name.size().y / 2.0), name, kit::INK);
            if !tag.is_empty() {
                let mut pill_ui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(row.left() + name_w + 12.0, row.center().y - 12.0), vec2(140.0, 24.0))));
                kit::pill(&mut pill_ui, tag, tone);
            }
            for (col, x, enabled) in [("bypass", row.right() - 104.0, true), ("proxy", row.right() - 32.0, proxyable)] {
                let on = if col == "bypass" { self.ai.bypass.contains(&id) } else { self.ai.proxy.contains(&id) };
                let cell = Rect::from_center_size(pos2(x, row.center().y), vec2(20.0, 20.0));
                let mut cell_ui = ui.new_child(egui::UiBuilder::new().id_salt((&id, col)).max_rect(cell.expand(4.0)));
                let resp = kit::checkbox(&mut cell_ui, on, "", enabled);
                let resp = if enabled { resp } else { resp.on_hover_text("у каталога нет адресов для этого сервиса") };
                if resp.clicked() {
                    let set = if col == "bypass" { &mut self.ai.bypass } else { &mut self.ai.proxy };
                    if on {
                        set.remove(&id);
                    } else {
                        set.insert(id.clone());
                    }
                }
            }
        }
        kit::hairline(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = if self.ai.busy_apply { "Записываю…" } else { "Применить" };
            if Btn::primary(label).enabled(!self.ai.busy_apply).show(ui).clicked() {
                self.apply_ai();
            }
            let busy = self.ai.probe_busy;
            if Btn::secondary(if busy { "Проверяю…" } else { "Проверить доступ" }).enabled(!busy).show(ui).clicked() {
                self.probe_ai(false);
            }
            if Btn::secondary("Вернуть hosts").show(ui).clicked() {
                self.call(Op::AiRestore, "ai:restoreHosts", Value::Null);
            }
        });
        if let Some(st) = self.ai.apply_state.clone() {
            status_line(ui, &st);
        }
        if self.ai.probe_busy {
            let w = ui.available_width();
            kit::progress(ui, self.ai.probe_frac, w);
        }
        if let Some(v) = self.ai.verdict.clone() {
            status_line(ui, &v);
        }
        if let Some(note) = &self.ai.rows_note {
            kit::hint(ui, note);
        }
        for r in self.ai.rows.clone() {
            let (tone, state) = ai_state(s(&r["state"]));
            let mut detail = s(&r["host"]).to_owned();
            if let Some(ip) = r["ip"].as_str() {
                detail += &format!(" → {ip}");
            }
            detail += &format!(", {}", s(&r["detail"]));
            if r["ms"].is_number() {
                detail += &format!(", {} мс", super::u(&r["ms"]));
            }
            kit::kv(ui, s(&r["name"]), |ui| {
                kit::pill(ui, state, tone);
            });
            kit::hint(ui, &detail);
            if let Some(fix) = r["fix"].as_str().and_then(ai_fix) {
                kit::hint(ui, &format!("Что сделать: {fix}"));
            }
        }
        ui.add_space(8.0);

        if kit::disclosure(ui, "ai-adv", "Для опытных") {
            let resp = kit::checkbox(ui, self.ai.blocklist, "Заодно ставить блоклист (26 500 имён)", true);
            if resp.clicked() {
                self.ai.blocklist = !self.ai.blocklist;
            }
            kit::hint(ui, "Реклама, скам и IP-логгеры уедут в 0.0.0.0. Если уже стоит AdGuard или другой блокировщик, правящий hosts, включайте что-то одно.");
            ui.horizontal(|ui| {
                if Btn::secondary(if self.ai.busy_source { "Качаю…" } else { "Обновить каталог" }).enabled(!self.ai.busy_source).show(ui).clicked() {
                    self.ai.busy_source = true;
                    self.call(Op::AiSource, "ai:refreshSource", Value::Null);
                }
                if Btn::secondary("Показать hosts").show(ui).clicked() {
                    self.call(Op::Ignore, "ai:openHosts", Value::Null);
                }
            });
            if let Some(st) = self.ai.hosts_state.clone() {
                status_line(ui, &st);
            }
            if self.ai.stale > 0 {
                if let Some(st) = self.ai.stale_state.clone() {
                    status_line(ui, &st);
                }
                if Btn::secondary("Убрать устаревшие дубли").enabled(!self.ai.busy_stale).show(ui).clicked() {
                    self.ai.busy_stale = true;
                    self.call(Op::AiDropStale, "ai:dropStale", Value::Null);
                }
            }
            kit::label(ui, "Свой SNI-прокси");
            let width = ui.available_width();
            kit::input(ui, &mut self.ai.entry, "IPv4 своего сервера, пусто — адреса каталога", width, 1);
            kit::hint(ui, "Сервер за границей, который пересылает TLS по имени из рукопожатия, не расшифровывая (nginx stream + ssl_preread). Все выбранные в колонке «прокси» имена пойдут на него; каждое проверяется до записи. Сохраняется кнопкой «Применить».");
            kit::label(ui, "Прокси для проверки");
            let width = ui.available_width();
            let resp = kit::input(ui, &mut self.ai.probe_proxy, "http://127.0.0.1:8888", width, 1);
            if resp.lost_focus() {
                self.call(Op::Ignore, "ai:setProxy", json!(self.ai.probe_proxy));
            }
            if Btn::secondary("Проверить через прокси").enabled(!self.ai.probe_busy).show(ui).clicked() {
                self.probe_ai(true);
            }
            kit::hint(ui, "Нужен, только чтобы убедиться, что зарубежный IP снимает блокировку по стране. Системный прокси Windows PLASTO не меняет. Поддерживается HTTP CONNECT, не SOCKS.");
        }
    }

    fn probe_ai(&mut self, via_proxy: bool) {
        if via_proxy && self.ai.probe_proxy.trim().is_empty() {
            self.ai.verdict = Some(super::theme::Status::new(super::theme::Tone::Bad, "Сначала укажите адрес прокси"));
            return;
        }
        if via_proxy {
            // Сохраняем синхронно: проба читает адрес из конфига.
            crate::api::call(&self.host, "ai:setProxy", json!(self.ai.probe_proxy));
        }
        self.ai.probe_busy = true;
        self.ai.probe_frac = 0.0;
        self.ai.rows.clear();
        self.ai.rows_note = None;
        self.ai.verdict = None;
        self.call(Op::AiProbe { via_proxy }, "ai:probe", json!({ "useProxy": via_proxy }));
    }

    /* ---------- DNS ------------------------------------------------------ */

    fn dns_detail(&mut self, ui: &mut Ui) {
        title(ui, "DNS", "Защищённый DNS для всех сетевых адаптеров. Прежние настройки сохраняются, а после переключения PLASTO проверяет, что имена разрешаются, и сам вернёт как было, если нет.");
        ui.add_space(8.0);
        if self.dns.presets.is_empty() {
            kit::hint(ui, "Загружаю список…");
            return;
        }
        if self.dns.checked_at.is_none() {
            self.dns_check_all();
        }
        let width = ui.available_width();
        for preset in self.dns.presets.clone() {
            let id = s(&preset["id"]).to_owned();
            let on = self.dns.selected == id;
            let (row, _) = ui.allocate_exact_size(vec2(width, 48.0), Sense::hover());
            let resp = ui.interact(row, ui.id().with(("dns", &id)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let p = ui.painter().clone();
            if on {
                p.rect_filled(row, 12.0, kit::FILL_2);
            } else if resp.hovered() {
                p.rect_filled(row, 12.0, kit::FILL_1);
            }
            let c = pos2(row.left() + 22.0, row.center().y);
            if on {
                p.circle_filled(c, 9.0, kit::OK);
                p.circle_filled(c, 3.5, kit::OK_INK);
            } else {
                p.circle_stroke(c, 8.0, egui::Stroke::new(2.0_f32, kit::HAIR_2));
            }
            let name = p.layout_no_wrap(s(&preset["name"]).to_owned(), kit::semibold(15.0), kit::INK);
            p.galley(pos2(row.left() + 44.0, row.center().y - name.size().y / 2.0), name.clone(), kit::INK);
            let mut used = name.size().x;
            if let Some((text, tone)) = self.dns.health.get(&id).map(dns_badge) {
                let at = Rect::from_min_size(pos2(row.left() + 44.0 + name.size().x + 12.0, row.center().y - 12.0), vec2(140.0, 24.0));
                let mut pill_ui = ui.new_child(egui::UiBuilder::new().id_salt(("dns-pill", &id)).max_rect(at));
                used += 12.0 + kit::pill(&mut pill_ui, &text, tone).rect.width();
            }
            let what = if preset["unblocks"] == true { "снимает блокировку по стране" } else { "только шифрует" };
            let note = super::theme::elided(ui, what, kit::regular(13.0), kit::INK_3, width - 80.0 - used);
            p.galley(pos2(row.right() - 16.0 - note.size().x, row.center().y - note.size().y / 2.0), note, kit::INK_3);
            if resp.clicked() {
                self.dns.selected = id;
            }
        }
        if let Some(p) = self.dns.presets.iter().find(|p| p["id"] == self.dns.selected.as_str()) {
            kit::hint(ui, s(&p["note"]));
        }
        if let Some(h) = self.dns.health.get(&self.dns.selected) {
            kit::hint(ui, &dns_detail_text(h));
        }
        ui.horizontal(|ui| {
            let label = if self.dns.busy_apply { "Переключаю…" } else { "Применить" };
            if Btn::primary(label).enabled(!self.dns.busy_apply && !self.dns.selected.is_empty()).show(ui).clicked() {
                let doing = if self.dns.has_backup { ("переключаю…", "выставляю адреса и проверяю") } else { ("включаю…", "выставляю адреса и проверяю") };
                self.dns_apply(doing);
            }
            if Btn::secondary("Вернуть как было").enabled(self.dns.has_backup && !self.dns.busy_apply).show(ui).clicked() {
                self.dns_restore();
            }
            let label = if self.dns.checking { "Проверяю…" } else { "Проверить все" };
            if Btn::secondary(label).enabled(!self.dns.checking).show(ui).clicked() {
                self.dns_check_all();
            }
        });
        if let Some(st) = self.dns.state.clone() {
            status_line(ui, &st);
        }
        if let Some(at) = self.dns.checked_at {
            kit::hint(ui, &format!("Проверено {}.", super::health::ago(at)));
        }
        let auto = self.dns_auto();
        if kit::checkbox(ui, auto, "Переключать на рабочий DNS автоматически", true).clicked() {
            self.set_config(json!({ "dnsAutoSwitch": !auto }));
            self.dns.fails = 0;
            self.dns_watch_in(std::time::Duration::from_secs(5));
        }
        kit::hint(ui, "Раз в 5 минут PLASTO проверяет выставленный DNS. Если он дважды подряд не ответил или перестал снимать блокировку по стране, PLASTO переключится на самый быстрый рабочий того же рода: снимающий блокировку — на снимающий. Когда пропадает вся сеть, DNS не трогается.");
        kit::hint(ui, "Happ и Hiddify в TUN-режиме перехватывают DNS, и тогда пресет просто не подействует. С hosts конфликта нет: на совпадающих именах выигрывает он.");
        ui.add_space(8.0);

        if kit::disclosure(ui, "dns-custom", "Свой DNS") {
            let width = ui.available_width();
            kit::input(ui, &mut self.dns.custom_name, "Название (необязательно)", width, 1);
            kit::input(ui, &mut self.dns.custom_text, "Адреса как на сайте резолвера: IP, https://…/dns-query, tls://имя", width, 3);
            let is_custom = self.dns.presets.iter().any(|p| p["id"] == self.dns.selected.as_str() && p["custom"] == true);
            ui.horizontal(|ui| {
                let label = if self.dns.busy_add { "Проверяю…" } else { "Добавить" };
                if Btn::primary(label).enabled(!self.dns.busy_add).show(ui).clicked() {
                    let text = self.dns.custom_text.trim().to_owned();
                    if text.is_empty() {
                        self.dns.custom_msg = Some("Вставьте хотя бы один адрес.".into());
                    } else {
                        self.dns.busy_add = true;
                        self.call(Op::DnsAdd, "dns:addCustom", json!({ "name": self.dns.custom_name, "text": text }));
                    }
                }
                if Btn::secondary("Удалить выбранный").enabled(is_custom).show(ui).clicked() {
                    self.call(Op::DnsRemove, "dns:removeCustom", json!(self.dns.selected));
                }
            });
            let msg = self.dns.custom_msg.clone().unwrap_or_else(|| "PLASTO сам найдёт недостающее: IP по имени, шаблон DoH, и проверит, отвечает ли сервер на обычный DNS или только по шифрованному каналу.".into());
            kit::hint(ui, &msg);
        }
    }
}

/// Значок у пресета в списке: задержка или что не так.
fn dns_badge(h: &Value) -> (String, Tone) {
    let ms = h["ms"].as_u64().unwrap_or(0);
    match s(&h["state"]) {
        "ok" => (format!("{ms} мс"), Tone::Ok),
        "slow" => (format!("{ms} мс"), Tone::Warn),
        "noSpoof" => ("не снимает блок".into(), Tone::Warn),
        _ => ("не отвечает".into(), Tone::Bad),
    }
}

/// Подробности проверки выбранного пресета — по каналам.
fn dns_detail_text(h: &Value) -> String {
    let yes_no = |ok: bool| if ok { "отвечает" } else { "не отвечает" };
    let mut parts = vec![format!("Обычный DNS: {}", yes_no(h["plain"] == true))];
    if let Some(doh) = h["doh"].as_bool() {
        parts.push(format!("DoH: {}", yes_no(doh)));
    }
    if let Some(spoofs) = h["spoofs"].as_bool() {
        parts.push(if spoofs { "подменяет адреса — блокировку по стране снимает".into() } else { "отдаёт настоящие адреса — блокировку по стране не снимет".into() });
    }
    format!("{}.", parts.join(" · "))
}

/// Итог пробы ИИ-сервиса: тон и короткая подпись.
fn ai_state(state: &str) -> (Tone, &'static str) {
    match state {
        "ok" => (Tone::Ok, "работает"),
        // Сеть доходит, но пустят ли после входа — проба не знает.
        "reachable" => (Tone::Warn, "сеть доходит"),
        "blocked" => (Tone::Bad, "режет сеть"),
        "geo" => (Tone::Warn, "блок по стране"),
        "partial" => (Tone::Warn, "ответ 403"),
        "hosts-inactive" => (Tone::Bad, "hosts не применён"),
        "proxy-dead" => (Tone::Bad, "прокси не отвечает"),
        "dns" => (Tone::Bad, "DNS"),
        "proxy" => (Tone::Bad, "прокси"),
        _ => (Tone::Warn, "неясно"),
    }
}

fn ai_fix(fix: &str) -> Option<&'static str> {
    Some(match fix {
        "zapret" => "режет провайдер: включите галку «обход» и включите обход",
        "proxy" => "сервер видит ваш IP: включите галку «прокси». Обход этого не лечит, но и не отменяет, оставьте обе",
        "dns" => "имя не разрешается: проверьте DNS. Если включён VPN в TUN-режиме, он перехватывает резолвер",
        "reboot" => "записи в hosts есть, но система их не применила: перезагрузите компьютер",
        "proxy-off" => "прокси из каталога не отвечает: снимите галку «прокси» для этого сервиса и нажмите «Применить»",
        _ => return None,
    })
}

