//! «Диагностика»: скорость, система, подбор стратегии и журнал — всё, куда
//! идут, когда что-то не работает.

use egui::{pos2, vec2, Align2, Rect, ScrollArea, Sense, Stroke, Ui, UiBuilder};
use serde_json::{json, Value};

use super::{
    glass,
    icons::Icon,
    kit::{self, Btn, Tone},
    pages::status_line,
    platform, s, u, App, DiagTab, Op, Live,
};

/// Скорость для таблицы и карточек: «604», «12,4», «0,8».
fn mbps(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}").replace('.', ",")
    }
}

/// Карточка замера: подпись, значение, единица, ряд для графика, цвет.
type Metric<'a> = (&'a str, Option<String>, &'a str, Vec<f64>, egui::Color32);

fn series(v: &Value) -> Vec<f64> {
    v.as_array().into_iter().flatten().filter_map(Value::as_f64).collect()
}

/// Подпись сервера: «Москва (2ip)», без города — просто «Cloudflare».
fn server_name(r: &Value) -> String {
    let (provider, city) = (s(&r["provider"]), s(&r["city"]));
    if city.is_empty() {
        if provider.is_empty() { s(&r["label"]).to_owned() } else { provider.to_owned() }
    } else {
        format!("{city} ({provider})")
    }
}

/// Колонка внутри панели: дочерний `Ui` в заданном прямоугольнике.
pub(super) fn column(ui: &mut Ui, rect: Rect, id: &str) -> Ui {
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)).id_salt(id));
    child.spacing_mut().item_spacing = vec2(12.0, 12.0);
    child
}

impl App {
    pub(super) fn diag_page(&mut self, ui: &mut Ui, rect: Rect) {
        glass::panel(ui, 20, rect, 16.0, self.gpu);
        let inner = rect.shrink2(vec2(28.0, 24.0));
        let mut top = column(ui, Rect::from_min_size(inner.min, vec2(inner.width(), 40.0)), "diag-tabs");
        let tabs = [DiagTab::Speed, DiagTab::Stability, DiagTab::System, DiagTab::Tests, DiagTab::Log];
        let at = tabs.iter().position(|t| *t == self.diag_tab).unwrap_or(0);
        if let Some(i) = kit::segmented(&mut top, "diag-tab", &["Скорость", "Стабильность", "Система", "Подбор стратегии", "Журнал"], at) {
            self.diag_tab = tabs[i];
        }
        let body = Rect::from_min_max(pos2(inner.left(), inner.top() + 40.0 + 20.0), inner.max);
        match self.diag_tab {
            DiagTab::Speed => self.speed_tab(ui, body),
            DiagTab::Stability => self.stab_tab(ui, body),
            DiagTab::System => self.system_tab(ui, body),
            DiagTab::Tests => self.tests_tab(ui, body),
            DiagTab::Log => self.log_tab(ui, body),
        }
    }

    /* ---------- Скорость ------------------------------------------------- */

    fn start_speed(&mut self) {
        self.speed.running = true;
        self.speed.frac = 0.0;
        self.speed.stage = "Ищу ближайшие серверы".into();
        self.speed.rows.clear();
        self.speed.result = None;
        self.speed.note = None;
        self.speed.live = 0.0;
        self.speed.phase = "reach".into();
        self.speed.server.clear();
        self.speed.live_series.clear();
        self.call(Op::Speed, "speed:run", Value::Null);
    }

    /// Лучшее по региону из строк замера: загрузка (с пингом и рядами) и отдача.
    fn best(&self, region: &str) -> (Option<&Value>, Option<&Value>) {
        let rows = self.speed.rows.iter().filter(|r| r["region"] == region);
        let down = rows.clone().filter(|r| r["down"].is_number()).max_by(|a, b| a["down"].as_f64().unwrap_or(0.0).total_cmp(&b["down"].as_f64().unwrap_or(0.0)));
        let up = rows.filter(|r| r["up"].is_number()).max_by(|a, b| a["up"].as_f64().unwrap_or(0.0).total_cmp(&b["up"].as_f64().unwrap_or(0.0)));
        (down, up)
    }

    fn speed_tab(&mut self, ui: &mut Ui, body: Rect) {
        let left = Rect::from_min_size(body.min, vec2(300.0, body.height()));
        let right = Rect::from_min_max(pos2(left.right() + 24.0, body.top()), body.max);
        let running = self.speed.running;
        let upload = self.speed.phase == "up";

        // Слева: шкала, этап, кнопка, живой график.
        let mut col = column(ui, left, "speed-left");
        let ui_l = &mut col;
        // Подпись этапа лежит под дугой внутри блока шкалы: так колонка вместе
        // с живым графиком влезает в высоту панели.
        // Концы дуги с толщиной линии кончаются на ~201 px от верха блока:
        // подпись ниже них, иначе длинный текст наезжает на дугу.
        let (g, _) = ui_l.allocate_exact_size(vec2(300.0, 226.0), Sense::hover());
        let gauge = Rect::from_center_size(pos2(g.center().x, g.top() + 112.0), vec2(260.0, 223.0));
        let shown = if running {
            self.speed.live
        } else {
            self.best("ru").0.and_then(|r| r["down"].as_f64()).unwrap_or(0.0)
        };
        kit::gauge(ui_l.painter(), gauge, kit::gauge_frac(shown), running && upload);
        kit::gauge_ticks(ui_l.painter(), gauge);
        let k = gauge.width() / 280.0;
        let num = if running && self.speed.phase == "reach" { "…".to_owned() } else { mbps(shown) };
        ui_l.painter().text(gauge.min + vec2(140.0, 118.0) * k, Align2::CENTER_CENTER, num, kit::semibold(52.0 * k), kit::INK);
        let unit = if running && upload { "Мбит/с, отдача" } else if running { "Мбит/с, загрузка" } else { "Мбит/с" };
        ui_l.painter().text(gauge.min + vec2(140.0, 158.0) * k, Align2::CENTER_CENTER, unit, kit::regular(14.0), kit::INK_2);
        let phase = if running {
            match self.speed.phase.as_str() {
                "reach" => "Ищу ближайшие серверы".to_owned(),
                "down" => format!("Загрузка: {}", self.speed.server),
                _ => format!("Отдача: {}", self.speed.server),
            }
        } else if let Some(r) = &self.speed.result {
            format!("Замер завершён за {} с", u(&r["durationMs"]) / 1000)
        } else {
            "Замер не влияет на работу обхода".to_owned()
        };
        ui_l.painter().text(pos2(g.center().x, g.bottom() - 4.0), Align2::CENTER_BOTTOM, &phase, kit::regular(14.0), kit::INK_2);
        if running {
            ui_l.horizontal(|ui| {
                kit::progress(ui, self.speed.frac, 240.0);
                kit::text(ui, &format!("{}%", (self.speed.frac * 100.0).round()), kit::regular(13.0), kit::INK_3);
            });
        }
        ui_l.vertical_centered(|ui| {
            let busy_tests = self.tests.running;
            if running {
                if Btn::primary("Остановить").icon(Icon::Stop).big().show(ui).clicked() {
                    self.call(Op::Ignore, "speed:cancel", Value::Null);
                }
            } else {
                let label = if self.speed.result.is_some() { "Замерить ещё раз" } else { "Начать замер" };
                if Btn::primary(label).icon(Icon::Play).big().enabled(!busy_tests).show(ui).clicked() {
                    self.start_speed();
                }
                if busy_tests {
                    kit::hint(ui, "Замер скорости недоступен, пока идёт подбор стратегии: замер забивает канал, и подбор забраковал бы рабочие стратегии.");
                }
            }
        });
        if running && !self.speed.live_series.is_empty() {
            let peak = self.speed.live_series.iter().cloned().fold(0.0, f64::max);
            // Строка над графиком: что меряем слева, пик справа.
            ui_l.spacing_mut().item_spacing.y = 6.0;
            let (h, _) = ui_l.allocate_exact_size(vec2(300.0, 16.0), Sense::hover());
            let p = ui_l.painter();
            let pk = p.layout_no_wrap(format!("пик {} Мбит/с", mbps(peak)), kit::regular(12.0), kit::INK_3);
            let pw = pk.size().x;
            p.galley(pos2(h.right() - pw, h.center().y - pk.size().y / 2.0), pk, kit::INK_3);
            let what = super::theme::elided(ui_l, &phase, kit::regular(12.0), kit::INK_3, h.width() - pw - 12.0);
            ui_l.painter().galley(pos2(h.left(), h.center().y - what.size().y / 2.0), what, kit::INK_3);
            let (r, _) = ui_l.allocate_exact_size(vec2(300.0, ui_l.available_height().clamp(24.0, 44.0)), Sense::hover());
            kit::sparkline(ui_l.painter(), r, &self.speed.live_series, if upload { kit::UP } else { kit::OK });
        }
        if let Some((note, _)) = &self.speed.note {
            kit::hint(ui_l, note);
        }

        // Справа: два региона, таблица серверов, вывод.
        let cards = Rect::from_min_size(right.min, vec2(right.width(), 104.0));
        let half = (cards.width() - 24.0) / 2.0;
        for (i, (title, region)) in [("Россия", "ru"), ("За рубежом", "abroad")].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(cards.left() + i as f32 * (half + 24.0), cards.top()), vec2(half, cards.height()));
            if i == 1 {
                ui.painter().line_segment([pos2(r.left() - 12.0, r.top()), pos2(r.left() - 12.0, r.bottom())], Stroke::new(1.0_f32, kit::HAIR));
            }
            self.region_card(ui, r, title, region);
        }
        ui.painter().line_segment([pos2(right.left(), cards.bottom() + 8.0), pos2(right.right(), cards.bottom() + 8.0)], Stroke::new(1.0_f32, kit::HAIR));

        let verdict = self.speed_verdict();
        let verdict_h = if verdict.is_some() { 64.0 } else { 0.0 };
        let table = Rect::from_min_max(pos2(right.left(), cards.bottom() + 20.0), pos2(right.right(), right.bottom() - verdict_h));
        self.speed_table(ui, table);
        if let Some((head, text)) = verdict {
            let mut v = column(ui, Rect::from_min_max(pos2(right.left(), table.bottom() + 12.0), right.max), "speed-verdict");
            v.spacing_mut().item_spacing.y = 4.0;
            kit::text(&mut v, &head, kit::semibold(14.0), kit::INK);
            kit::hint(&mut v, &text);
        }
    }

    fn region_card(&self, ui: &mut Ui, r: Rect, title: &str, region: &str) {
        let p = ui.painter().clone();
        let (down, up) = self.best(region);
        p.text(r.left_top(), Align2::LEFT_TOP, title, kit::semibold(17.0), kit::INK);
        let server = down.map(server_name).unwrap_or_else(|| "ещё не мерили".into());
        p.text(pos2(r.right(), r.top() + 4.0), Align2::RIGHT_TOP, server, kit::regular(13.0), kit::INK_3);
        let w = r.width() / 3.0;
        let metrics: [Metric; 3] = [
            ("Загрузка", down.and_then(|d| d["down"].as_f64()).map(mbps), "Мбит/с", down.map(|d| series(&d["downSeries"])).unwrap_or_default(), kit::OK),
            ("Отдача", up.and_then(|d| d["up"].as_f64()).map(mbps), "Мбит/с", up.map(|d| series(&d["upSeries"])).unwrap_or_default(), kit::UP),
            (
                "Пинг",
                down.and_then(|d| d["ping"].as_f64()).map(|v| format!("{v:.0}")),
                "мс",
                down.map(|d| [series(&d["pingSeries"]), series(&d["loadedSeries"])].concat()).unwrap_or_default(),
                kit::PING,
            ),
        ];
        for (i, (name, value, unit, data, color)) in metrics.into_iter().enumerate() {
            let x = r.left() + i as f32 * w;
            p.text(pos2(x, r.top() + 28.0), Align2::LEFT_TOP, name, kit::regular(13.0), kit::INK_3);
            match value {
                Some(v) => {
                    let g = p.layout_no_wrap(v, kit::semibold(22.0), kit::INK);
                    let vw = g.size().x;
                    p.galley(pos2(x, r.top() + 46.0), g, kit::INK);
                    p.text(pos2(x + vw + 4.0, r.top() + 60.0), Align2::LEFT_CENTER, unit, kit::regular(12.0), kit::INK_3);
                }
                None => {
                    p.text(pos2(x, r.top() + 46.0), Align2::LEFT_TOP, "нет", kit::semibold(22.0), kit::INK_4);
                }
            }
            kit::sparkline(&p, Rect::from_min_size(pos2(x, r.top() + 76.0), vec2(w - 16.0, 26.0)), &data, color);
        }
    }

    fn speed_table(&mut self, ui: &mut Ui, table: Rect) {
        let cols = [("Сервер", 0.0), ("Пинг", 84.0), ("Под нагрузкой", 120.0), ("Загрузка", 96.0), ("Отдача", 88.0)];
        let fixed: f32 = cols[1..].iter().map(|c| c.1).sum();
        let first = table.width() - fixed - 16.0;
        let head = Rect::from_min_size(table.min, vec2(table.width(), 32.0));
        let p = ui.painter().clone();
        p.rect_filled(head, 8.0, kit::FILL_1);
        let mut x = head.left() + 8.0;
        for (i, (name, w)) in cols.iter().enumerate() {
            let w = if i == 0 { first } else { *w };
            let (pos, align) = if i == 0 { (pos2(x, head.center().y), Align2::LEFT_CENTER) } else { (pos2(x + w, head.center().y), Align2::RIGHT_CENTER) };
            p.text(pos, align, *name, kit::regular(13.0), kit::INK_3);
            x += w;
        }
        let rows_rect = Rect::from_min_max(pos2(table.left(), head.bottom() + 4.0), table.max);
        let mut col = column(ui, rows_rect, "speed-rows");
        let current = (self.speed.running, self.speed.phase.clone(), self.speed.server.clone());
        ScrollArea::vertical().id_salt("speed-rows-scroll").auto_shrink([false, false]).show(&mut col, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if self.speed.rows.is_empty() {
                kit::hint(ui, if self.speed.running { "Ищу серверы, которые отвечают…" } else { "Здесь будут серверы в России и за рубежом: Яндекс, Selectel, 2ip, Cloudflare, Vultr, OVH, Hetzner." });
            }
            for r in &self.speed.rows {
                let (row, _) = ui.allocate_exact_size(vec2(table.width() - 10.0, 32.0), Sense::hover());
                let p = ui.painter();
                let live = current.0 && (current.1 == "down" || current.1 == "up") && s(&r["label"]) == current.2;
                if live {
                    p.rect_filled(row, 8.0, kit::OK_SOFT);
                }
                let mut x = row.left() + 8.0;
                // Страна.
                let cc = s(&r["country"]);
                if !cc.is_empty() {
                    let badge = Rect::from_min_size(pos2(x, row.center().y - 10.0), vec2(28.0, 20.0));
                    p.rect_filled(badge, 4.0, kit::FILL_2);
                    p.text(badge.center(), Align2::CENTER_CENTER, cc, kit::semibold(12.0), kit::INK_2);
                }
                let name = super::theme::elided(ui, &server_name(r), kit::regular(14.0), kit::INK, first - 44.0);
                p.galley(pos2(x + 36.0, row.center().y - name.size().y / 2.0), name, kit::INK);
                x += first;
                let ping = match (r["ping"].as_f64(), r["jitter"].as_f64()) {
                    (Some(pg), Some(j)) => format!("{pg:.0} ±{}", format!("{j:.1}").replace('.', ",")),
                    (Some(pg), None) => format!("{pg:.0}"),
                    _ => String::new(),
                };
                let cells = [
                    (ping, 84.0, kit::INK),
                    (r["loaded"].as_f64().map(|v| format!("{v:.0}")).unwrap_or_default(), 120.0, kit::INK),
                    (
                        if live && current.1 == "down" { mbps(self.speed.live) } else if r["downError"].is_string() { "нет данных".into() } else { r["down"].as_f64().map(mbps).unwrap_or_default() },
                        96.0,
                        if live && current.1 == "down" { kit::OK_TEXT } else if r["downError"].is_string() { kit::BAD_TEXT } else if r["down"].as_f64().is_some_and(|v| v < 30.0) { kit::WARN_TEXT } else { kit::INK },
                    ),
                    (
                        if live && current.1 == "up" { mbps(self.speed.live) } else if r["upError"].is_string() { "нет данных".into() } else { r["up"].as_f64().map(mbps).unwrap_or_default() },
                        88.0,
                        if live && current.1 == "up" { kit::OK_TEXT } else if r["upError"].is_string() { kit::BAD_TEXT } else { kit::INK },
                    ),
                ];
                for (text, w, color) in cells {
                    p.text(pos2(x + w, row.center().y), Align2::RIGHT_CENTER, text, kit::regular(14.0), color);
                    x += w;
                }
                let resp = ui.interact(row, ui.id().with(("srv", s(&r["label"]))), Sense::hover());
                if let Some(e) = r["downError"].as_str() {
                    resp.on_hover_text(e);
                }
            }
        });
    }

    /// Вывод после замера: во сколько раз за рубежом медленнее и кто не отдаёт данные.
    fn speed_verdict(&self) -> Option<(String, String)> {
        let r = self.speed.result.as_ref()?;
        let ru = self.best("ru").0.and_then(|v| v["down"].as_f64());
        let ab = self.best("abroad").0.and_then(|v| v["down"].as_f64());
        let dead: Vec<String> = self.speed.rows.iter().filter(|r| r["downError"].is_string() || r["down"].as_f64().is_some_and(|v| v < 30.0)).map(server_name).collect();
        let mut quiet: Vec<String> = r["silent"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        let (alive, total) = (u(&r["twoip"]["alive"]), u(&r["twoip"]["total"]));
        if alive < total {
            quiet.push(format!("2ip: {} из {total}", total - alive));
        }
        let head = match (ru, ab) {
            (Some(a), Some(b)) if a > b * 1.5 => format!("За рубежом в {} раза медленнее", format!("{:.1}", a / b).replace('.', ",")),
            (Some(_), Some(_)) => "Скорость в России и за рубежом сопоставима".into(),
            _ => "Замер завершён не полностью".into(),
        };
        let mut text = Vec::new();
        if !dead.is_empty() {
            text.push(format!("{} почти не отдают данные. Так выглядит замедление зарубежных хостеров.", dead.join(", ")));
        }
        if !quiet.is_empty() {
            text.push(format!("Не ответили: {}.", quiet.join(", ")));
        }
        Some((head, text.join(" ")))
    }

    /* ---------- Система -------------------------------------------------- */

    pub(super) fn run_diag(&mut self) {
        self.diag.busy = Some("Проверяю…".into());
        self.diag.rows.clear();
        self.diag.verdict = None;
        self.diag.failed = false;
        self.call(Op::Diag, "diag:run", Value::Null);
    }

    fn system_tab(&mut self, ui: &mut Ui, body: Rect) {
        // Первый раз проверка идёт сама, без кнопки.
        if !self.diag.auto {
            self.diag.auto = true;
            if self.diag.busy.is_none() && self.diag.rows.is_empty() {
                self.run_diag();
            }
        }
        let aside_w = 300.0;
        let list = Rect::from_min_max(body.min, pos2(body.right() - aside_w - 25.0, body.bottom()));
        let aside = Rect::from_min_max(pos2(body.right() - aside_w, body.top()), body.max);
        ui.painter().line_segment([pos2(aside.left() - 12.0, aside.top()), pos2(aside.left() - 12.0, aside.bottom())], Stroke::new(1.0_f32, kit::HAIR));

        let mut col = column(ui, list, "diag-list");
        ScrollArea::vertical().id_salt("diag-scroll").auto_shrink([false, false]).show(&mut col, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.diag.rows.is_empty() {
                kit::hint(ui, if self.diag.busy.is_some() { "Проверяю…" } else { "Проверка ещё не запускалась. Она смотрит то, что мешает обходу: права, WinDivert, конфликтующие программы, системный прокси, защищённый DNS, hosts." });
            }
            for r in &self.diag.rows {
                let (tone, word) = match s(&r["level"]) {
                    "ok" => (Tone::Ok, "в порядке"),
                    "fail" => (Tone::Bad, "проблема"),
                    _ => (Tone::Warn, "внимание"),
                };
                let width = ui.available_width() - 10.0;
                let msg = ui.painter().layout(s(&r["message"]).to_owned(), kit::regular(13.0), kit::INK_3, width - 160.0);
                let h = (22.0 + msg.size().y + 16.0).max(44.0);
                let (row, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
                let p = ui.painter();
                kit::dot(p, pos2(row.left() + 8.0, row.top() + 19.0), tone, 4.0);
                p.text(pos2(row.left() + 24.0, row.top() + 10.0), Align2::LEFT_TOP, s(&r["title"]), kit::regular(15.0), kit::INK);
                p.galley(pos2(row.left() + 24.0, row.top() + 30.0), msg, kit::INK_3);
                let (bg, fg) = match tone {
                    Tone::Ok => (kit::OK_SOFT, kit::OK_TEXT),
                    Tone::Bad => (kit::BAD_SOFT, kit::BAD_TEXT),
                    _ => (kit::WARN_SOFT, kit::WARN_TEXT),
                };
                let g = p.layout_no_wrap(word.to_owned(), kit::regular(13.0), fg);
                let pill = Rect::from_min_size(pos2(row.right() - g.size().x - 24.0, row.top() + 8.0), g.size() + vec2(24.0, 8.0));
                p.rect_filled(pill, pill.height() / 2.0, bg);
                p.galley(pill.min + vec2(12.0, 4.0), g, fg);
                p.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0_f32, kit::HAIR));
            }
        });

        let mut a = column(ui, aside, "diag-aside");
        let problems: Vec<&Value> = self.diag.rows.iter().filter(|r| r["level"] != "ok").collect();
        if self.diag.failed {
            kit::text(&mut a, "Проверка не выполнилась", kit::semibold(17.0), kit::INK);
        } else if let Some(v) = self.diag.verdict.clone() {
            let head = match problems.len() {
                0 => "Всё в порядке".to_owned(),
                n => format!("Найдено проблем: {n}"),
            };
            kit::text(&mut a, &head, kit::semibold(17.0), kit::INK);
            status_line(&mut a, &v);
        }
        // Что можно исправить отсюда же: старая служба и забытый драйвер.
        let mut action = None;
        for r in problems {
            let (label, call, hint) = match s(&r["id"]) {
                "oldservice" => ("Удалить старую службу", "service:remove", "Служба из старой версии сама запускает обход. После удаления включать его будет приложение."),
                "windivert" => ("Снять драйвер WinDivert", "service:dropDriver", "Помогает, если обход не включается после другой программы на WinDivert. zapret зарегистрирует свой драйвер заново."),
                _ => continue,
            };
            a.add_space(4.0);
            kit::hint(&mut a, hint);
            let off = self.state != "on" && self.state != "timeout";
            let busy = self.svc_busy || self.diag.busy.is_some();
            if Btn::secondary(label).enabled(!busy && (off || call == "service:remove")).show(&mut a).clicked() {
                action = Some(call);
            }
        }
        if let Some(call) = action {
            self.svc_busy = true;
            self.call(Op::Service, call, Value::Null);
        }
        let label = self.diag.busy.clone().unwrap_or_else(|| if self.diag.rows.is_empty() { "Проверить систему".into() } else { "Проверить заново".into() });
        if Btn::primary(&label).enabled(self.diag.busy.is_none()).show(&mut a).clicked() {
            self.run_diag();
        }
        a.add_space(4.0);
        load_meters(&mut a);
    }

    /* ---------- Подбор стратегии ---------------------------------------- */

    pub(super) fn start_tests(&mut self, only: Option<Vec<String>>) {
        self.tests.running = true;
        self.tests.rows.clear();
        self.tests.frac = 0.0;
        self.tests.live = Default::default();
        self.tests.note = Some(("Идёт проверка…".into(), false));
        self.tests.stage = "Проверяю, что доступно без обхода".into();
        // Карточка показывает подбор; настоящее состояние вернётся по окончании.
        self.tests.resume = Some((self.state.clone(), self.reason.clone()));
        self.render_state("testing", None);
        self.call(Op::TestRun, "test:run", json!({ "only": only, "stopOnPerfect": self.tests.stop_perfect }));
    }

    fn tests_tab(&mut self, ui: &mut Ui, body: Rect) {
        let aside_w = 300.0;
        let list = Rect::from_min_max(body.min, pos2(body.right() - aside_w - 25.0, body.bottom()));
        let aside = Rect::from_min_max(pos2(body.right() - aside_w, body.top()), body.max);
        ui.painter().line_segment([pos2(aside.left() - 12.0, aside.top()), pos2(aside.left() - 12.0, aside.bottom())], Stroke::new(1.0_f32, kit::HAIR));

        let mut col = column(ui, list, "tests-list");
        if self.tests.running {
            // Полоса на всю ширину, этап — строкой ниже с обрезкой: длинное
            // имя стратегии не должно распирать колонку.
            let w = col.available_width();
            kit::progress(&mut col, self.tests.frac, w);
            let stage = super::theme::elided(&col, &self.tests.stage, kit::regular(13.0), kit::INK_3, w);
            let (r, _) = col.allocate_exact_size(stage.size(), Sense::hover());
            col.painter().galley(r.min, stage, kit::INK_3);
            live_row(&mut col, &self.tests.live, self.tests.frac);
        }
        if let Some(b) = self.tests.baseline.clone() {
            status_line(&mut col, &b);
        }
        if let Some((note, _)) = &self.tests.note {
            kit::hint(&mut col, note);
        }
        let current = self.cfg_str("strategy").unwrap_or("").to_owned();
        let mut picked = None;
        ScrollArea::vertical().id_salt("tests-scroll").auto_shrink([false, false]).show(&mut col, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, r) in self.tests.rows.iter().enumerate() {
                let ok = r["ok"] == true;
                let score = u(&r["score"]);
                let width = ui.available_width() - 10.0;
                let (row, _) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
                let resp = ui.interact(row, ui.id().with(("test", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Применить эту стратегию");
                let p = ui.painter();
                if resp.hovered() {
                    p.rect_filled(row, 8.0, kit::FILL_1);
                }
                let is_current = s(&r["preset"]) == current;
                let name = p.layout_no_wrap(s(&r["label"]).to_owned(), kit::semibold(15.0), if ok { kit::INK } else { kit::INK_3 });
                let name_w = name.size().x;
                p.galley(pos2(row.left() + 8.0, row.top() + 8.0), name, kit::INK);
                if is_current {
                    p.text(pos2(row.left() + 8.0 + name_w + 10.0, row.top() + 16.0), Align2::LEFT_CENTER, "текущая", kit::regular(13.0), kit::OK_TEXT);
                }
                let sub = if ok {
                    let mut groups = r["groups"].as_object().map(|g| g.iter().map(|(k, v)| format!("{k} {}/{}", u(&v["ok"]), u(&v["total"]))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    if r["medianMs"].is_number() {
                        groups += &format!(", медиана {} мс", u(&r["medianMs"]));
                    }
                    groups
                } else {
                    r["error"].as_str().unwrap_or("не запустилась").to_owned()
                };
                p.text(pos2(row.left() + 8.0, row.top() + 32.0), Align2::LEFT_TOP, sub, kit::regular(13.0), kit::INK_3);
                let bar = Rect::from_min_size(pos2(row.right() - 64.0 - 160.0, row.center().y - 3.0), vec2(150.0, 6.0));
                p.rect_filled(bar, 3.0, kit::TRACK);
                let fill = score as f32 / 100.0;
                let tone_color = if score >= 80 { kit::OK } else if score >= 40 { kit::WARN } else { kit::BAD };
                if fill > 0.0 {
                    p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * fill, bar.height())), 3.0, tone_color);
                }
                p.text(pos2(row.right() - 8.0, row.center().y), Align2::RIGHT_CENTER, format!("{score}%"), kit::semibold(15.0), kit::INK);
                p.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0_f32, kit::HAIR));
                if resp.clicked() && ok {
                    picked = Some(s(&r["preset"]).to_owned());
                }
            }
        });
        if let Some(file) = picked.filter(|f| !f.is_empty()) {
            self.save_strategy(file, true);
        }

        let mut a = column(ui, aside, "tests-aside");
        kit::hint(&mut a, "Прогоняет стратегии по Discord, YouTube, Google и Cloudflare и показывает, какая работает у вашего провайдера. Клик по строке применяет стратегию.");
        let free = !self.tests.running && !self.speed.running;
        if Btn::primary("Проверить все").enabled(free).show(&mut a).clicked() {
            self.start_tests(None);
        }
        if Btn::secondary("Только текущую").enabled(free && !self.sel_strategy.is_empty()).show(&mut a).clicked() {
            let file = self.sel_strategy.clone();
            self.start_tests(Some(vec![file]));
        }
        if kit::checkbox(&mut a, self.tests.stop_perfect, "Остановиться на первой со 100%", !self.tests.running).clicked() {
            self.tests.stop_perfect = !self.tests.stop_perfect;
        }
        if self.tests.running && Btn::secondary("Остановить").show(&mut a).clicked() {
            self.call(Op::Ignore, "test:cancel", Value::Null);
        }
        if self.speed.running {
            kit::hint(&mut a, "Подбор недоступен, пока идёт замер скорости: замер забивает канал, и рабочие стратегии выглядели бы нерабочими.");
        }
        kit::hint(&mut a, "Стратегии проверяются в стороне: обход на время подбора работает как работал. Если какую-то стратегию так не проверить, обход выключится и в конце вернётся как было.");
    }

    /* ---------- Журнал --------------------------------------------------- */

    fn log_tab(&mut self, ui: &mut Ui, body: Rect) {
        let mut top = column(ui, Rect::from_min_size(body.min, vec2(body.width(), 32.0)), "log-top");
        let mut copy = false;
        let mut clear = false;
        top.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            kit::text(ui, "Показать", kit::regular(13.0), kit::INK_3);
            if kit::filter(ui, "Всё", !self.log_errors).clicked() {
                self.log_errors = false;
            }
            if kit::filter(ui, "Ошибки", self.log_errors).clicked() {
                self.log_errors = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let copied = self.copied.is_some_and(|(t, _)| t.elapsed().as_millis() < 1600);
                copy = kit::filter(ui, if copied { "Скопировано" } else { "Скопировать" }, false).clicked();
                clear = kit::filter(ui, "Очистить", false).clicked();
            });
        });
        let well = Rect::from_min_max(pos2(body.left(), body.top() + 44.0), body.max);
        ui.painter().rect_filled(well, 12.0, kit::WELL);
        let mut col = column(ui, well.shrink2(vec2(16.0, 12.0)), "log-well");
        let errors = self.log_errors;
        let entries: Vec<&Value> = self.logs.iter().filter(|e| !errors || matches!(s(&e["level"]), "error" | "err" | "warn")).collect();
        ScrollArea::vertical().id_salt("log-scroll").stick_to_bottom(true).auto_shrink([false, false]).show(&mut col, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if entries.is_empty() {
                kit::hint(ui, if errors { "Ошибок нет." } else { "Журнал пуст. Здесь появятся сообщения обхода и Telegram-прокси." });
            }
            let width = ui.available_width() - 10.0;
            for e in &entries {
                let color = match s(&e["level"]) {
                    "error" | "err" => kit::BAD_TEXT,
                    "warn" => kit::WARN_TEXT,
                    "debug" => kit::INK_3,
                    _ => kit::INK,
                };
                let time = platform::local_time(u(&e["ts"]));
                let msg = ui.painter().layout(s(&e["message"]).to_owned(), kit::regular(13.0), color, width - 84.0);
                let (row, _) = ui.allocate_exact_size(vec2(width, msg.size().y), Sense::hover());
                ui.painter().text(row.left_top(), Align2::LEFT_TOP, time, kit::regular(13.0), kit::INK_3);
                ui.painter().galley(pos2(row.left() + 84.0, row.top()), msg, color);
            }
        });
        if copy {
            let text = self.logs.iter().map(|e| format!("{} {}", platform::local_time(u(&e["ts"])), s(&e["message"]))).collect::<Vec<_>>().join("\n");
            ui.ctx().copy_text(text);
            self.copied = Some((std::time::Instant::now(), true));
        }
        if clear {
            self.logs.clear();
            self.call(Op::Ignore, "engine:clearLogs", Value::Null);
        }
    }
}

/// Загрузка процессора, памяти, видеокарты и системного диска: строка с
/// числом и полоска под ней (у диска — активность, место — числом). Обновляется раз в секунду, пока вкладка открыта.
fn load_meters(ui: &mut Ui) {
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    let Some(l) = super::sysload::current() else {
        kit::hint(ui, "Замеряю загрузку системы…");
        return;
    };
    let gb = |b: u64| format!("{:.1}", b as f64 / (1u64 << 30) as f64).replace('.', ",");
    let pct = |v: f64| format!("{v:.0} %");
    let mut rows: Vec<(String, String, Option<f64>)> = vec![
        ("Процессор".into(), l.cpu.map_or("—".into(), pct), l.cpu),
        match l.ram {
            Some((used, total)) if total > 0 => ("Память".into(), format!("{} из {} ГБ", gb(used), gb(total)), Some(used as f64 * 100.0 / total as f64)),
            _ => ("Память".into(), "—".into(), None),
        },
        ("Видеокарта".into(), l.gpu.map_or("—".into(), pct), l.gpu),
    ];
    if let Some((drive, free, total)) = l.disk.filter(|d| d.2 > 0) {
        let busy = l.disk_busy.map_or(String::new(), |b| format!("{} · ", pct(b)));
        rows.push((format!("Диск {drive}"), format!("{busy}свободно {} из {} ГБ", gb(free), gb(total)), l.disk_busy));
    }
    let width = ui.available_width();
    ui.spacing_mut().item_spacing.y = 4.0;
    for (name, value, frac) in rows {
        let (r, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
        let p = ui.painter();
        p.text(r.left_center(), Align2::LEFT_CENTER, name, kit::regular(14.0), kit::INK);
        p.text(r.right_center(), Align2::RIGHT_CENTER, value, kit::regular(13.0), kit::INK_3);
        kit::progress(ui, frac.unwrap_or(0.0) as f32 / 100.0, width);
        ui.add_space(6.0);
    }
}

/// Строка «сейчас проверяется»: стратегия, её место в очереди, сколько
/// сайтов ещё ждут ответа и по точке на сайт — зелёная открылся, красная
/// нет, светлая — проверяется сейчас.
fn live_row(ui: &mut Ui, live: &Live, frac: f32) {
    ui.add_space(8.0);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(row, 8.0, kit::FILL_1);
    let step = 14.0;
    let dots_w = step * live.targets.max(1) as f32;
    let right = row.right() - 12.0;
    let count = format!("{}/{}", live.done(), live.targets);
    let count_g = p.layout_no_wrap(count, kit::semibold(15.0), kit::INK);
    let dots_left = right - count_g.size().x - 16.0 - dots_w;
    let text_w = dots_left - row.left() - 24.0;
    let name = if live.label.is_empty() { "Без обхода" } else { live.label.as_str() };
    let name = super::theme::elided(ui, name, kit::semibold(15.0), kit::INK, text_w);
    p.galley(pos2(row.left() + 12.0, row.top() + 10.0), name, kit::INK);
    let place = if live.label.is_empty() { "замер, что открывается без обхода".to_owned() } else { format!("стратегия {} из {}", live.index + 1, live.count.max(1)) };
    // Ждут ответа: начаты и без итога. Повторные попытки называем по имени.
    let waiting: Vec<usize> = (0..live.targets).filter(|&k| live.tries[k] > 0 && live.marks[k].is_none()).collect();
    let again: Vec<&str> = waiting.iter().filter(|&&k| live.tries[k] > 1).map(|&k| live.hosts[k].as_str()).collect();
    let sub = match waiting.as_slice() {
        _ if live.targets > 0 && live.done() >= live.targets => format!("{place} · {}%", (frac * 100.0) as u32),
        [] => format!("{place} · запускаю winws…"),
        [k] if live.tries[*k] > 1 => format!("{place} · {}, попытка {}", live.hosts[*k], live.tries[*k]),
        [k] => format!("{place} · {}", live.hosts[*k]),
        many if again.is_empty() => format!("{place} · жду ответа от {} {}", many.len(), sites(many.len())),
        many => format!("{place} · жду ответа от {} {}, повторно: {}", many.len(), sites(many.len()), again.join(", ")),
    };
    let sub = super::theme::elided(ui, &sub, kit::regular(13.0), kit::INK_3, text_w);
    p.galley(pos2(row.left() + 12.0, row.top() + 36.0), sub, kit::INK_3);
    for k in 0..live.targets {
        let color = match live.marks[k] {
            Some(true) => kit::OK,
            Some(false) => kit::BAD,
            None if live.tries[k] > 0 => kit::INK_2,
            None => kit::TRACK,
        };
        p.circle_filled(pos2(dots_left + step * (k as f32 + 0.5), row.center().y), 4.0, color);
    }
    p.galley(pos2(right - count_g.size().x, row.center().y - count_g.size().y / 2.0), count_g, kit::INK);
    ui.add_space(8.0);
}

/// «сайта» или «сайтов» после «от N».
fn sites(n: usize) -> &'static str {
    if n % 10 == 1 && n % 100 != 11 { "сайта" } else { "сайтов" }
}
