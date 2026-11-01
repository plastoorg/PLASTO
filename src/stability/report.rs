//! Отчёт о стабильности одной HTML-страницей: открыть в браузере, сохранить
//! в PDF, приложить к заявке провайдеру. Без внешних файлов и скриптов.
//!
//! Имя сети Wi-Fi и адреса точек доступа в отчёт не попадают: его пересылают
//! чужим людям. Адреса роутера и узла провайдера — попадают: без них
//! провайдеру не на что смотреть.

use serde_json::Value;

use super::analyze::{describe, duration, fmt1};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn ms(v: &Value) -> String {
    v.as_f64().map(|v| if v < 10.0 { fmt1(v) } else { format!("{v:.0}") }).map_or("—".into(), |v| format!("{v} мс"))
}

fn pct(v: &Value) -> String {
    v.as_f64().map_or("—".into(), |v| format!("{}%", fmt1(v)))
}

/// График: средняя задержка до интернета, провайдера и роутера по столбцам,
/// обрывы — красным фоном, пропуски — серым.
fn chart(q: &Value) -> String {
    let buckets: Vec<&Value> = q["buckets"].as_array().map(|b| b.iter().collect()).unwrap_or_default();
    if buckets.is_empty() {
        return String::new();
    }
    let (w, h, pad) = (900.0, 220.0, 36.0);
    let n = buckets.len() as f64;
    let bw = (w - pad) / n;
    let peak = buckets.iter().filter_map(|b| ["inet", "isp", "gw"].iter().filter_map(|k| b["layers"][*k]["avg"].as_f64()).reduce(f64::max)).fold(5.0, f64::max);
    let ymax = [10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0].into_iter().find(|v| *v >= peak * 1.2).unwrap_or(2000.0);
    let y = |v: f64| h - 16.0 - (v.min(ymax) / ymax) * (h - 24.0);
    let mut svg = format!(r##"<svg viewBox="0 0 {w} {h}" role="img" aria-label="Задержка и обрывы"><rect x="{pad}" y="8" width="{}" height="{}" fill="#f6f4f0"/>"##, w - pad, h - 24.0);
    // Пропуски — одним прямоугольником на серию, чтобы между столбцами не было швов.
    let mut gap: Option<usize> = None;
    for i in 0..=buckets.len() {
        let empty = i < buckets.len() && buckets[i]["secs"].as_u64() == Some(0);
        match (empty, gap) {
            (true, None) => gap = Some(i),
            (false, Some(start)) => {
                svg += &format!(r##"<rect x="{:.1}" y="8" width="{:.1}" height="{}" fill="#e2ded8"/>"##, pad + start as f64 * bw, (i - start) as f64 * bw, h - 24.0);
                gap = None;
            }
            _ => {}
        }
    }
    for (i, b) in buckets.iter().enumerate() {
        let secs = b["secs"].as_f64().unwrap_or(0.0);
        let down = b["down"].as_f64().unwrap_or(0.0);
        if secs > 0.0 && down > 0.0 {
            svg += &format!(r#"<rect x="{:.1}" y="8" width="{:.1}" height="{}" fill="rgba(214,69,50,{:.2})"/>"#, pad + i as f64 * bw, bw + 0.5, h - 24.0, 0.25 + 0.6 * down / secs);
        }
    }
    for (key, color) in [("inet", "#2e9e62"), ("isp", "#8a63d2"), ("gw", "#3b8fd0")] {
        let mut d = String::new();
        let mut pen = false;
        for (i, b) in buckets.iter().enumerate() {
            match b["layers"][key]["avg"].as_f64() {
                Some(v) => {
                    d += &format!("{}{:.1},{:.1} ", if pen { "L" } else { "M" }, pad + (i as f64 + 0.5) * bw, y(v));
                    pen = true;
                }
                None => pen = false,
            }
        }
        if !d.is_empty() {
            svg += &format!(r#"<path d="{d}" fill="none" stroke="{color}" stroke-width="1.6"/>"#);
        }
    }
    for frac in [0.0, 0.5, 1.0] {
        let v = ymax * frac;
        svg += &format!(r##"<text x="{}" y="{:.1}" font-size="11" text-anchor="end" fill="#888">{v:.0}</text>"##, pad - 6.0, y(v) + 4.0);
    }
    svg += r##"<text x="4" y="20" font-size="11" fill="#888">мс</text></svg>"##;
    svg
}

/// Обрывы с запуска мониторинга, не попавшие в период: на главной счётчик
/// идёт с запуска, а отчёт и страница — за выбранный период, и «1 обрыв» там
/// рядом с «обрывов не было» здесь выглядел бы ошибкой. Сколько и с какого
/// момента идёт счёт.
pub fn earlier_outages(q: &Value) -> Option<(u64, i64)> {
    let (total, since) = (q["live"]["outages"].as_u64()?, q["live"]["since"].as_i64()?);
    let shown = q["events"].as_array()?.iter().filter(|e| e["kind"] == "outage" && e["ts"].as_i64().is_some_and(|t| t >= since)).count() as u64;
    (total > shown).then_some((total - shown, since))
}

pub fn render(q: &Value, stamp: &dyn Fn(i64) -> String) -> String {
    let sum = &q["summary"];
    let agg = &sum["agg"];
    let live = &q["live"];
    let adapter = &live["adapter"];
    let (from, to) = (q["from"].as_i64().unwrap_or(0), q["to"].as_i64().unwrap_or(0));
    let mut html = String::from(
        r#"<!doctype html><html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Стабильность интернета — отчёт PLASTO</title><style>
body{font:15px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;color:#1d1a1c;background:#fff;max-width:960px;margin:32px auto;padding:0 16px}
h1{font-size:24px;margin:0 0 4px}h2{font-size:17px;margin:28px 0 8px}
.muted{color:#6b6568}.verdict{padding:12px 16px;border-radius:10px;background:#f6f4f0;margin:16px 0}
.bad{border-left:4px solid #d64532}.warn{border-left:4px solid #e0a23a}.ok{border-left:4px solid #2e9e62}.idle{border-left:4px solid #aaa}
table{border-collapse:collapse;width:100%;font-size:14px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid #eee}th{color:#6b6568;font-weight:600}
td.n{text-align:right;font-variant-numeric:tabular-nums}svg{width:100%;height:auto}
.legend span{display:inline-block;margin-right:16px;font-size:13px}.legend i{display:inline-block;width:10px;height:10px;border-radius:50%;margin-right:6px;vertical-align:middle}
@media print{body{margin:0}}
</style></head><body>"#,
    );
    html += "<h1>Стабильность интернета</h1>";
    html += &format!("<div class=\"muted\">Период: {} — {} · отчёт сформирован {} программой PLASTO</div>", esc(&stamp(from)), esc(&stamp(to)), esc(&stamp(to)));
    let measured = sum["secs"].as_i64().unwrap_or(0);
    html += &format!("<div class=\"muted\">Замерено: {} из {}; остальное время мониторинг не работал (программа закрыта или компьютер спал).</div>", duration(measured), duration(to - from));

    // Вывод.
    html += &format!("<div class=\"verdict {}\"><b>{}</b>", esc(sum["tone"].as_str().unwrap_or("idle")), esc(sum["head"].as_str().unwrap_or("")));
    for l in sum["lines"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        html += &format!("<p style=\"margin:6px 0 0\">{}</p>", esc(l));
    }
    if let Some(up) = sum["upPct"].as_f64() {
        html += &format!("<p style=\"margin:6px 0 0\">Связь была {}% замеренного времени.</p>", fmt1(up));
    }
    html += "</div>";

    // Подключение.
    html += "<h2>Подключение</h2><table>";
    let row = |k: &str, v: String| format!("<tr><th style=\"width:220px\">{}</th><td>{}</td></tr>", esc(k), esc(&v));
    if let Some(name) = live["iface"]["name"].as_str() {
        let kind = if live["iface"]["wifi"] == true { "Wi-Fi" } else { "кабель" };
        html += &row("Сетевой адаптер", format!("{name} ({kind})"));
    }
    if let Some(v) = adapter["version"].as_str() {
        html += &row("Драйвер", format!("{} {v}{}", adapter["provider"].as_str().unwrap_or(""), adapter["date"].as_str().map(|d| format!(" от {d}")).unwrap_or_default()));
    }
    if adapter["powerSaving"] == true {
        html += &row("Энергосбережение адаптера", "Windows разрешено отключать адаптер".into());
    }
    html += &row("Роутер (шлюз)", live["gateway"].as_str().unwrap_or("—").into());
    html += &row("Первый узел провайдера", live["ispHop"].as_str().unwrap_or("не определён").into());
    if let Some(r) = agg["rssi"].as_f64() {
        html += &row("Сигнал Wi-Fi", format!("в среднем {r:.0} dBm, худший {} dBm", agg["rssiMin"]));
    }
    html += "</table>";

    // Узлы на пути.
    html += "<h2>Путь до интернета</h2><table><tr><th>Узел</th><th class=\"n\">Средний пинг</th><th class=\"n\">Максимум</th><th class=\"n\">Джиттер</th><th class=\"n\">Потери</th></tr>";
    for (name, key) in [("Роутер", "gw"), ("Узел провайдера", "isp"), ("Интернет (77.88.8.8, 1.1.1.1, 8.8.8.8)", "inet"), ("DNS по UDP (как игры)", "udp")] {
        let l = &agg["layers"][key];
        html += &format!(
            "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
            esc(name),
            ms(&l["avg"]),
            ms(&l["max"]),
            ms(&l["jitter"]),
            pct(&l["loss"])
        );
    }
    html += "</table>";

    html += "<h2>Задержка и обрывы</h2><div class=\"legend\"><span><i style=\"background:#2e9e62\"></i>интернет</span><span><i style=\"background:#8a63d2\"></i>узел провайдера</span><span><i style=\"background:#3b8fd0\"></i>роутер</span><span><i style=\"background:#d64532\"></i>нет связи</span><span><i style=\"background:#e2ded8\"></i>нет данных</span></div>";
    html += &chart(q);
    html += &format!("<div class=\"muted\" style=\"display:flex;justify-content:space-between\"><span>{}</span><span>{}</span></div>", esc(&stamp(from)), esc(&stamp(to)));

    // Обрывы — то, ради чего отчёт и пишется.
    let events: Vec<&Value> = q["events"].as_array().map(|e| e.iter().collect()).unwrap_or_default();
    let outages: Vec<&&Value> = events.iter().filter(|e| e["kind"] == "outage").collect();
    html += &format!("<h2>Обрывы ({})</h2>", outages.len());
    if outages.is_empty() {
        html += "<p class=\"muted\">За этот период обрывов не было.</p>";
    } else {
        html += "<table><tr><th>Начало</th><th class=\"n\">Длительность</th><th>Где оборвалось</th></tr>";
        for e in &outages {
            let (text, _) = describe(e["kind"].as_str().unwrap_or(""), &e["detail"]).unwrap_or_default();
            let place = text.split_once(": ").map_or(text.as_str(), |(_, p)| p).to_owned();
            html += &format!(
                "<tr><td>{}</td><td class=\"n\">{}</td><td>{}</td></tr>",
                esc(&stamp(e["ts"].as_i64().unwrap_or(0))),
                duration(e["detail"]["secs"].as_i64().unwrap_or(0)),
                esc(&place)
            );
        }
        html += "</table>";
    }
    if let Some((n, since)) = earlier_outages(q) {
        html += &format!(
            "<p class=\"muted\">Ещё {n} {} с начала мониторинга ({}) — до начала периода. Чтобы они попали в отчёт, выберите период длиннее.</p>",
            crate::stability::plural(n, "обрыв был", "обрыва было", "обрывов было"),
            esc(&stamp(since))
        );
    }

    // Отключения Wi-Fi при работе по кабелю — шум: Windows сама гасит Wi-Fi,
    // когда подключён кабель.
    let wifi = agg["wifi"] == true || live["iface"]["wifi"] == true;
    let other: Vec<(i64, String)> = events
        .iter()
        .filter(|e| e["kind"] != "outage")
        .filter(|e| wifi || e["kind"] != "wifi_disc")
        .filter_map(|e| describe(e["kind"].as_str()?, &e["detail"]).filter(|(_, lvl)| *lvl != "info").map(|(t, _)| (e["ts"].as_i64().unwrap_or(0), t)))
        .collect();
    if !other.is_empty() {
        html += &format!("<h2>Другие события ({})</h2><table><tr><th>Время</th><th>Событие</th></tr>", other.len());
        for (ts, t) in other {
            html += &format!("<tr><td style=\"white-space:nowrap\">{}</td><td>{}</td></tr>", esc(&stamp(ts)), esc(&t));
        }
        html += "</table>";
    }

    html += "<h2>Как мерилось</h2><p class=\"muted\">Раз в секунду, параллельно: ICMP-запрос к роутеру, к первому узлу провайдера (найден трассировкой) и к трём внешним адресам разных сетей — 77.88.8.8, 1.1.1.1, 8.8.8.8, плюс DNS-запрос по UDP к 77.88.8.8. Связь считается пропавшей, если не ответил ни один внешний адрес и UDP; обрыв — две и более таких секунды подряд. Место обрыва — ближайший к компьютеру узел, который перестал отвечать. Дополнительно записываются состояние сетевого адаптера и Wi-Fi и события системного журнала Windows (сбросы адаптера, отключения Wi-Fi, сон).</p>";
    html += "</body></html>";
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn отчёт_содержит_вывод_обрывы_и_экранирует_текст() {
        let q = json!({
            "from": 0, "to": 3600,
            "summary": { "head": "1 обрыв, без интернета 14 с", "tone": "bad", "lines": ["<script>"], "secs": 3600, "upPct": 99.6, "agg": { "layers": { "inet": { "avg": 17.5, "loss": 0.4 } } } },
            "buckets": [{ "secs": 60, "down": 14, "layers": { "inet": { "avg": 18.0 } } }, { "secs": 0, "layers": {} }],
            "events": [
                { "ts": 100, "kind": "outage", "detail": { "secs": 14, "scope": "isp" } },
                { "ts": 200, "kind": "adapter_reset", "detail": {} },
                { "ts": 250, "kind": "wifi_disc", "detail": { "reason": "кабель подключён" } },
                { "ts": 300, "kind": "monitor", "detail": { "on": true } },
                { "ts": 400, "kind": "roam", "detail": { "ssid": "МояСеть", "to": "aa:bb" } },
            ],
            "live": { "gateway": "192.168.1.1", "ispHop": "100.107.0.1", "iface": { "name": "Intel", "wifi": false } },
        });
        let html = render(&q, &|ts| format!("t{ts}"));
        assert!(html.contains("1 обрыв, без интернета 14 с"));
        assert!(html.contains("&lt;script&gt;") && !html.contains("<script>"));
        assert!(html.contains("<td>t100</td><td class=\"n\">14 с</td><td>не отвечал провайдер</td>"));
        assert!(html.contains("Сброс сетевого адаптера"));
        assert!(html.contains("100.107.0.1"));
        assert!(!html.contains("МояСеть") && !html.contains("aa:bb"), "имя сети и BSSID в отчёт не попадают");
        assert!(!html.contains("Мониторинг включён"), "служебные события — не в отчёт");
        assert!(html.contains("<svg"));
        assert!(!html.contains("кабель подключён"), "отключения Wi-Fi при кабеле — не в отчёт");
        assert!(!html.contains("до начала периода"));
    }

    #[test]
    fn обрывы_до_начала_периода_упомянуты() {
        let q = |outages: u64, events: Value| json!({ "events": events, "live": { "since": 50, "outages": outages } });
        let ev = json!([{ "ts": 100, "kind": "outage", "detail": {} }, { "ts": 10, "kind": "outage", "detail": {} }]);
        assert_eq!(earlier_outages(&q(1, ev.clone())), None, "единственный обрыв с запуска уже в периоде");
        assert_eq!(earlier_outages(&q(3, ev)), Some((2, 50)), "обрыв до запуска мониторинга не в счёт");
        let html = render(&q(1, json!([])), &|ts| format!("t{ts}"));
        assert!(html.contains("За этот период обрывов не было.") && html.contains("Ещё 1 обрыв был с начала мониторинга (t50) — до начала периода"));
    }
}
