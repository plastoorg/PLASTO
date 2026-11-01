//! «Диагностика → Стабильность»: мониторинг связи — что было, где рвалось и
//! что с Wi-Fi, адаптером и драйвером. Данные копит бэкенд (`stability`),
//! страница только спрашивает сводку за выбранный период и рисует её.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, Pos2, Rect, ScrollArea, Sense, Stroke, Ui};
use serde_json::{json, Value};

use super::{
    diag::column,
    icons::Icon,
    kit::{self, Btn, Tone},
    platform, s, App, DiagTab, Op, Page,
};

/// Фильтры времени: ключ для бэкенда и подпись.
pub const RANGES: [(&str, &str); 7] = [("5m", "5 мин"), ("15m", "15 мин"), ("1h", "Час"), ("1d", "День"), ("1w", "Неделя"), ("1mo", "Месяц"), ("1y", "Год")];

#[derive(Default)]
pub struct Stab {
    pub running: bool,
    pub range: usize,
    /// Последний ответ `stab:query`.
    pub data: Option<Value>,
    pub busy: bool,
    pub due: Option<Instant>,
    /// Секунда сборщика: состояние «прямо сейчас».
    pub live: Option<Value>,
    pub live_due: Option<Instant>,
    pub live_busy: bool,
    pub note: Option<String>,
    /// «Очистить историю» нажата один раз: второе нажатие в течение 4 с — очистка.
    pub clear_armed: Option<Instant>,
    pub export_busy: bool,
    /// Итог сохранения отчёта: текст и ошибка ли это.
    pub export_note: Option<(String, bool)>,
}

impl Stab {
    pub fn new() -> Self {
        // По умолчанию — час: и свежие секунды видны, и есть с чем сравнить.
        Stab { range: 2, ..Default::default() }
    }

    /// Как часто перечитывать: короткие периоды — почти вживую.
    fn every(&self) -> Duration {
        Duration::from_secs(match self.range {
            0 | 1 => 2,
            2 => 5,
            3 | 4 => 30,
            _ => 120,
        })
    }
}

/* ---------- подписи ----------------------------------------------------- */

fn scope_text(scope: &str) -> &'static str {
    crate::stability::scope_text(scope)
}

fn ms(v: &Value) -> Option<String> {
    v.as_f64().map(|v| if v < 10.0 { format!("{:.1}", v).replace('.', ",") } else { format!("{v:.0}") })
}

fn pct(v: f64) -> String {
    if v == 0.0 {
        "0%".into()
    } else if v < 0.1 {
        "<0,1%".into()
    } else {
        format!("{:.1}%", v).replace('.', ",")
    }
}

fn secs_text(secs: i64) -> String {
    crate::stability::duration(secs)
}

fn stamp(ts: i64) -> String {
    platform::local_stamp((ts.max(0) as u64) * 1000)
}

/// Подпись оси времени: на коротких периодах — время, на длинных — дата.
fn tick_label(ts: i64, span: i64) -> String {
    let st = stamp(ts);
    if span <= 600 {
        platform::local_time((ts.max(0) as u64) * 1000)
    } else if span <= 86400 {
        st.get(11..16).unwrap_or(&st).to_owned()
    } else {
        st.get(0..5).unwrap_or(&st).to_owned()
    }
}

/// Событие по-человечески и его тон. None — служебное, в ленту не идёт.
pub fn event_text(e: &Value) -> Option<(String, Tone)> {
    let (text, level) = crate::stability::describe(s(&e["kind"]), &e["detail"])?;
    Some((text, match level {
        "bad" => Tone::Bad,
        "warn" => Tone::Warn,
        _ => Tone::Off,
    }))
}

fn tone_color(t: Tone) -> Color32 {
    match t {
        Tone::Bad => kit::BAD,
        Tone::Warn => kit::WARN,
        Tone::Ok => kit::OK,
        _ => kit::INK_4,
    }
}

fn tone_text(t: Tone) -> Color32 {
    match t {
        Tone::Bad => kit::BAD_TEXT,
        Tone::Warn => kit::WARN_TEXT,
        Tone::Ok => kit::OK_TEXT,
        _ => kit::INK_2,
    }
}

/// Тон по потерям: до 1% — норма, до 5% — заметно, дальше — плохо.
fn loss_tone(loss: Option<f64>) -> Tone {
    match loss {
        None => Tone::Off,
        Some(l) if l < 1.0 => Tone::Ok,
        Some(l) if l < 5.0 => Tone::Warn,
        _ => Tone::Bad,
    }
}

/// Сигнал Wi-Fi словами: −50 отлично, −60 хорошо, −70 терпимо, дальше плохо.
fn signal_word(dbm: f64) -> (&'static str, Tone) {
    if dbm >= -55.0 {
        ("отличный", Tone::Ok)
    } else if dbm >= -65.0 {
        ("хороший", Tone::Ok)
    } else if dbm >= -72.0 {
        ("средний", Tone::Warn)
    } else {
        ("слабый", Tone::Bad)
    }
}

/* ---------- страница ---------------------------------------------------- */

impl App {
    pub(super) fn stab_toggle(&mut self) {
        let on = !self.stab.running;
        self.set_config(json!({ "stabilityMonitor": on }));
        if on {
            self.stab.running = true;
            self.call(Op::StabStatus, "stab:start", Value::Null);
        } else {
            self.call(Op::Ignore, "stab:stop", Value::Null);
        }
        self.stab.due = Some(Instant::now() + Duration::from_millis(1200));
    }

    /// Каждый кадр: если вкладка на экране и пора — перечитать сводку и
    /// «прямо сейчас». Сборщик сам интерфейс не будит: иначе окно
    /// перерисовывалось бы раз в секунду на любой странице. На главной
    /// «прямо сейчас» нужно для строки мониторинга — реже.
    pub(super) fn stab_tick(&mut self, ctx: &egui::Context) {
        let tab = self.page == Page::Diag && self.diag_tab == DiagTab::Stability;
        let home = self.page == Page::Home && self.stab.running;
        if self.hidden || !(tab || home) {
            return;
        }
        let now = Instant::now();
        let live_due = *self.stab.live_due.get_or_insert(now);
        if now >= live_due && !self.stab.live_busy {
            self.stab.live_busy = true;
            self.stab.live_due = Some(now + Duration::from_secs(if tab { 2 } else { 5 }));
            self.call(Op::StabLive, "stab:live", Value::Null);
        }
        ctx.request_repaint_after(self.stab.live_due.unwrap_or(now).saturating_duration_since(now));
        if !tab {
            return;
        }
        let due = *self.stab.due.get_or_insert(now);
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }
        if !self.stab.busy {
            self.stab.busy = true;
            self.stab.due = Some(now + self.stab.every());
            self.call(Op::StabQuery, "stab:query", json!({ "range": RANGES[self.stab.range].0 }));
        }
    }

    /// Строка мониторинга на главной: есть ли сейчас связь и сколько было обрывов.
    pub(super) fn stab_home(&self) -> Option<(String, Tone)> {
        if !self.stab.running {
            return None;
        }
        let Some(l) = &self.stab.live else {
            return Some(("Мониторинг стабильности запущен".into(), Tone::Off));
        };
        let n = l["outages"].as_u64().unwrap_or(0);
        Some(if l["state"] == "down" {
            (format!("Мониторинг: сейчас нет связи — {}", scope_text(s(&l["scope"]))), Tone::Bad)
        } else if n > 0 {
            let since = l["since"].as_i64().map(|t| platform::local_time((t.max(0) as u64) * 1000)).map(|t| format!(" с {}", &t[..t.len().min(5)])).unwrap_or_default();
            (format!("Мониторинг: {n} {}{since}", crate::stability::plural(n, "обрыв", "обрыва", "обрывов")), Tone::Warn)
        } else {
            ("Мониторинг стабильности: обрывов нет".into(), Tone::Ok)
        })
    }

    pub(super) fn on_stab_export(&mut self, v: Value) {
        self.stab.export_busy = false;
        self.stab.export_note = Some(match v["error"].as_str() {
            Some(e) => (format!("Отчёт не сохранён: {e}"), true),
            None => (format!("Отчёт сохранён: {}", s(&v["path"])), false),
        });
    }

    pub(super) fn on_stab_live(&mut self, v: Value) {
        self.stab.live_busy = false;
        self.stab.running = v["running"] == true;
        self.stab.live = v["live"].as_object().map(|_| v["live"].clone());
    }

    pub(super) fn on_stab_query(&mut self, v: Value) {
        self.stab.busy = false;
        if let Some(e) = v["error"].as_str() {
            self.stab.note = Some(e.to_owned());
            return;
        }
        // Ответ на старый фильтр, пока пользователь уже выбрал другой.
        if v["range"] != RANGES[self.stab.range].0 {
            self.stab.due = Some(Instant::now());
            return;
        }
        self.stab.note = None;
        self.stab.running = v["running"] == true;
        if v["live"].is_object() {
            self.stab.live = Some(v["live"].clone());
        }
        self.stab.data = Some(v);
    }

    pub(super) fn stab_tab(&mut self, ui: &mut Ui, body: Rect) {
        // Сверху: фильтры времени, справа — состояние и включатель.
        let bar = Rect::from_min_size(body.min, vec2(body.width(), 40.0));
        let mut top = column(ui, bar, "stab-top");
        let mut pick = None;
        let mut toggle = false;
        top.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (i, (_, label)) in RANGES.iter().enumerate() {
                if kit::filter(ui, label, i == self.stab.range).clicked() {
                    pick = Some(i);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let (label, icon) = if self.stab.running { ("Выключить", Icon::Stop) } else { ("Включить мониторинг", Icon::Play) };
                let btn = if self.stab.running { Btn::secondary(label) } else { Btn::primary(label) };
                toggle = btn.icon(icon).show(ui).clicked();
                let (text, tone) = self.stab_now();
                kit::text(ui, &text, kit::regular(13.0), tone_text(tone));
                let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                kit::dot(ui.painter(), r.center(), tone, 4.0);
            });
        });
        if let Some(i) = pick.filter(|i| *i != self.stab.range) {
            self.stab.range = i;
            self.stab.due = Some(Instant::now());
        }
        if toggle {
            self.stab_toggle();
        }

        let content = Rect::from_min_max(pos2(body.left(), bar.bottom() + 16.0), body.max);
        let aside_w = 300.0;
        let main = Rect::from_min_max(content.min, pos2(content.right() - aside_w - 25.0, content.bottom()));
        let aside = Rect::from_min_max(pos2(content.right() - aside_w, content.top()), content.max);
        ui.painter().line_segment([pos2(aside.left() - 12.0, aside.top()), pos2(aside.left() - 12.0, aside.bottom())], Stroke::new(1.0_f32, kit::HAIR));

        let Some(data) = self.stab.data.clone() else {
            let mut c = column(ui, main, "stab-empty");
            kit::hint(&mut c, if self.stab.note.is_some() { self.stab.note.as_deref().unwrap_or("") } else { "Загружаю…" });
            return;
        };
        self.stab_main(ui, main, &data);
        self.stab_aside(ui, aside, &data);
    }

    /// Строка «прямо сейчас» в шапке вкладки.
    fn stab_now(&self) -> (String, Tone) {
        if !self.stab.running {
            return ("Мониторинг выключен".into(), Tone::Off);
        }
        let Some(l) = &self.stab.live else {
            return ("Запускаю…".into(), Tone::Off);
        };
        match s(&l["state"]) {
            "ok" => (format!("Связь есть · {} мс", ms(&l["inet"]).or_else(|| ms(&l["udp"])).unwrap_or_else(|| "—".into())), Tone::Ok),
            "down" => {
                let since = l["downSince"].as_i64().map(|t| format!(" {}", secs_text(l["ts"].as_i64().unwrap_or(t) - t + 1))).unwrap_or_default();
                (format!("Нет связи{since}: {}", scope_text(s(&l["scope"]))), Tone::Bad)
            }
            _ => ("Измеряю…".into(), Tone::Off),
        }
    }

    fn stab_main(&mut self, ui: &mut Ui, main: Rect, data: &Value) {
        let sum = &data["summary"];
        let mut c = column(ui, main, "stab-main");
        c.spacing_mut().item_spacing.y = 4.0;
        let tone = match s(&sum["tone"]) {
            "bad" => Tone::Bad,
            "warn" => Tone::Warn,
            "ok" => Tone::Ok,
            _ => Tone::Off,
        };
        c.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 22.0), Sense::hover());
            kit::dot(ui.painter(), r.center(), tone, 5.0);
            kit::text(ui, s(&sum["head"]), kit::semibold(17.0), kit::INK);
        });
        let lines: Vec<&str> = sum["lines"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let mut head_h = 26.0;
        if lines.is_empty() && sum["secs"].as_u64() == Some(0) {
            let t = if self.stab.running {
                "Мониторинг только начал работу — данные появятся через несколько секунд."
            } else {
                "Включите мониторинг: раз в секунду PLASTO проверяет роутер, узел провайдера и интернет, следит за Wi-Fi, адаптером и журналом Windows. Когда связь рвётся — видно, где именно."
            };
            let r = kit::paragraph(&mut c, t, kit::regular(13.0), kit::INK_3, main.width());
            head_h += r.rect.height() + 4.0;
        }
        // Вердикт не должен съесть график: не больше трёх строк, остальное — в подсказке.
        for l in lines.iter().take(3) {
            let r = kit::paragraph(&mut c, l, kit::regular(13.0), kit::INK_2, main.width());
            head_h += r.rect.height() + 4.0;
        }
        if lines.len() > 3 {
            let r = kit::paragraph(&mut c, &format!("И ещё {} — наведите сюда", lines.len() - 3), kit::regular(13.0), kit::INK_4, main.width());
            r.on_hover_text(lines[3..].join("\n\n"));
            head_h += 20.0;
        }
        let chart = Rect::from_min_max(pos2(main.left(), main.top() + head_h + 12.0), main.max);
        if chart.height() > 80.0 {
            timeline(ui, chart, data);
        }
    }

    fn stab_aside(&mut self, ui: &mut Ui, aside: Rect, data: &Value) {
        let sum = &data["summary"];
        let agg = &sum["agg"];
        let live = self.stab.live.clone().filter(|_| self.stab.running).or_else(|| data["live"].as_object().map(|_| data["live"].clone())).unwrap_or(Value::Null);
        let mut col = column(ui, aside, "stab-aside");
        let (mut export, mut clear) = (false, None);
        ScrollArea::vertical().id_salt("stab-aside-scroll").auto_shrink([false, false]).show(&mut col, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let w = ui.available_width() - 10.0;

            // Отчёт и очистка — над сводкой, чтобы не искать под лентой событий.
            let armed = self.stab.clear_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let label = if self.stab.export_busy { "Сохраняю…" } else { "Сохранить отчёт" };
                let r = kit::filter(ui, label, false).on_hover_text("HTML-страница за выбранный период: открыть в браузере, сохранить в PDF, приложить к заявке провайдеру. Имя сети Wi-Fi в отчёт не попадает.");
                if r.clicked() && !self.stab.export_busy {
                    export = true;
                }
                let r = kit::filter(ui, if armed { "Точно очистить?" } else { "Очистить историю" }, armed).on_hover_text("Удаляет все замеры и события. Мониторинг продолжит работу.");
                if r.clicked() {
                    clear = Some(armed);
                }
            });
            if armed {
                ui.ctx().request_repaint_after(Duration::from_secs(4));
            }
            if let Some((note, bad)) = &self.stab.export_note {
                let color = if *bad { kit::BAD_TEXT } else { kit::INK_3 };
                kit::paragraph(ui, note, kit::regular(12.0), color, w);
            }
            ui.add_space(4.0);

            // Связь за период.
            kit::label(ui, "Связь за период");
            let up = sum["upPct"].as_f64();
            ui.horizontal(|ui| {
                let big = up.map(|u| if u >= 99.995 { "100%".to_owned() } else { format!("{:.2}%", u).replace('.', ",") }).unwrap_or_else(|| "—".into());
                kit::text(ui, &big, kit::semibold(26.0), kit::INK);
                kit::text(ui, "со связью", kit::regular(13.0), kit::INK_3);
            });
            let outages = sum["outages"].as_u64().unwrap_or(0);
            let measured = sum["secs"].as_i64().unwrap_or(0);
            let span = data["to"].as_i64().unwrap_or(0) - data["from"].as_i64().unwrap_or(0);
            let mut info = vec![if outages == 0 {
                "Обрывов нет".to_owned()
            } else {
                format!("{outages} {} · без связи {}", crate::stability::plural(outages, "обрыв", "обрыва", "обрывов"), secs_text(sum["downSecs"].as_i64().unwrap_or(0)))
            }];
            if let Some((n, since)) = crate::stability::earlier_outages(data) {
                info.push(format!("Ещё {n} {} с {} — до начала периода", crate::stability::plural(n, "обрыв", "обрыва", "обрывов"), stamp(since)));
            }
            if measured > 0 && measured < span * 95 / 100 {
                info.push(format!("Замерено {} из {}", secs_text(measured), secs_text(span)));
            }
            if let Some(first) = data["firstTs"].as_i64() {
                if first > data["from"].as_i64().unwrap_or(0) {
                    info.push(format!("Данные с {}", stamp(first)));
                }
            }
            for i in info {
                kit::hint(ui, &i);
            }
            ui.add_space(6.0);

            // Путь: роутер → провайдер → интернет.
            kit::label(ui, "Путь до интернета · средний пинг, потери");
            let rows = [
                ("Роутер", "gw", live["gateway"].as_str()),
                ("Провайдер", "isp", live["ispHop"].as_str()),
                ("Интернет", "inet", None),
                ("UDP (как в играх)", "udp", None),
            ];
            for (name, key, addr) in rows {
                let l = &agg["layers"][key];
                let (r, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
                let p = ui.painter();
                let tone = loss_tone(l["loss"].as_f64());
                kit::dot(p, pos2(r.left() + 4.0, r.center().y), tone, 3.5);
                p.text(pos2(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, name, kit::regular(14.0), kit::INK);
                let value = match (ms(&l["avg"]), l["loss"].as_f64()) {
                    (Some(a), Some(loss)) => format!("{a} мс · {}", pct(loss)),
                    (None, Some(_)) => "не отвечает".into(),
                    _ => "не мерили".into(),
                };
                p.text(pos2(r.right(), r.center().y), Align2::RIGHT_CENTER, value, kit::regular(13.0), tone_text(tone));
                let tip = match (key, addr) {
                    ("isp", None) => "Первый узел провайдера ещё не найден: трассировка повторяется раз в 10 минут.".to_owned(),
                    ("udp", _) => "DNS-запрос к 77.88.8.8 по UDP — так ходят игры и звонки.".to_owned(),
                    ("inet", _) => "Лучший ответ из 77.88.8.8, 1.1.1.1 и 8.8.8.8.".to_owned(),
                    (_, Some(a)) => format!("Адрес: {a}"),
                    _ => String::new(),
                };
                let jitter = l["jitter"].as_f64().map(|j| format!("Джиттер {} мс, максимум {} мс", crate::stability::fmt1(j), ms(&l["max"]).unwrap_or_default()));
                let tip = [Some(tip).filter(|t| !t.is_empty()), jitter].into_iter().flatten().collect::<Vec<_>>().join("\n");
                if !tip.is_empty() {
                    resp.on_hover_text(tip);
                }
            }
            ui.add_space(6.0);

            // Подключение: Wi-Fi или кабель, адаптер и драйвер.
            let wifi_now = live["iface"]["wifi"] == true;
            let adapter = &live["adapter"];
            if wifi_now || agg["wifi"] == true {
                kit::label(ui, "Wi-Fi");
                if let Some(rssi) = agg["rssi"].as_f64() {
                    let (word, tone) = signal_word(rssi);
                    kit::text(ui, &format!("Сигнал {rssi:.0} dBm · {word}"), kit::regular(14.0), tone_text(tone));
                    if let Some(min) = agg["rssiMin"].as_i64() {
                        kit::hint(ui, &format!("Худший {min} dBm. Хороший сигнал — от −60, ниже −75 связь рвётся."));
                    }
                }
                if let Some(wl) = live["wifi"].as_object() {
                    let ch = wl.get("channel").and_then(Value::as_u64).map(|c| format!("канал {c} ({})", if c > 14 { "5 ГГц" } else { "2,4 ГГц" })).unwrap_or_default();
                    kit::hint(ui, &format!("{} · {} · {} Мбит/с", wl.get("ssid").and_then(Value::as_str).unwrap_or("—"), ch, wl.get("mbps").and_then(Value::as_u64).unwrap_or(0)));
                }
            } else if live["iface"].is_object() {
                kit::label(ui, "Кабель");
                let mbps = live["iface"]["mbps"].as_u64().unwrap_or(0);
                let speed = if mbps >= 1000 { format!("{} Гбит/с", crate::stability::fmt1(mbps as f64 / 1000.0).trim_end_matches(",0")) } else { format!("{mbps} Мбит/с") };
                kit::text(ui, &format!("Линк {speed}"), kit::regular(14.0), kit::INK);
                if let Some(min) = agg["linkMin"].as_u64().filter(|m| *m < mbps) {
                    kit::hint(ui, &format!("Падала до {min} Мбит/с — признак плохого кабеля или разъёма."));
                }
            }
            if adapter.is_object() {
                kit::hint(ui, s(&adapter["name"]));
                if let Some(v) = adapter["version"].as_str() {
                    kit::hint(ui, &format!("Драйвер {v}{}", adapter["date"].as_str().map(|d| format!(" от {d}")).unwrap_or_default()));
                }
                if adapter["powerSaving"] == true {
                    let r = kit::paragraph(ui, "Windows может отключать адаптер для экономии энергии", kit::regular(13.0), kit::WARN_TEXT, w);
                    r.on_hover_text("На ноутбуках — частая причина отвалов. Диспетчер устройств → сетевой адаптер → «Управление электропитанием» → снять галку «Разрешить отключение этого устройства».");
                }
            }
            let ev = &sum["events"];
            let counts = [
                ("linkDrops", "потерь подключения"),
                ("resets", "сбросов адаптера"),
                ("wifiDisc", "отключений Wi-Fi"),
                ("roams", "переходов между точками"),
                ("driver", "предупреждений драйвера"),
            ];
            let found: Vec<String> = counts.iter().filter_map(|(k, t)| ev[*k].as_u64().filter(|n| *n > 0).map(|n| format!("{n} {t}"))).collect();
            if !found.is_empty() {
                kit::hint(ui, &format!("За период: {}", found.join(", ")));
            }
            ui.add_space(6.0);

            // Лента событий: свежие сверху.
            kit::label(ui, "События");
            let wifi_relevant = wifi_now || agg["wifi"] == true;
            let events: Vec<(i64, String, Tone)> = data["events"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .filter(|e| wifi_relevant || !matches!(s(&e["kind"]), "wifi_disc" | "roam"))
                .filter_map(|e| event_text(e).map(|(t, tone)| (e["ts"].as_i64().unwrap_or(0), t, tone)))
                .take(150)
                .collect();
            if events.is_empty() {
                kit::hint(ui, "Событий за этот период нет.");
            }
            for (ts, text, tone) in events {
                let g = ui.painter().layout(text, kit::regular(13.0), tone_text(tone), w - 16.0);
                let (r, _) = ui.allocate_exact_size(vec2(w, g.size().y + 18.0), Sense::hover());
                let p = ui.painter();
                kit::dot(p, pos2(r.left() + 4.0, r.top() + 7.0), tone, 3.0);
                p.text(pos2(r.left() + 16.0, r.top()), Align2::LEFT_TOP, stamp(ts), kit::regular(12.0), kit::INK_4);
                p.galley(pos2(r.left() + 16.0, r.top() + 16.0), g, kit::INK);
            }
        });
        if export {
            self.stab.export_busy = true;
            self.stab.export_note = None;
            self.call(Op::StabExport, "stab:export", json!({ "range": RANGES[self.stab.range].0 }));
        }
        match clear {
            Some(true) => {
                self.stab.clear_armed = None;
                self.stab.data = None;
                self.call(Op::StabClear, "stab:clear", Value::Null);
            }
            Some(false) => self.stab.clear_armed = Some(Instant::now()),
            None => {}
        }
    }
}

/* ---------- график ------------------------------------------------------ */

const C_INET: Color32 = kit::OK;
const C_ISP: Color32 = kit::PING;
const C_GW: Color32 = kit::UP;

/// Округление шкалы вверх до «красивого» числа.
fn nice(v: f64) -> f64 {
    for n in [5.0, 10.0, 20.0, 30.0, 50.0, 75.0, 100.0, 150.0, 200.0, 300.0, 500.0, 1000.0] {
        if v <= n {
            return n;
        }
    }
    (v / 1000.0).ceil() * 1000.0
}

/// Лента времени: задержка по слоям, обрывы, сигнал или линк, события.
fn timeline(ui: &mut Ui, rect: Rect, data: &Value) {
    let buckets: Vec<&Value> = data["buckets"].as_array().map(|b| b.iter().collect()).unwrap_or_default();
    if buckets.is_empty() {
        return;
    }
    let (from, to) = (data["from"].as_i64().unwrap_or(0), data["to"].as_i64().unwrap_or(1));
    let span = (to - from).max(1);
    let p = ui.painter().clone();

    // Легенда.
    let legend_y = rect.top() + 8.0;
    let mut x = rect.left() + 40.0;
    for (name, color) in [("Интернет", C_INET), ("Провайдер", C_ISP), ("Роутер", C_GW), ("Нет связи", kit::BAD), ("Нет данных", kit::INK_4)] {
        p.circle_filled(pos2(x + 4.0, legend_y), 4.0, color);
        let g = p.layout_no_wrap(name.to_owned(), kit::regular(12.0), kit::INK_3);
        let gw = g.size().x;
        p.galley(pos2(x + 12.0, legend_y - g.size().y / 2.0), g, kit::INK_3);
        x += gw + 28.0;
    }

    // Полосы: задержка (большая), сигнал или линк, события, ось времени.
    let left = rect.left() + 40.0;
    let plot = Rect::from_min_max(pos2(left, rect.top() + 24.0), pos2(rect.right() - 4.0, rect.bottom() - 18.0));
    let events_h = 14.0;
    let sub_h = (plot.height() * 0.24).clamp(36.0, 70.0);
    let lat = Rect::from_min_max(plot.min, pos2(plot.right(), plot.bottom() - sub_h - events_h - 10.0));
    let sub = Rect::from_min_max(pos2(plot.left(), lat.bottom() + 6.0), pos2(plot.right(), lat.bottom() + 6.0 + sub_h));
    let evs = Rect::from_min_max(pos2(plot.left(), sub.bottom() + 4.0), pos2(plot.right(), sub.bottom() + 4.0 + events_h));
    let n = buckets.len();
    let bw = plot.width() / n as f32;
    let bx = |i: usize| plot.left() + i as f32 * bw;
    let tx = |ts: i64| plot.left() + ((ts - from) as f32 / span as f32) * plot.width();

    // Шкала задержки — по средним (а не по пикам), чтобы рядовой пинг не
    // прижимался к нулю из-за одного выброса.
    let peak = buckets.iter().filter_map(|b| ["inet", "isp", "gw"].iter().filter_map(|k| b["layers"][*k]["avg"].as_f64()).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))).fold(0.0, f64::max);
    let ymax = nice((peak * 1.3).max(5.0));
    let ly = |v: f64| lat.bottom() - (v.min(ymax) / ymax) as f32 * lat.height();

    p.rect_filled(lat, 8.0, kit::FILL_1);
    p.rect_filled(sub, 6.0, kit::FILL_1);
    for frac in [0.5_f32, 1.0] {
        let y = lat.bottom() - frac * lat.height();
        p.line_segment([pos2(lat.left(), y), pos2(lat.right(), y)], Stroke::new(1.0_f32, kit::HAIR));
        p.text(pos2(lat.left() - 6.0, y), Align2::RIGHT_CENTER, format!("{:.0}", ymax as f32 * frac), kit::regular(11.0), kit::INK_4);
    }
    p.text(pos2(lat.left() - 6.0, lat.bottom()), Align2::RIGHT_BOTTOM, "мс", kit::regular(11.0), kit::INK_4);

    // Пропуски — одним куском на серию пустых столбцов: отдельные столбики
    // с зазорами между ними рябят «расчёской».
    let empty = |i: usize| buckets[i]["secs"].as_u64() == Some(0);
    let mut i = 0;
    while i < n {
        if !empty(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && empty(i) {
            i += 1;
        }
        for lane in [lat, sub] {
            p.rect_filled(Rect::from_min_max(pos2(bx(start), lane.top()), pos2(bx(i), lane.bottom())), 0.0, Color32::from_black_alpha(28));
        }
    }
    // Обрывы и потери — по столбцам, внахлёст на полпикселя, без зазоров.
    for (i, b) in buckets.iter().enumerate() {
        let col = Rect::from_min_max(pos2(bx(i), lat.top()), pos2(bx(i) + bw + 0.5, lat.bottom()));
        let secs = b["secs"].as_f64().unwrap_or(0.0);
        if secs == 0.0 {
            continue;
        }
        let down = b["down"].as_f64().unwrap_or(0.0);
        if down > 0.0 {
            let a = (0.25 + 0.6 * (down / secs)) as f32;
            p.rect_filled(col, 0.0, kit::BAD.gamma_multiply(a));
        } else if let Some(loss) = b["layers"]["inet"]["loss"].as_f64().filter(|l| *l > 0.0) {
            // Потери без обрыва — жёлтая отметка снизу, выше — больше потерь.
            let h = (lat.height() * 0.25 * (loss / 20.0).min(1.0) as f32).max(3.0);
            p.rect_filled(Rect::from_min_max(pos2(col.left(), lat.bottom() - h), col.max), 0.0, kit::WARN.gamma_multiply(0.7));
        }
    }

    // Линия по столбцам с разрывами там, где значения нет.
    let polyline = |values: &mut dyn Iterator<Item = Option<Pos2>>, color: Color32, width: f32| {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() == 1 {
                p.circle_filled(run[0], width, color);
            } else if run.len() > 1 {
                p.add(egui::Shape::line(std::mem::take(run), Stroke::new(width, color)));
            }
            run.clear();
        };
        for v in values {
            match v {
                Some(q) => run.push(q),
                None => flush(&mut run),
            }
        }
        flush(&mut run);
    };

    // Линии слоёв: дальние под ближними.
    for (key, color) in [("inet", C_INET), ("isp", C_ISP), ("gw", C_GW)] {
        polyline(&mut buckets.iter().enumerate().map(|(i, b)| b["layers"][key]["avg"].as_f64().map(|v| pos2(bx(i) + bw / 2.0, ly(v)))), color, 1.6);
    }

    // Нижняя полоса: сигнал Wi-Fi, а у кабеля — скорость линка и ошибки.
    let wifi = buckets.iter().any(|b| b["rssi"].is_number());
    let sub_label = if wifi { "dBm" } else { "Мбит/с" };
    p.text(pos2(sub.left() - 6.0, sub.bottom()), Align2::RIGHT_BOTTOM, sub_label, kit::regular(11.0), kit::INK_4);
    if wifi {
        // −90…−30 dBm; ниже −70 подсвечено.
        let sy = |v: f64| sub.bottom() - (((v + 90.0) / 60.0).clamp(0.0, 1.0) as f32) * sub.height();
        let warn_y = sy(-70.0);
        p.rect_filled(Rect::from_min_max(pos2(sub.left(), warn_y), sub.max), 6.0, kit::WARN.gamma_multiply(0.08));
        p.text(pos2(sub.left() - 6.0, warn_y), Align2::RIGHT_CENTER, "−70", kit::regular(11.0), kit::INK_4);
        polyline(&mut buckets.iter().enumerate().map(|(i, b)| b["rssi"].as_f64().map(|v| pos2(bx(i) + bw / 2.0, sy(v)))), C_GW, 1.4);
    } else {
        let top = buckets.iter().filter_map(|b| b["linkMin"].as_f64()).fold(0.0, f64::max).max(1.0);
        p.text(pos2(sub.left() - 6.0, sub.top()), Align2::RIGHT_TOP, format!("{top:.0}"), kit::regular(11.0), kit::INK_4);
        // Линия скорости линка; где она ниже обычной — жёлтые точки.
        let sy = |v: f64| sub.bottom() - 3.0 - (v / top) as f32 * (sub.height() - 8.0);
        polyline(&mut buckets.iter().enumerate().map(|(i, b)| b["linkMin"].as_f64().map(|v| pos2(bx(i) + bw / 2.0, sy(v)))), kit::INK_4, 1.4);
        for (i, b) in buckets.iter().enumerate() {
            if let Some(v) = b["linkMin"].as_f64().filter(|v| *v < top) {
                p.circle_filled(pos2(bx(i) + bw / 2.0, sy(v)), 2.5, kit::WARN);
            }
            if b["err"].as_u64().is_some_and(|e| e > 0) {
                p.circle_filled(pos2(bx(i) + bw / 2.0, sub.top() + 4.0), 2.5, kit::BAD);
            }
        }
    }

    // События — метки на своей полосе.
    let events: Vec<&Value> = data["events"].as_array().map(|e| e.iter().collect()).unwrap_or_default();
    for e in &events {
        if let Some((_, tone)) = event_text(e).filter(|(_, t)| *t != Tone::Off) {
            let x = tx(e["ts"].as_i64().unwrap_or(from));
            if x >= evs.left() && x <= evs.right() {
                p.circle_filled(pos2(x, evs.center().y), 3.0, tone_color(tone));
            }
        }
    }

    // Ось времени.
    let ticks = 5;
    for k in 0..=ticks {
        let ts = from + span * k / ticks;
        let x = tx(ts);
        let align = if k == 0 { Align2::LEFT_TOP } else if k == ticks { Align2::RIGHT_TOP } else { Align2::CENTER_TOP };
        p.text(pos2(x, plot.bottom() + 4.0), align, tick_label(ts, span), kit::regular(11.0), kit::INK_4);
    }

    // Наведение: столбец, его значения и события в нём.
    let resp = ui.interact(plot, ui.id().with("stab-timeline"), Sense::hover());
    if let Some(pos) = resp.hover_pos() {
        let i = (((pos.x - plot.left()) / bw) as usize).min(n - 1);
        let b = buckets[i];
        let x = bx(i) + bw / 2.0;
        p.line_segment([pos2(x, plot.top()), pos2(x, evs.bottom())], Stroke::new(1.0_f32, kit::INK_4));
        let t0 = b["t"].as_i64().unwrap_or(from);
        let t1 = t0 + (span as f64 / n as f64).round() as i64;
        let in_bucket: Vec<String> = events
            .iter()
            .filter(|e| e["ts"].as_i64().is_some_and(|t| t >= t0 && t < t1))
            .filter_map(|e| event_text(e).map(|(t, _)| t))
            .take(6)
            .collect();
        resp.on_hover_ui_at_pointer(|ui| {
            ui.set_max_width(320.0);
            let head = if span <= 3600 { platform::local_time((t0.max(0) as u64) * 1000) } else { stamp(t0) };
            ui.label(egui::RichText::new(head).font(kit::semibold(13.0)).color(kit::INK));
            if b["secs"].as_u64() == Some(0) {
                ui.label(egui::RichText::new("Нет данных: PLASTO был закрыт или компьютер спал").font(kit::regular(12.0)).color(kit::INK_3));
            } else {
                let down = b["down"].as_i64().unwrap_or(0);
                if down > 0 {
                    let by = b["downBy"].as_object().and_then(|m| m.iter().max_by_key(|(_, v)| v.as_i64().unwrap_or(0)).map(|(k, _)| k.clone())).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("Без связи {}: {}", secs_text(down), scope_text(&by))).font(kit::regular(12.0)).color(kit::BAD_TEXT));
                }
                for (name, key) in [("Интернет", "inet"), ("Провайдер", "isp"), ("Роутер", "gw"), ("UDP", "udp")] {
                    let l = &b["layers"][key];
                    if let Some(a) = ms(&l["avg"]) {
                        let loss = l["loss"].as_f64().filter(|v| *v > 0.0).map(|v| format!(" · потери {}", pct(v))).unwrap_or_default();
                        ui.label(egui::RichText::new(format!("{name}: {a} мс{loss}")).font(kit::regular(12.0)).color(kit::INK_2));
                    } else if l["loss"].is_number() {
                        ui.label(egui::RichText::new(format!("{name}: не отвечал")).font(kit::regular(12.0)).color(kit::BAD_TEXT));
                    }
                }
                if let Some(r) = b["rssi"].as_f64() {
                    ui.label(egui::RichText::new(format!("Wi-Fi: {r:.0} dBm")).font(kit::regular(12.0)).color(kit::INK_2));
                }
                if let Some(e) = b["err"].as_u64().filter(|e| *e > 0) {
                    ui.label(egui::RichText::new(format!("Ошибок адаптера: {e}")).font(kit::regular(12.0)).color(kit::WARN_TEXT));
                }
            }
            for t in in_bucket {
                ui.label(egui::RichText::new(t).font(kit::regular(12.0)).color(kit::INK));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn шкала_округляется_вверх() {
        assert_eq!(nice(3.0), 5.0);
        assert_eq!(nice(21.0), 30.0);
        assert_eq!(nice(1200.0), 2000.0);
    }

    #[test]
    fn события_по_человечески() {
        let e = json!({ "kind": "outage", "detail": { "secs": 14, "scope": "isp" } });
        assert_eq!(event_text(&e).unwrap().0, "Обрыв 14 с: не отвечал провайдер");
        assert!(event_text(&json!({ "kind": "link_up", "detail": {} })).is_none(), "служебное не показываем");
        assert_eq!(signal_word(-80.0).0, "слабый");
        assert_eq!(loss_tone(Some(0.5)), Tone::Ok);
        assert_eq!(pct(0.05), "<0,1%");
    }
}
