//! Мониторинг стабильности: что было со связью, секунда за секундой, и где
//! именно она рвалась — чтобы показать провайдеру не «у меня лагает», а
//! «21:43:07–21:43:21 роутер отвечал, а первый узел провайдера — нет».
//!
//! Раз в секунду, параллельно:
//! - ICMP до роутера (шлюза по умолчанию), первого узла провайдера (найден
//!   трассировкой) и трёх внешних адресов — Яндекс DNS, Cloudflare, Google;
//! - DNS-запрос по UDP: игры ходят по UDP, а ICMP маршрутизаторы режут
//!   отдельно, поэтому интернет «есть», если ответил хоть кто-то из внешних;
//! - состояние адаптера (линк, скорость, ошибки) и Wi-Fi (сигнал, скорость,
//!   точка доступа).
//! Раз в 30 секунд — системный журнал: сбросы адаптера, отключения Wi-Fi,
//! сон, предупреждения драйвера, нехватка TCP-портов.
//!
//! Сбор идёт, пока PLASTO запущен (хотя бы в трее). Пропуски честно остаются
//! пропусками: на графике это «нет данных», а не обрыв.

mod analyze;
mod report;
mod store;
mod win;

use std::{
    net::{Ipv4Addr, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

pub use analyze::{describe, duration, fmt1, plural, scope_text, Agg, Event, Incident, Rtt, Sample, Scope};
pub use report::earlier_outages;
use analyze::{buckets, rollup, summarize, Tracker};
use store::{Store, HOUR, MINUTE};

use crate::host::Host;

/// Внешние адреса: Яндекс DNS, Cloudflare, Google. Разные сети, чтобы
/// сбой одной не выглядел как обрыв интернета.
const EXTERNAL: [Ipv4Addr; 3] = [Ipv4Addr::new(77, 88, 8, 8), Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];
const DNS: Ipv4Addr = Ipv4Addr::new(77, 88, 8, 8);
const PROBE_TIMEOUT_MS: u32 = 900;
/// Как часто сбрасывать секунды на диск: график «5 минут» читает из базы.
const FLUSH_EVERY: i64 = 2;
const EVENTS_EVERY: i64 = 30;
const ISP_EVERY: i64 = 600;
const ADAPTER_EVERY: i64 = 600;
/// При первом запуске журнал событий читается на месяц назад: сбросы
/// адаптера и отвалы Wi-Fi видны сразу, ещё до первых замеров.
const HISTORY: i64 = 30 * 86400;

static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static LIVE: Mutex<Option<Value>> = Mutex::new(None);

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn db_path(app: &Host) -> Result<PathBuf, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    Ok(root.join("stability.db"))
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

pub fn start(app: &Host) -> Value {
    if !RUNNING.swap(true, Ordering::AcqRel) {
        STOP.store(false, Ordering::Release);
        let app = app.clone();
        std::thread::Builder::new().name("stability".into()).spawn(move || collector(app)).ok();
    }
    status(app)
}

pub fn stop() -> Value {
    STOP.store(true, Ordering::Release);
    json!(true)
}

pub fn status(app: &Host) -> Value {
    let first = db_path(app).ok().and_then(|p| Store::open(&p).ok()).and_then(|s| s.first_ts());
    json!({ "running": running(), "firstTs": first, "live": LIVE.lock().ok().and_then(|l| l.clone()) })
}

/// Секунда «прямо сейчас» — без базы, для шапки вкладки раз в 2 секунды.
pub fn live() -> Value {
    json!({ "running": running(), "live": LIVE.lock().ok().and_then(|l| l.clone()) })
}

pub fn clear(app: &Host) -> Value {
    match db_path(app).and_then(|p| Store::open(&p)).and_then(|mut s| s.clear()) {
        Ok(()) => json!({ "ok": true }),
        Err(e) => json!({ "error": e }),
    }
}

/// Имя файла отчёта из локального «29.10.2026 05:30».
fn report_name(stamp: &str, fallback: i64) -> String {
    let parts: Vec<&str> = stamp.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [d, m, y, hh, mm, ..] => format!("plasto-stability-{y}-{m}-{d}_{hh}-{mm}.html"),
        _ => format!("plasto-stability-{fallback}.html"),
    }
}

/// Отчёт за период — HTML в папке данных PLASTO, сразу открывается.
pub fn export(app: &Host, payload: &Value) -> Value {
    let q = query(app, payload);
    if q["error"].is_string() {
        return q;
    }
    let stamp = |ts: i64| crate::platform::local_stamp((ts.max(0) as u64) * 1000);
    let html = report::render(&q, &stamp);
    let saved = app.path().app_data_dir().map_err(|e| e.to_string()).and_then(|root| {
        let dir = root.join("reports");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(report_name(&stamp(now()), now()));
        std::fs::write(&file, html).map_err(|e| e.to_string())?;
        Ok(file)
    });
    match saved {
        Ok(file) => {
            // Через проводник: браузер откроется обычным, а не с правами администратора.
            #[cfg(windows)]
            let _ = std::process::Command::new("explorer.exe").arg(&file).spawn();
            json!({ "path": file.to_string_lossy() })
        }
        Err(e) => json!({ "error": e }),
    }
}

/* ---------- запрос для страницы ---------------------------------------- */

/// Длина периода и число столбцов графика для фильтра.
pub fn range(name: &str) -> (i64, usize) {
    match name {
        "5m" => (300, 300),
        "15m" => (900, 300),
        "1h" => (3600, 360),
        "1w" => (7 * 86400, 336),
        "1mo" => (30 * 86400, 360),
        "1y" => (365 * 86400, 365),
        _ => (86400, 288),
    }
}

/// Агрегаты за [from, to) с подходящей подробностью: секунды для часа и
/// короче, минуты до недели, часы дальше. Хвост, который сборщик ещё не
/// свернул, добирается из более подробного уровня.
fn load(store: &Store, from: i64, to: i64) -> Result<Vec<Agg>, String> {
    let span = to - from;
    let minutes_done = store.meta("minutes_done").and_then(|v| v.parse().ok()).unwrap_or(from);
    let hours_done = store.meta("hours_done").and_then(|v| v.parse().ok()).unwrap_or(from);
    if span <= 3600 {
        return Ok(rollup(&store.raw(from, to)?, 1));
    }
    let raw_tail = |from: i64| store.raw(from.max(minutes_done), to).map(|r| rollup(&r, 60));
    if span <= 7 * 86400 {
        let mut out = store.aggs(MINUTE, from, minutes_done.max(from))?;
        out.extend(raw_tail(from)?);
        return Ok(out);
    }
    let mut out = store.aggs(HOUR, from, hours_done.max(from))?;
    out.extend(store.aggs(MINUTE, hours_done.max(from), minutes_done.max(from))?);
    out.extend(raw_tail(from)?);
    Ok(out)
}

pub fn query(app: &Host, payload: &Value) -> Value {
    let name = payload["range"].as_str().unwrap_or("1d");
    let (span, n) = range(name);
    let to = now() + 1;
    let from = to - span;
    let result = db_path(app).and_then(|p| Store::open(&p)).and_then(|store| {
        let aggs = load(&store, from, to)?;
        let mut total = Agg::new(from);
        for a in &aggs {
            total.merge(a);
        }
        let events = store.events(from, to)?;
        let live = LIVE.lock().ok().and_then(|l| l.clone()).unwrap_or(Value::Null);
        let summary = summarize(&total, &events, &live["adapter"]);
        Ok(json!({
            "range": name, "from": from, "to": to, "step": span as f64 / n as f64,
            "buckets": buckets(&aggs, from, to, n).iter().map(Agg::to_json).collect::<Vec<_>>(),
            // Самые свежие — последними; на год событий может быть много.
            "events": events.iter().rev().take(500).rev().map(Event::to_json).collect::<Vec<_>>(),
            "summary": summary,
            "firstTs": store.first_ts(),
            "running": running(),
            "live": live,
        }))
    });
    result.unwrap_or_else(|e| json!({ "error": e }))
}

/* ---------- замеры ------------------------------------------------------ */

fn echo(to: Ipv4Addr) -> Rtt {
    match win::ping(to, None, PROBE_TIMEOUT_MS) {
        win::Echo::Reply { ms, .. } => Rtt::Ms(ms),
        _ => Rtt::Lost,
    }
}

/// DNS-запрос A для ya.ru по UDP: путь, которым ходят игры и звонки.
fn dns_probe(id: u16) -> Rtt {
    let mut q = vec![(id >> 8) as u8, id as u8, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["ya", "ru"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    let run = || -> std::io::Result<Rtt> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.set_read_timeout(Some(Duration::from_millis(PROBE_TIMEOUT_MS as u64)))?;
        let start = std::time::Instant::now();
        sock.send_to(&q, (DNS, 53))?;
        let mut buf = [0u8; 512];
        loop {
            let (n, _) = sock.recv_from(&mut buf)?;
            if n >= 2 && buf[0] == (id >> 8) as u8 && buf[1] == id as u8 {
                return Ok(Rtt::Ms(start.elapsed().as_secs_f32() * 1000.0));
            }
        }
    };
    run().unwrap_or(Rtt::Lost)
}

/// Домашние адреса: за роутером бывает второй роутер, и он ещё не провайдер.
fn home(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 192 && o[1] == 168 || o[0] == 172 && (16..32).contains(&o[1])
}

/// Первый узел провайдера: трассировка до Яндекс DNS, первый после роутера
/// не домашний узел, который отвечает на эхо (иначе он молчал бы и при
/// живом интернете — и любой обрыв выглядел бы как вина провайдера).
fn find_isp(gw: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
    let mut seen_gw = gw.is_none();
    for ttl in 1..=8u8 {
        match win::ping(EXTERNAL[0], Some(ttl), 1000) {
            win::Echo::Expired { from } => {
                if Some(from) == gw {
                    seen_gw = true;
                    continue;
                }
                if !seen_gw || home(from) {
                    continue;
                }
                let answers = (0..3).filter(|_| matches!(win::ping(from, None, 1000), win::Echo::Reply { .. })).count();
                if answers >= 2 {
                    return Some(from);
                }
            }
            win::Echo::Reply { .. } => return None,
            win::Echo::Timeout => {}
        }
    }
    None
}

/// Что сборщик помнит между секундами, чтобы замечать перемены.
#[derive(Default)]
struct Ctx {
    index: Option<u32>,
    iface: Option<win::Iface>,
    gw: Option<Ipv4Addr>,
    isp: Option<Ipv4Addr>,
    counters: Option<(u32, u64, u64)>,
    bssid: Option<String>,
    winws: Option<bool>,
    adapter: Value,
    adapter_at: i64,
}

fn collector(app: Host) {
    let _guard = Guard;
    let opened = db_path(&app).and_then(|p| Store::open(&p));
    let mut store = match opened {
        Ok(s) => s,
        Err(e) => {
            let _ = app.emit("plasto:stab:error", json!({ "error": e }));
            return;
        }
    };
    let t0 = now();
    let log = |store: &mut Store, e: Event| {
        let _ = store.add_event(&e);
        let _ = app.emit("plasto:stab:event", e.to_json());
    };
    log(&mut store, Event::new(t0, "monitor", json!({ "on": true })));
    // Что не успел свернуть прошлый запуск (его могли закрыть посреди минуты).
    let _ = fill(&mut store, t0);
    // Журнал — когда уже известен драйвер: его старые предупреждения тоже нужны.
    let mut driver = win::route(EXTERNAL[0]).and_then(|(i, _)| win::iface(i)).and_then(|i| win::adapter_info(&i.guid)["service"].as_str().map(str::to_owned));
    poll_journal(&mut store, &app, driver.as_deref());

    let wlan = win::Wlan::open();
    let isp: Arc<Mutex<Option<Ipv4Addr>>> = Arc::default();
    let isp_busy = Arc::new(AtomicBool::new(false));
    let mut ctx = Ctx::default();
    let mut tracker = Tracker::default();
    let mut pending: Vec<Sample> = Vec::new();
    // 0 — «давно»: `ts - 0` больше любого интервала (а i64::MIN переполнился бы).
    let (mut last_ts, mut isp_at, mut events_at) = (0i64, 0i64, t0);
    let mut dns_id: u16 = (t0 & 0xffff) as u16;
    // Для главной: сколько обрывов с запуска мониторинга.
    let mut outages = 0u32;

    while !STOP.load(Ordering::Acquire) {
        let ts = now();
        if ts == last_ts {
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_millis();
            std::thread::sleep(Duration::from_millis((1000 - ms as u64).max(5)));
            continue;
        }

        // Маршрут и адаптер. Без маршрута (кабель выдернут) — тот же адаптер,
        // что был: он и покажет, что линка нет.
        let route = win::route(EXTERNAL[0]);
        let index = route.map(|r| r.0).or(ctx.index);
        let gw = route.and_then(|r| r.1);
        let ifc = index.and_then(win::iface);
        if let Some(i) = &ifc {
            if ctx.iface.as_ref().map(|o| &o.guid) != Some(&i.guid) {
                log(&mut store, Event::new(ts, "iface", json!({ "name": i.name, "wifi": i.wifi })));
                ctx.adapter_at = 0;
                ctx.counters = None;
            }
            if let Some(o) = &ctx.iface {
                if o.guid == i.guid {
                    if o.up && !i.up {
                        log(&mut store, Event::new(ts, "link_down", json!({ "name": i.name })));
                    } else if !o.up && i.up {
                        log(&mut store, Event::new(ts, "link_up", json!({ "name": i.name, "mbps": i.link_mbps })));
                    } else if o.up && i.up && !i.wifi && o.link_mbps > 0 && i.link_mbps > 0 && o.link_mbps != i.link_mbps {
                        // У Wi-Fi скорость гуляет постоянно; у кабеля смена — событие.
                        log(&mut store, Event::new(ts, "link_speed", json!({ "from": o.link_mbps, "to": i.link_mbps })));
                    }
                }
            }
            if ts - ctx.adapter_at >= ADAPTER_EVERY {
                ctx.adapter = win::adapter_info(&i.guid);
                ctx.adapter_at = ts;
                driver = ctx.adapter["service"].as_str().map(str::to_owned);
            }
        }
        if gw != ctx.gw && route.is_some() {
            if ctx.gw.is_some() || gw.is_some() {
                log(&mut store, Event::new(ts, "gateway", json!({ "from": ctx.gw.map(|g| g.to_string()), "to": gw.map(|g| g.to_string()) })));
            }
            ctx.gw = gw;
            isp_at = 0;
        }
        ctx.index = index;

        // Узел провайдера ищется в фоне: трассировка идёт до десяти секунд.
        if ts - isp_at >= ISP_EVERY && !isp_busy.swap(true, Ordering::AcqRel) {
            isp_at = ts;
            let (slot, busy, gw) = (isp.clone(), isp_busy.clone(), ctx.gw);
            std::thread::spawn(move || {
                *slot.lock().unwrap() = find_isp(gw);
                busy.store(false, Ordering::Release);
            });
        }
        let isp_ip = *isp.lock().unwrap();
        if isp_ip.is_some() && isp_ip != ctx.isp {
            log(&mut store, Event::new(ts, "isp_hop", json!({ "ip": isp_ip.map(|i| i.to_string()) })));
        }
        ctx.isp = isp_ip;

        // Пробы — параллельно, каждая не дольше PROBE_TIMEOUT_MS.
        dns_id = dns_id.wrapping_add(1);
        let (mut g, mut p, mut ext, mut u) = (Rtt::Skip, Rtt::Skip, [Rtt::Lost; 3], Rtt::Lost);
        std::thread::scope(|s| {
            let hg = gw.map(|ip| s.spawn(move || echo(ip)));
            let hp = isp_ip.map(|ip| s.spawn(move || echo(ip)));
            let he: Vec<_> = EXTERNAL.iter().map(|&ip| s.spawn(move || echo(ip))).collect();
            let hu = s.spawn(move || dns_probe(dns_id));
            g = hg.map_or(Rtt::Skip, |h| h.join().unwrap_or(Rtt::Lost));
            p = hp.map_or(Rtt::Skip, |h| h.join().unwrap_or(Rtt::Lost));
            for (i, h) in he.into_iter().enumerate() {
                ext[i] = h.join().unwrap_or(Rtt::Lost);
            }
            u = hu.join().unwrap_or(Rtt::Lost);
        });
        let inet = ext.iter().filter_map(|r| r.ms()).min_by(f32::total_cmp).map_or(Rtt::Lost, Rtt::Ms);

        // Адаптер: ошибки за секунду — разница счётчиков.
        let mut sample = Sample { ts, rtt: [g, p, inet, u], ..Default::default() };
        if let Some(i) = &ifc {
            sample.up = Some(i.up);
            sample.wifi = i.wifi;
            sample.link = (i.link_mbps > 0).then_some(i.link_mbps);
            if let Some((idx, e, d)) = ctx.counters {
                if idx == i.index {
                    sample.err = i.errors.saturating_sub(e).min(u32::MAX as u64) as u32;
                    sample.disc = i.discards.saturating_sub(d).min(u32::MAX as u64) as u32;
                }
            }
            ctx.counters = Some((i.index, i.errors, i.discards));
        }
        let wifi = ifc.as_ref().filter(|i| i.wifi).and_then(|i| wlan.as_ref()?.state(&i.guid));
        if let Some(w) = wifi.as_ref().filter(|w| w.connected) {
            sample.rssi = w.rssi;
            sample.quality = Some(w.quality);
            sample.rate = Some(w.rx_mbps);
            if ctx.bssid.as_ref().is_some_and(|b| *b != w.bssid) {
                log(&mut store, Event::new(ts, "roam", json!({ "ssid": w.ssid, "from": ctx.bssid, "to": w.bssid })));
            }
            ctx.bssid = Some(w.bssid.clone());
        }

        // Обход: обрывы при включённом и выключенном winws — разные истории.
        let winws = crate::runtime::snapshot(&app)["running"] == true;
        if ctx.winws.is_some_and(|w| w != winws) {
            log(&mut store, Event::new(ts, "winws", json!({ "on": winws })));
        }
        ctx.winws = Some(winws);

        if let Some(inc) = tracker.feed(&sample) {
            record_incident(&mut store, &app, &inc);
            outages += 1;
        }
        let state = analyze::classify(&sample);
        let live = json!({
            "ts": ts,
            "state": match state { Some(Ok(())) => "ok", Some(Err(_)) => "down", None => "unknown" },
            "scope": state.and_then(|s| s.err()).map(Scope::as_str),
            "downSince": tracker.ongoing(),
            "gw": g.ms(), "isp": p.ms(), "inet": inet.ms(), "udp": u.ms(),
            "gateway": gw.map(|g| g.to_string()), "ispHop": isp_ip.map(|i| i.to_string()),
            "iface": ifc.as_ref().map(|i| json!({ "name": i.name, "wifi": i.wifi, "up": i.up, "mbps": i.link_mbps })),
            "wifi": wifi.as_ref().filter(|w| w.connected).map(|w| json!({ "ssid": w.ssid, "rssi": w.rssi, "quality": w.quality, "mbps": w.rx_mbps, "channel": w.channel })),
            "adapter": ctx.adapter,
            "winws": winws,
            "since": t0, "outages": outages,
        });
        *LIVE.lock().unwrap() = Some(live);
        ctx.iface = ifc;

        pending.push(sample);
        if ts - pending[0].ts >= FLUSH_EVERY {
            let _ = store.insert_raw(&pending);
            pending.clear();
        }
        // Новая минута: прошлую — в агрегаты (и час, если он кончился).
        if last_ts != 0 && ts / 60 != last_ts / 60 {
            let _ = store.insert_raw(&pending);
            pending.clear();
            let _ = fill(&mut store, ts);
        }
        if ts - events_at >= EVENTS_EVERY {
            events_at = ts;
            poll_journal(&mut store, &app, driver.as_deref());
        }
        last_ts = ts;
    }

    let _ = store.insert_raw(&pending);
    if let Some(inc) = tracker.flush() {
        record_incident(&mut store, &app, &inc);
    }
    let end = now();
    let _ = fill(&mut store, end);
    log(&mut store, Event::new(end, "monitor", json!({ "on": false })));
    *LIVE.lock().unwrap() = None;
    let _ = app.emit("plasto:stab:stopped", json!({}));
}

fn record_incident(store: &mut Store, app: &Host, inc: &Incident) {
    let e = Event::new(inc.start, "outage", json!({ "end": inc.end, "secs": inc.secs(), "scope": inc.scope.as_str() }));
    let _ = store.add_event(&e);
    let _ = app.emit("plasto:stab:incident", e.to_json());
}

/// Сворачивает секунды в минуты, а минуты в часы — всё, что закончилось к
/// `now` и ещё не свёрнуто. Отметки хранятся в базе: закрытый посреди минуты
/// PLASTO досчитает её при следующем запуске.
fn fill(store: &mut Store, now: i64) -> Result<(), String> {
    let minute = now - now.rem_euclid(60);
    let from = store.meta("minutes_done").and_then(|v| v.parse().ok()).unwrap_or(minute - store::RAW_KEEP).max(minute - store::RAW_KEEP);
    if from < minute {
        for a in rollup(&store.raw(from, minute)?, 60) {
            store.put_agg(MINUTE, &a)?;
        }
        store.set_meta("minutes_done", &minute.to_string())?;
    }
    let hour = now - now.rem_euclid(3600);
    let from = store.meta("hours_done").and_then(|v| v.parse().ok()).unwrap_or(hour - store::MINUTE_KEEP).max(hour - store::MINUTE_KEEP);
    if from < hour {
        let mut by_hour: Vec<Agg> = Vec::new();
        for m in store.aggs(MINUTE, from, hour)? {
            let h = m.ts - m.ts.rem_euclid(3600);
            if by_hour.last().is_none_or(|a| a.ts != h) {
                by_hour.push(Agg::new(h));
            }
            by_hour.last_mut().unwrap().merge(&m);
        }
        for a in &by_hour {
            store.put_agg(HOUR, a)?;
        }
        store.set_meta("hours_done", &hour.to_string())?;
        store.prune(now)?;
    }
    Ok(())
}

/* ---------- журнал событий --------------------------------------------- */

/// Перевод записи журнала в событие мониторинга. None — не наше.
fn journal_event(r: &win::Record, driver: Option<&str>) -> Option<Event> {
    let field = |name: &str| r.data.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    let msg = || r.message.clone().map(|m| m.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "));
    let (kind, detail) = match (r.provider.as_str(), r.id) {
        ("Microsoft-Windows-NDIS", 10400) => ("adapter_reset", json!({ "name": field("AdapterName"), "text": msg() })),
        ("Microsoft-Windows-Kernel-Power", 42) => ("sleep", json!({})),
        ("Microsoft-Windows-Kernel-Power", 107) => ("resume", json!({})),
        ("Tcpip", 4231 | 4266) => ("ports", json!({ "text": msg() })),
        ("Microsoft-Windows-WLAN-AutoConfig", 8003) => ("wifi_disc", json!({ "ssid": field("SSID"), "reason": field("Reason").or_else(msg) })),
        ("Microsoft-Windows-WLAN-AutoConfig", 8001) => ("wifi_conn", json!({ "ssid": field("SSID") })),
        // Драйверы пишут потерю линка своими кодами, но словами похоже: по
        // тексту она узнаётся у Intel, Realtek и прочих, и у потерь линка
        // появляется история за месяц — ещё до включения мониторинга.
        (p, _) if Some(p) == driver && link_text(&msg().unwrap_or_default()) == Some(false) => ("link_down", json!({ "text": msg(), "from": "journal" })),
        (p, _) if Some(p) == driver && link_text(&msg().unwrap_or_default()) == Some(true) => ("link_up", json!({ "text": msg(), "from": "journal" })),
        (p, id) if Some(p) == driver && r.level <= 3 => ("driver_warn", json!({ "provider": p, "id": id, "text": msg() })),
        _ => return None,
    };
    Some(Event::new(r.ts, kind, detail))
}

/// Сообщение драйвера о линке: Some(false) — пропал, Some(true) — появился.
fn link_text(text: &str) -> Option<bool> {
    let t = text.to_lowercase();
    if ["link is disconnected", "link is down", "link down", "disconnected", "связь потеряна", "отключ"].iter().any(|k| t.contains(k)) {
        Some(false)
    } else if ["link has been established", "link is up", "link up", "connected at", "установлен"].iter().any(|k| t.contains(k)) {
        Some(true)
    } else {
        None
    }
}

/// XPath-запрос: новые с прошлой записи, а в первый раз — за месяц.
fn xpath(filter: &str, after: Option<u64>) -> String {
    match after {
        Some(id) => format!("*[System[({filter}) and EventRecordID > {id}]]"),
        None => format!("*[System[({filter}) and TimeCreated[timediff(@SystemTime) <= {}]]]", HISTORY * 1000),
    }
}

fn poll_journal(store: &mut Store, app: &Host, driver: Option<&str>) {
    let mut sys = "(Provider[@Name='Microsoft-Windows-NDIS'] and EventID=10400) or (Provider[@Name='Microsoft-Windows-Kernel-Power'] and (EventID=42 or EventID=107)) or (Provider[@Name='Tcpip'] and (EventID=4231 or EventID=4266))".to_owned();
    // Имя службы драйвера — из реестра; в журнале драйвер подписывается им.
    if let Some(d) = driver.filter(|d| d.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')) {
        sys += &format!(" or (Provider[@Name='{d}'] and (Level=1 or Level=2 or Level=3))");
    }
    let channels = [("System", "evt_system", sys), ("Microsoft-Windows-WLAN-AutoConfig/Operational", "evt_wlan", "EventID=8001 or EventID=8003".to_owned())];
    for (channel, key, filter) in channels {
        let after = store.meta(key).and_then(|v| v.parse().ok());
        let records = win::events(channel, &xpath(&filter, after), &|r| r.provider != "Microsoft-Windows-Kernel-Power", 2000);
        let Some(last) = records.last().map(|r| r.record_id) else { continue };
        for r in &records {
            if let Some(e) = journal_event(r, driver) {
                let _ = store.add_event(&e);
                if after.is_some() {
                    let _ = app.emit("plasto:stab:event", e.to_json());
                }
            }
        }
        let _ = store.set_meta(key, &last.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn имя_отчёта_из_локального_времени() {
        assert_eq!(report_name("29.10.2026 05:30", 7), "plasto-stability-2026-10-29_05-30.html");
        assert_eq!(report_name("—", 7), "plasto-stability-7.html");
    }

    #[test]
    fn фильтры_времени() {
        assert_eq!(range("5m"), (300, 300));
        assert_eq!(range("1y").0, 365 * 86400);
        assert_eq!(range("что-то"), range("1d"));
    }

    #[test]
    fn домашние_адреса_не_считаются_провайдером() {
        assert!(home("192.168.0.1".parse().unwrap()));
        assert!(home("172.20.1.1".parse().unwrap()));
        assert!(!home("10.0.0.1".parse().unwrap()), "10/8 у провайдеров — обычное дело");
        assert!(!home("100.64.0.1".parse().unwrap()));
    }

    #[test]
    fn записи_журнала_становятся_событиями() {
        let rec = |provider: &str, id: u32, level: u8| win::Record { provider: provider.into(), id, level, ts: 5, ..Default::default() };
        assert_eq!(journal_event(&rec("Microsoft-Windows-NDIS", 10400, 3), None).unwrap().kind, "adapter_reset");
        assert_eq!(journal_event(&rec("Microsoft-Windows-Kernel-Power", 42, 4), None).unwrap().kind, "sleep");
        let mut w = rec("Microsoft-Windows-WLAN-AutoConfig", 8003, 4);
        w.data = vec![("SSID".into(), "home".into()), ("Reason".into(), "".into())];
        w.message = Some("Отключено.\n\nПричина: тест".into());
        let e = journal_event(&w, None).unwrap();
        assert_eq!((e.kind.as_str(), e.detail["ssid"].as_str(), e.detail["reason"].as_str()), ("wifi_disc", Some("home"), Some("Отключено. Причина: тест")));
        // Предупреждения драйвера — только от драйвера своего адаптера.
        assert_eq!(journal_event(&rec("e1dexpress", 99, 3), Some("e1dexpress")).unwrap().kind, "driver_warn");
        assert!(journal_event(&rec("e1dexpress", 98, 4), Some("e1dexpress")).is_none());
        // А потерю и появление линка он пишет текстом.
        let mut down = rec("e1dexpress", 27, 3);
        down.message = Some("Intel(R) Ethernet Connection (17) I219-V
 Network link is disconnected.".into());
        assert_eq!(journal_event(&down, Some("e1dexpress")).unwrap().kind, "link_down");
        let mut up = rec("e1dexpress", 32, 4);
        up.message = Some("Network link has been established at 1Gbps full duplex.".into());
        assert_eq!(journal_event(&up, Some("e1dexpress")).unwrap().kind, "link_up");
        assert!(journal_event(&rec("Netwtw14", 7021, 3), Some("e1dexpress")).is_none());
    }

    #[test]
    fn запрос_журнала_с_отметкой_и_без() {
        assert_eq!(xpath("EventID=1", Some(7)), "*[System[(EventID=1) and EventRecordID > 7]]");
        assert!(xpath("EventID=1", None).contains("timediff(@SystemTime) <= 2592000000"));
    }

    /* Живой сборщик во временной папке: cargo test --lib -- --ignored --nocapture живой_сборщик */
    #[test]
    #[ignore = "живая сеть, ~80 с"]
    fn живой_сборщик() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("APPDATA", dir.path());
        let (app, _rx) = Host::new();
        start(&app);
        std::thread::sleep(Duration::from_secs(75));
        println!("live: {:#}", status(&app)["live"]);
        stop();
        while running() {
            std::thread::sleep(Duration::from_millis(100));
        }
        for r in ["5m", "1h", "1d", "1y"] {
            let q = query(&app, &json!({ "range": r }));
            let filled = q["buckets"].as_array().map_or(0, |b| b.iter().filter(|x| x["secs"].as_u64() > Some(0)).count());
            println!("{r}: столбцов с данными {filled}, секунд {}, вердикт {}", q["summary"]["secs"], q["summary"]["head"]);
        }
        let q = query(&app, &json!({ "range": "1mo" }));
        println!("сводка за месяц: {:#}", q["summary"]);
        let mut kinds = std::collections::BTreeMap::new();
        for e in q["events"].as_array().unwrap() {
            *kinds.entry(e["kind"].as_str().unwrap().to_owned()).or_insert(0) += 1;
        }
        println!("события: {kinds:?}");
        println!("последняя секунда: {:#}", query(&app, &json!({ "range": "5m" }))["buckets"].as_array().unwrap().iter().rev().find(|b| b["secs"].as_u64() > Some(0)).unwrap());
    }

    #[test]
    fn свёртка_досчитывает_минуты_и_часы_после_перерыва() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = Store::open(&dir.path().join("s.db")).unwrap();
        let base = 1_000_000 * 3600;
        let samples: Vec<Sample> = (0..180).map(|i| Sample { ts: base + i, rtt: [Rtt::Ms(1.0), Rtt::Skip, Rtt::Ms(10.0), Rtt::Ms(12.0)], up: Some(true), ..Default::default() }).collect();
        st.insert_raw(&samples).unwrap();
        // PLASTO закрыли, открыли через два часа.
        fill(&mut st, base + 2 * 3600 + 5).unwrap();
        let minutes = st.aggs(MINUTE, base, base + 3600).unwrap();
        assert_eq!(minutes.iter().map(|a| a.secs).collect::<Vec<_>>(), vec![60, 60, 60]);
        let hours = st.aggs(HOUR, base, base + 7200).unwrap();
        assert_eq!((hours.len(), hours[0].secs), (1, 180));
        // Повторный вызов ничего не задваивает.
        fill(&mut st, base + 2 * 3600 + 10).unwrap();
        assert_eq!(st.aggs(HOUR, base, base + 7200).unwrap()[0].secs, 180);
        // Запрос «неделя» видит эти три минуты.
        let loaded = load(&st, base - 86400, base + 2 * 3600 + 10).unwrap();
        assert_eq!(loaded.iter().map(|a| a.secs).sum::<u32>(), 180);
    }
}

#[cfg(test)]
mod report_preview {
    use super::*;

    /* Отчёт по настоящей базе (только чтение) в файл из PLASTO_REPORT_OUT. */
    #[test]
    #[ignore = "читает настоящую базу"]
    fn отчёт_по_настоящим_данным() {
        let (app, _rx) = Host::new();
        let q = query(&app, &json!({ "range": "1h" }));
        let html = report::render(&q, &|ts| crate::platform::local_stamp((ts.max(0) as u64) * 1000));
        std::fs::write(std::env::var("PLASTO_REPORT_OUT").unwrap(), html).unwrap();
    }
}
