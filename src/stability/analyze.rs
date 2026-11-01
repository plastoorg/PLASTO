//! Логика мониторинга без сети и Windows: что значит секунда замеров, где
//! обрыв, как сводить секунды в минуты и часы и что сказать человеку.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Ответ одной точки за секунду.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Rtt {
    /// Не мерили: точка неизвестна (нет шлюза, узел провайдера ещё не найден).
    #[default]
    Skip,
    Lost,
    Ms(f32),
}

impl Rtt {
    /// В базе: NULL — не мерили, -1 — потеря, иначе миллисекунды.
    pub fn to_sql(self) -> Option<f64> {
        match self {
            Rtt::Skip => None,
            Rtt::Lost => Some(-1.0),
            Rtt::Ms(v) => Some(v as f64),
        }
    }

    pub fn from_sql(v: Option<f64>) -> Self {
        match v {
            None => Rtt::Skip,
            Some(v) if v < 0.0 => Rtt::Lost,
            Some(v) => Rtt::Ms(v as f32),
        }
    }

    pub fn ms(self) -> Option<f32> {
        match self {
            Rtt::Ms(v) => Some(v),
            _ => None,
        }
    }
}

/// Слои пути: роутер, первый узел провайдера, интернет (лучший из внешних
/// адресов по ICMP), интернет по UDP (DNS-запрос — так ходят игры).
pub const LAYERS: [&str; 4] = ["gw", "isp", "inet", "udp"];

/// Одна секунда.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// Unix-время, секунды.
    pub ts: i64,
    pub rtt: [Rtt; 4],
    /// Линк адаптера: подключён ли кабель или Wi-Fi. None — адаптер не найден.
    pub up: Option<bool>,
    pub wifi: bool,
    pub rssi: Option<i32>,
    pub quality: Option<u8>,
    /// Скорость Wi-Fi или линка адаптера, Мбит/с.
    pub rate: Option<u32>,
    pub link: Option<u32>,
    /// Ошибки и отброшенные пакеты адаптера за эту секунду.
    pub err: u32,
    pub disc: u32,
}

/// Где оборвалось, от ближнего к дальнему.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Адаптер без линка: кабель выдернут, Wi-Fi отключился.
    Link,
    /// Не отвечает роутер: домашняя сеть, Wi-Fi, сам роутер.
    Local,
    /// Роутер отвечает, первый узел провайдера — нет.
    Isp,
    /// Узел провайдера отвечает (или неизвестен), внешние адреса — нет.
    Internet,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Link, Scope::Local, Scope::Isp, Scope::Internet];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Link => "link",
            Scope::Local => "local",
            Scope::Isp => "isp",
            Scope::Internet => "internet",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// Была ли в эту секунду связь с интернетом. None — нечем судить: ничего
/// внешнего не мерили (мониторинг только запускается).
pub fn classify(s: &Sample) -> Option<Result<(), Scope>> {
    let [gw, isp, inet, udp] = s.rtt;
    if s.up == Some(false) {
        return Some(Err(Scope::Link));
    }
    if inet.ms().is_some() || udp.ms().is_some() {
        return Some(Ok(()));
    }
    if inet == Rtt::Skip && udp == Rtt::Skip {
        return None;
    }
    // Роутер или узел провайдера мог промолчать и при живом интернете —
    // ICMP режут; поэтому смотрим на них, только когда интернета нет.
    Some(Err(if gw == Rtt::Lost {
        Scope::Local
    } else if isp == Rtt::Lost {
        Scope::Isp
    } else {
        Scope::Internet
    }))
}

/* ---------- обрывы ------------------------------------------------------ */

/// Обрыв — столько секунд без интернета подряд. Одна потерянная секунда —
/// это потеря пакета, а не обрыв.
pub const MIN_OUTAGE: i64 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Incident {
    pub start: i64,
    /// Последняя секунда без связи.
    pub end: i64,
    pub scope: Scope,
}

impl Incident {
    pub fn secs(&self) -> i64 {
        self.end - self.start + 1
    }
}

/// Собирает секунды без связи в обрывы. Пропуск во времени (сон, PLASTO был
/// закрыт) обрывает серию: что было в паузе, неизвестно.
#[derive(Default)]
pub struct Tracker {
    run: Option<(i64, i64, [u32; 4])>,
}

impl Tracker {
    pub fn feed(&mut self, s: &Sample) -> Option<Incident> {
        let down = match classify(s) {
            Some(Err(scope)) => Some(scope),
            _ => None,
        };
        let mut done = None;
        if let Some((start, last, counts)) = self.run {
            if down.is_none() || s.ts != last + 1 {
                done = Self::close(start, last, counts);
                self.run = None;
            }
        }
        if let Some(scope) = down {
            let (start, _, mut counts) = self.run.unwrap_or((s.ts, s.ts, [0; 4]));
            counts[scope as usize] += 1;
            self.run = Some((start, s.ts, counts));
        }
        done
    }

    /// Незакрытый обрыв при остановке мониторинга.
    pub fn flush(&mut self) -> Option<Incident> {
        self.run.take().and_then(|(start, last, counts)| Self::close(start, last, counts))
    }

    /// Идёт ли сейчас обрыв и с какой секунды.
    pub fn ongoing(&self) -> Option<i64> {
        self.run.map(|(start, _, _)| start)
    }

    fn close(start: i64, last: i64, counts: [u32; 4]) -> Option<Incident> {
        if last - start + 1 < MIN_OUTAGE {
            return None;
        }
        // Причина — где оборвалось чаще всего; при равенстве — что ближе.
        let scope = Scope::ALL.into_iter().max_by_key(|s| (counts[*s as usize], std::cmp::Reverse(*s as usize))).unwrap();
        Some(Incident { start, end: last, scope })
    }
}

/* ---------- агрегаты ---------------------------------------------------- */

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub sent: u32,
    pub lost: u32,
    pub sum: f64,
    pub max: f32,
    pub jit_sum: f64,
    pub jit_n: u32,
}

impl Layer {
    fn merge(&mut self, o: &Layer) {
        self.sent += o.sent;
        self.lost += o.lost;
        self.sum += o.sum;
        self.max = self.max.max(o.max);
        self.jit_sum += o.jit_sum;
        self.jit_n += o.jit_n;
    }

    pub fn avg(&self) -> Option<f64> {
        let ok = self.sent - self.lost;
        (ok > 0).then(|| self.sum / ok as f64)
    }

    pub fn loss(&self) -> Option<f64> {
        (self.sent > 0).then(|| self.lost as f64 * 100.0 / self.sent as f64)
    }

    pub fn jitter(&self) -> Option<f64> {
        (self.jit_n > 0).then(|| self.jit_sum / self.jit_n as f64)
    }

    fn to_json(&self) -> Value {
        json!({ "avg": self.avg(), "max": (self.sent > self.lost).then_some(self.max), "loss": self.loss(), "jitter": self.jitter() })
    }
}

/// Свёртка секунд за минуту, час или столбец графика. Все поля складываются,
/// поэтому минуты сворачиваются в часы, а часы — в столбцы без потерь.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Agg {
    pub ts: i64,
    /// Секунд с замерами; остальное — PLASTO был закрыт или компьютер спал.
    pub secs: u32,
    /// Секунд без интернета, по причинам (индекс — `Scope`).
    pub down: [u32; 4],
    pub layers: [Layer; 4],
    pub wifi_secs: u32,
    pub rssi_min: Option<i32>,
    pub rssi_sum: i64,
    pub rssi_n: u32,
    pub q_sum: u32,
    pub q_n: u32,
    pub rate_min: Option<u32>,
    pub link_min: Option<u32>,
    pub err: u32,
    pub disc: u32,
}

fn min_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

impl Agg {
    pub fn new(ts: i64) -> Self {
        Agg { ts, ..Default::default() }
    }

    /// Добавляет секунду. `prev` — предыдущая секунда, для джиттера.
    pub fn push(&mut self, s: &Sample, prev: Option<&Sample>) {
        self.secs += 1;
        if let Some(Err(scope)) = classify(s) {
            self.down[scope as usize] += 1;
        }
        let prev = prev.filter(|p| p.ts + 1 == s.ts);
        for (i, r) in s.rtt.iter().enumerate() {
            let l = &mut self.layers[i];
            match r {
                Rtt::Skip => {}
                Rtt::Lost => {
                    l.sent += 1;
                    l.lost += 1;
                }
                Rtt::Ms(v) => {
                    l.sent += 1;
                    l.sum += *v as f64;
                    l.max = l.max.max(*v);
                    if let Some(p) = prev.and_then(|p| p.rtt[i].ms()) {
                        l.jit_sum += (v - p).abs() as f64;
                        l.jit_n += 1;
                    }
                }
            }
        }
        if s.wifi {
            self.wifi_secs += 1;
        }
        if let Some(r) = s.rssi {
            self.rssi_min = min_opt(self.rssi_min, Some(r));
            self.rssi_sum += r as i64;
            self.rssi_n += 1;
        }
        if let Some(q) = s.quality {
            self.q_sum += q as u32;
            self.q_n += 1;
        }
        self.rate_min = min_opt(self.rate_min, s.rate);
        self.link_min = min_opt(self.link_min, s.link);
        self.err += s.err;
        self.disc += s.disc;
    }

    pub fn merge(&mut self, o: &Agg) {
        self.secs += o.secs;
        for i in 0..4 {
            self.down[i] += o.down[i];
            self.layers[i].merge(&o.layers[i]);
        }
        self.wifi_secs += o.wifi_secs;
        self.rssi_min = min_opt(self.rssi_min, o.rssi_min);
        self.rssi_sum += o.rssi_sum;
        self.rssi_n += o.rssi_n;
        self.q_sum += o.q_sum;
        self.q_n += o.q_n;
        self.rate_min = min_opt(self.rate_min, o.rate_min);
        self.link_min = min_opt(self.link_min, o.link_min);
        self.err += o.err;
        self.disc += o.disc;
    }

    pub fn down_secs(&self) -> u32 {
        self.down.iter().sum()
    }

    pub fn rssi_avg(&self) -> Option<f64> {
        (self.rssi_n > 0).then(|| self.rssi_sum as f64 / self.rssi_n as f64)
    }

    pub fn to_json(&self) -> Value {
        let mut layers = serde_json::Map::new();
        for (i, name) in LAYERS.iter().enumerate() {
            layers.insert((*name).into(), self.layers[i].to_json());
        }
        let down: serde_json::Map<String, Value> = Scope::ALL.iter().map(|s| (s.as_str().to_owned(), json!(self.down[*s as usize]))).collect();
        json!({
            "t": self.ts, "secs": self.secs, "down": self.down_secs(), "downBy": down, "layers": layers,
            "wifi": self.wifi_secs > 0, "rssi": self.rssi_avg(), "rssiMin": self.rssi_min,
            "quality": (self.q_n > 0).then(|| self.q_sum as f64 / self.q_n as f64),
            "rateMin": self.rate_min, "linkMin": self.link_min, "err": self.err, "disc": self.disc,
        })
    }
}

/// Сворачивает секунды в агрегаты по `step` секунд (минуты — 60, часы — 3600).
pub fn rollup(samples: &[Sample], step: i64) -> Vec<Agg> {
    let mut out: Vec<Agg> = Vec::new();
    let mut prev: Option<&Sample> = None;
    for s in samples {
        let ts = s.ts - s.ts.rem_euclid(step);
        if out.last().is_none_or(|a| a.ts != ts) {
            out.push(Agg::new(ts));
        }
        out.last_mut().unwrap().push(s, prev);
        prev = Some(s);
    }
    out
}

/// Раскладывает агрегаты по `n` столбцам на отрезке [from, to). Пустой
/// столбец (`secs == 0`) — данных нет, график рисует разрыв.
pub fn buckets(aggs: &[Agg], from: i64, to: i64, n: usize) -> Vec<Agg> {
    let n = n.max(1);
    let width = ((to - from) as f64 / n as f64).max(1.0);
    let mut out: Vec<Agg> = (0..n).map(|i| Agg::new(from + (i as f64 * width) as i64)).collect();
    for a in aggs {
        if a.ts < from || a.ts >= to {
            continue;
        }
        let i = (((a.ts - from) as f64 / width) as usize).min(n - 1);
        out[i].merge(a);
    }
    out
}

/* ---------- события ----------------------------------------------------- */

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub ts: i64,
    pub kind: String,
    pub detail: Value,
}

impl Event {
    pub fn new(ts: i64, kind: &str, detail: Value) -> Self {
        Event { ts, kind: kind.into(), detail }
    }

    pub fn to_json(&self) -> Value {
        json!({ "ts": self.ts, "kind": self.kind, "detail": self.detail })
    }
}

/// Где оборвалось — словами.
pub fn scope_text(scope: &str) -> &'static str {
    match scope {
        "link" => "адаптер без подключения",
        "local" => "не отвечал роутер",
        "isp" => "не отвечал провайдер",
        _ => "сбой дальше провайдера",
    }
}

/// Событие по-человечески и его важность: "bad", "warn" или "info".
/// None — служебное (линк вернулся, проснулся), его не показывают.
pub fn describe(kind: &str, d: &Value) -> Option<(String, &'static str)> {
    let text = |k: &str| d[k].as_str().filter(|t| !t.is_empty());
    Some(match kind {
        "outage" => (format!("Обрыв {}: {}", duration(d["secs"].as_i64().unwrap_or(0)), scope_text(d["scope"].as_str().unwrap_or(""))), "bad"),
        "link_down" => ("Пропало подключение адаптера".into(), "bad"),
        "link_speed" => (format!("Скорость линка {} → {} Мбит/с", d["from"], d["to"]), "warn"),
        "adapter_reset" => ("Сброс сетевого адаптера".into(), "warn"),
        "driver_warn" => (format!("Драйвер: {}", text("text").unwrap_or("предупреждение")), "warn"),
        "wifi_disc" => (format!("Wi-Fi отключился{}", text("reason").map(|r| format!(": {r}")).unwrap_or_default()), "warn"),
        "ports" => ("Закончились TCP-порты".into(), "warn"),
        "roam" => ("Wi-Fi перешёл на другую точку доступа".into(), "info"),
        "sleep" => ("Компьютер уснул".into(), "info"),
        "winws" => (if d["on"] == true { "Обход включён" } else { "Обход выключен" }.into(), "info"),
        "monitor" => (if d["on"] == true { "Мониторинг включён" } else { "Мониторинг выключен" }.into(), "info"),
        "gateway" => (format!("Роутер: {}", text("to").unwrap_or("нет")), "info"),
        "isp_hop" => (format!("Узел провайдера: {}", text("ip").unwrap_or("—")), "info"),
        _ => return None,
    })
}

fn count(events: &[Event], kind: &str) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

/// Моменты события без повторов: одну потерю линка видят и сборщик, и
/// журнал драйвера — это одна потеря, а не две.
fn moments(events: &[Event], kind: &str, merge: i64) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for e in events.iter().filter(|e| e.kind == kind) {
        if out.last().is_none_or(|&t| e.ts - t > merge) {
            out.push(e.ts);
        }
    }
    out
}

/// Сброс адаптера рядом с потерей линка — её следствие: драйвер Intel,
/// например, сбрасывает адаптер, пока кабель не вернётся.
const RESET_NEAR_LINK: i64 = 120;

/// Сводка и вывод за период: по ней рисуются карточки и строка вердикта.
/// `adapter` — что известно об адаптере сейчас (драйвер, энергосбережение).
pub fn summarize(total: &Agg, events: &[Event], adapter: &Value) -> Value {
    let outages: Vec<&Event> = events.iter().filter(|e| e.kind == "outage").collect();
    let mut by = [(0u32, 0i64); 4];
    for e in &outages {
        if let Some(s) = e.detail["scope"].as_str().and_then(Scope::parse) {
            by[s as usize].0 += 1;
            by[s as usize].1 += e.detail["secs"].as_i64().unwrap_or(0);
        }
    }
    let up_pct = (total.secs > 0).then(|| (total.secs - total.down_secs()) as f64 * 100.0 / total.secs as f64);
    let links = moments(events, "link_down", 10);
    let link_drops = links.len();
    let resets_at = moments(events, "adapter_reset", 0);
    let resets_link = resets_at.iter().filter(|&&r| links.iter().any(|&l| (r - l).abs() <= RESET_NEAR_LINK)).count();
    let resets = resets_at.len() - resets_link;
    let slowdowns = events.iter().filter(|e| e.kind == "link_speed" && e.detail["to"].as_u64() < e.detail["from"].as_u64()).count();
    let wifi_disc = count(events, "wifi_disc");
    let roams = count(events, "roam");
    let driver = count(events, "driver_warn");
    let ports = count(events, "ports");
    let sleeps = count(events, "sleep");

    // Вердикт: сначала то, что объясняет обрывы, потом — что настораживает.
    let mut lines: Vec<String> = Vec::new();
    let mut tone = "ok";
    let head = if total.secs == 0 && link_drops == 0 {
        tone = "idle";
        "За этот период данных нет — мониторинг не работал".to_owned()
    } else if outages.is_empty() && link_drops > 0 {
        // Журнал помнит больше, чем успел намерить мониторинг.
        "Замеры обрывов не застали, но по журналу Windows связь пропадала".to_owned()
    } else if outages.is_empty() {
        "Обрывов не было".to_owned()
    } else {
        tone = "bad";
        let secs: i64 = by.iter().map(|b| b.1).sum();
        format!("{} {}, без интернета {}", outages.len(), plural(outages.len() as u64, "обрыв", "обрыва", "обрывов"), duration(secs))
    };
    let main = Scope::ALL.into_iter().filter(|s| by[*s as usize].0 > 0).max_by_key(|s| by[*s as usize].1);
    if let Some(scope) = main {
        let (n, _) = by[scope as usize];
        let share = if n as usize == outages.len() { "Все".to_owned() } else { format!("{n} из {}", outages.len()) };
        lines.push(match scope {
            Scope::Link => format!("{share} — адаптер терял подключение: отключался кабель или Wi-Fi. Это на стороне компьютера, не провайдера."),
            Scope::Local if total.wifi_secs > 0 => format!("{share} — не отвечал роутер, а компьютер подключён по Wi-Fi: скорее всего, дело в беспроводной связи или роутере."),
            Scope::Local => format!("{share} — не отвечал роутер: проблема в домашней сети — кабель, роутер или его питание."),
            Scope::Isp => format!("{share} — на стороне провайдера: роутер отвечал, а первый узел провайдера — нет."),
            Scope::Internet => format!("{share} — дальше домашней сети: роутер и узел провайдера отвечали, а внешние адреса — нет."),
        });
    }
    let inet_loss = total.layers[2].loss().unwrap_or(0.0);
    if total.secs > 0 && inet_loss >= 1.0 {
        tone = if tone == "ok" { "warn" } else { tone };
        lines.push(format!("Потери пакетов до интернета {}% — для игр и звонков заметно уже от 1%.", fmt1(inet_loss)));
    }
    let gw_loss = total.layers[0].loss().unwrap_or(0.0);
    if total.wifi_secs > 0 && gw_loss >= 1.0 {
        tone = if tone == "ok" { "warn" } else { tone };
        lines.push(format!("Даже до роутера теряется {}% пакетов — это Wi-Fi, провайдер тут ни при чём.", fmt1(gw_loss)));
    }
    if let (Some(avg), Some(min)) = (total.rssi_avg(), total.rssi_min) {
        if avg < -70.0 || min < -80 {
            tone = if tone == "ok" { "warn" } else { tone };
            lines.push(format!("Сигнал Wi-Fi слабый: в среднем {avg:.0} dBm, падал до {min} dBm. Хороший — от −60."));
        }
    }
    if link_drops > 0 {
        tone = if tone == "ok" { "warn" } else { tone };
        let after = if resets_link > 0 { format!(", после этого адаптер сбрасывался ещё {resets_link} {}", plural(resets_link as u64, "раз", "раза", "раз")) } else { String::new() };
        let cause = if total.wifi_secs > 0 { "Wi-Fi-адаптер терял подключение" } else { "адаптер терял линк — кабель переставал работать" };
        lines.push(format!(
            "Подключение к роутеру пропадало {link_drops} {}: {cause}{after}. Это до провайдера: проверьте кабель, разъёмы, роутер и его питание.",
            plural(link_drops as u64, "раз", "раза", "раз")
        ));
    }
    if resets > 0 {
        tone = if tone == "ok" { "warn" } else { tone };
        lines.push(format!("Сетевой адаптер сбрасывался {resets} {} без потери линка — обычно это драйвер: стоит обновить его с сайта производителя.", plural(resets as u64, "раз", "раза", "раз")));
    }
    if slowdowns > 0 {
        tone = if tone == "ok" { "warn" } else { tone };
        lines.push(format!("Скорость линка падала {slowdowns} {} (например, с 1 Гбит/с до 100 Мбит/с) — признак плохого кабеля или разъёма.", plural(slowdowns as u64, "раз", "раза", "раз")));
    }
    if driver > 0 {
        lines.push(format!("Драйвер адаптера записал {driver} {} в системный журнал.", plural(driver as u64, "предупреждение", "предупреждения", "предупреждений")));
    }
    if ports > 0 {
        tone = if tone == "ok" { "warn" } else { tone };
        lines.push("В системе заканчивались TCP-порты: какая-то программа открывает слишком много соединений, новые не устанавливаются.".into());
    }
    if adapter["powerSaving"] == true && (link_drops > 0 || resets > 0) {
        lines.push("Windows разрешено отключать адаптер для экономии энергии — на ноутбуках это частая причина отвалов. Отключается в свойствах адаптера, вкладка «Управление электропитанием».".into());
    }

    json!({
        "head": head, "tone": tone, "lines": lines,
        "secs": total.secs, "upPct": up_pct, "downSecs": total.down_secs(),
        "outages": outages.len(),
        "outagesBy": Scope::ALL.iter().map(|s| (s.as_str().to_owned(), json!({ "count": by[*s as usize].0, "secs": by[*s as usize].1 }))).collect::<serde_json::Map<_, _>>(),
        "agg": total.to_json(),
        "events": {
            "resets": resets, "resetsAfterLink": resets_link, "linkDrops": link_drops, "slowdowns": slowdowns, "wifiDisc": wifi_disc,
            "roams": roams, "driver": driver, "ports": ports, "sleeps": sleeps,
        },
    })
}

pub fn fmt1(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

pub fn plural(n: u64, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (d10, d100) = (n % 10, n % 100);
    if d10 == 1 && d100 != 11 {
        one
    } else if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
        few
    } else {
        many
    }
}

pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs} с")
    } else if secs < 3600 {
        format!("{} мин {} с", secs / 60, secs % 60)
    } else {
        format!("{} ч {} мин", secs / 3600, secs % 3600 / 60)
    }
}

/* ---------- время ------------------------------------------------------- */

/// Unix-время из `2026-10-16T14:11:24.1234567Z` (так пишет журнал событий).
pub fn parse_utc(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Дни от эпохи по гражданскому календарю (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, gw: Rtt, isp: Rtt, inet: Rtt, udp: Rtt) -> Sample {
        Sample { ts, rtt: [gw, isp, inet, udp], up: Some(true), ..Default::default() }
    }
    const OK: Rtt = Rtt::Ms(10.0);
    const L: Rtt = Rtt::Lost;
    const S: Rtt = Rtt::Skip;

    #[test]
    fn секунда_без_интернета_относится_к_ближайшему_молчащему_узлу() {
        assert_eq!(classify(&sample(0, OK, OK, OK, OK)), Some(Ok(())));
        // Интернет есть хоть по UDP — значит есть, даже если ICMP режут.
        assert_eq!(classify(&sample(0, L, L, L, OK)), Some(Ok(())));
        assert_eq!(classify(&sample(0, L, L, L, L)), Some(Err(Scope::Local)));
        assert_eq!(classify(&sample(0, OK, L, L, L)), Some(Err(Scope::Isp)));
        assert_eq!(classify(&sample(0, OK, OK, L, L)), Some(Err(Scope::Internet)));
        // Узел провайдера не найден — граница «дальше роутера».
        assert_eq!(classify(&sample(0, OK, S, L, L)), Some(Err(Scope::Internet)));
        assert_eq!(classify(&sample(0, S, S, S, S)), None);
        let mut s = sample(0, L, L, L, L);
        s.up = Some(false);
        assert_eq!(classify(&s), Some(Err(Scope::Link)));
    }

    #[test]
    fn обрыв_это_две_секунды_подряд_и_разрыв_во_времени_его_закрывает() {
        let mut t = Tracker::default();
        let seq = [
            sample(1, OK, OK, OK, OK),
            sample(2, OK, L, L, L), // одна секунда — потеря, не обрыв
            sample(3, OK, OK, OK, OK),
            sample(4, OK, L, L, L),
            sample(5, OK, L, L, L),
            sample(6, L, L, L, L),
            sample(7, OK, OK, OK, OK),
            sample(8, OK, OK, L, L),
            sample(9, OK, OK, L, L),
            // компьютер спал: пауза закрывает серию
            sample(100, OK, OK, L, L),
        ];
        let got: Vec<Incident> = seq.iter().filter_map(|s| t.feed(s)).collect();
        assert_eq!(got, vec![Incident { start: 4, end: 6, scope: Scope::Isp }, Incident { start: 8, end: 9, scope: Scope::Internet }]);
        assert_eq!(t.ongoing(), Some(100));
        assert_eq!(t.flush(), None, "одна секунда после паузы — не обрыв");
    }

    #[test]
    fn агрегат_считает_потери_джиттер_и_складывается() {
        let mut s = vec![sample(60, Rtt::Ms(10.0), S, Rtt::Ms(20.0), OK), sample(61, Rtt::Ms(14.0), S, L, L), sample(62, L, S, Rtt::Ms(30.0), OK)];
        s[0].rssi = Some(-60);
        s[1].rssi = Some(-70);
        s[1].err = 3;
        let a = rollup(&s, 60);
        assert_eq!(a.len(), 1);
        let a = &a[0];
        assert_eq!((a.ts, a.secs), (60, 3));
        assert_eq!(a.layers[0].loss().unwrap().round(), 33.0);
        assert_eq!(a.layers[0].avg(), Some(12.0));
        assert_eq!(a.layers[0].jitter(), Some(4.0));
        assert_eq!(a.layers[1].sent, 0);
        assert_eq!(a.down, [0, 0, 0, 1]);
        assert_eq!((a.rssi_min, a.rssi_avg(), a.err), (Some(-70), Some(-65.0), 3));
        let mut twice = a.clone();
        twice.merge(a);
        assert_eq!((twice.secs, twice.layers[0].sent, twice.rssi_min), (6, 6, Some(-70)));
    }

    #[test]
    fn столбцы_графика_покрывают_отрезок_а_пустые_значат_нет_данных() {
        let aggs: Vec<Agg> = [0, 60, 120, 600].iter().map(|&ts| Agg { ts, secs: 60, ..Default::default() }).collect();
        let b = buckets(&aggs, 0, 1200, 4);
        assert_eq!(b.iter().map(|a| a.ts).collect::<Vec<_>>(), vec![0, 300, 600, 900]);
        assert_eq!(b.iter().map(|a| a.secs).collect::<Vec<_>>(), vec![180, 0, 60, 0]);
    }

    #[test]
    fn вердикт_называет_главную_причину() {
        let total = Agg { secs: 3600, down: [0, 0, 40, 0], ..Default::default() };
        let ev = |scope: &str, secs: i64| Event::new(0, "outage", json!({ "scope": scope, "secs": secs }));
        let events = vec![ev("isp", 30), ev("isp", 10), ev("local", 5), Event::new(0, "adapter_reset", json!({}))];
        let v = summarize(&total, &events, &json!({}));
        assert_eq!(v["tone"], "bad");
        assert_eq!(v["head"], "3 обрыва, без интернета 45 с");
        assert!(v["lines"][0].as_str().unwrap().starts_with("2 из 3 — на стороне провайдера"));
        assert!(v["lines"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("сбрасывался 1 раз без потери линка")));
        assert_eq!(summarize(&Agg::default(), &[], &json!({}))["tone"], "idle");
        assert_eq!(summarize(&Agg { secs: 10, ..Default::default() }, &[], &json!({}))["head"], "Обрывов не было");
    }

    #[test]
    fn сброс_после_потери_линка_не_винит_драйвер() {
        let total = Agg { secs: 60, ..Default::default() };
        let events = vec![
            Event::new(100, "link_down", json!({})),
            Event::new(103, "link_down", json!({ "from": "journal" })), // та же потеря из журнала
            Event::new(118, "adapter_reset", json!({})),
            Event::new(153, "adapter_reset", json!({})),
            Event::new(5000, "adapter_reset", json!({})),
        ];
        let v = summarize(&total, &events, &json!({ "powerSaving": true }));
        assert_eq!((v["events"]["linkDrops"].as_u64(), v["events"]["resetsAfterLink"].as_u64(), v["events"]["resets"].as_u64()), (Some(1), Some(2), Some(1)));
        let lines: Vec<&str> = v["lines"].as_array().unwrap().iter().filter_map(|l| l.as_str()).collect();
        assert!(lines[0].starts_with("Подключение к роутеру пропадало 1 раз") && lines[0].contains("сбрасывался ещё 2 раза"), "{lines:?}");
        assert!(lines[1].contains("сбрасывался 1 раз без потери линка"));
        assert!(lines.iter().any(|l| l.contains("экономии энергии")));
        assert!(v["head"].as_str().unwrap().contains("по журналу"));
    }

    #[test]
    fn время_журнала() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-10-16T14:11:24.1234567Z"), Some(1_792_159_884));
        assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_utc("мусор"), None);
    }

    #[test]
    fn длительность_и_склонения() {
        assert_eq!(duration(45), "45 с");
        assert_eq!(duration(125), "2 мин 5 с");
        assert_eq!(duration(3720), "1 ч 2 мин");
        assert_eq!(plural(21, "обрыв", "обрыва", "обрывов"), "обрыв");
        assert_eq!(plural(12, "обрыв", "обрыва", "обрывов"), "обрывов");
    }
}
