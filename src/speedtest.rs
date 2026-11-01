//! Замер скорости интернета до серверов в России и за рубежом.
//!
//! Итог — два лучших результата: внутри РФ и за её пределами. Разница между
//! ними сама по себе диагноз: зарубежные хостеры и Cloudflare в РФ
//! замедляют, и обычный «спидтест» до ближайшего сервера этого не покажет.
//!
//! Как меряем:
//! 1. Одно TCP-соединение до каждого сервера параллельно — кто вообще
//!    отвечает и кто ближе. У 2ip серверов две сотни, из них берём по два
//!    ближайших в каждом регионе; остальные провайдеры идут все.
//! 2. По каждому кандидату: пинг и джиттер (время TCP-рукопожатия), затем
//!    загрузка в несколько потоков. Один поток канал не заполняет (на
//!    600 Мбит/с — около 90), первые секунды разгона TCP в счёт не идут.
//!    Пока идёт загрузка, рядом меряется пинг — задержка под нагрузкой
//!    (bufferbloat).
//! 3. Отдача — на тех, кто её принимает: Selectel, Cloudflare и лучший по
//!    загрузке сервер 2ip в каждом регионе.
//!
//! speedtest.net из РФ не отвечает, поэтому Ookla здесь нет.

mod http;
mod twoip;

use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::host::Host;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    Ru,
    Abroad,
}

impl Region {
    fn as_str(self) -> &'static str {
        match self {
            Region::Ru => "ru",
            Region::Abroad => "abroad",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Proto {
    /// Любой большой файл: только загрузка.
    File(&'static str),
    /// LibreSpeed: `garbage.php` и `empty.php` от базового адреса.
    LibreSpeed(&'static str),
    /// `speed.cloudflare.com`: `__down` и `__up`.
    Cloudflare,
    /// Сервер 2ip, WebSocket.
    TwoIp,
}

#[derive(Clone, Debug)]
pub struct Server {
    pub provider: &'static str,
    pub city: String,
    pub country: String,
    pub region: Region,
    pub host: String,
    pub port: u16,
    pub proto: Proto,
}

impl Server {
    pub fn label(&self) -> String {
        if self.city.is_empty() {
            self.provider.to_owned()
        } else {
            format!("{} · {}", self.provider, self.city)
        }
    }

    fn uploads(&self) -> bool {
        !matches!(self.proto, Proto::File(_))
    }

    /// Cloudflare на шестнадцать потоков по 25 МБ отвечает 429.
    fn down_streams(&self) -> usize {
        if self.proto == Proto::Cloudflare {
            6
        } else {
            DOWN_STREAMS
        }
    }

    fn pull(&self, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
        match self.proto {
            Proto::File(url) => http::pull(url, bytes, stop),
            Proto::LibreSpeed(base) => http::pull(&format!("{base}/garbage.php?ckSize=100"), bytes, stop),
            // Больше 25 МБ за запрос Cloudflare не отдаёт: 100 МБ — уже 403.
            Proto::Cloudflare => http::pull("https://speed.cloudflare.com/__down?bytes=25000000", bytes, stop),
            Proto::TwoIp => twoip::pull(self, bytes, stop),
        }
    }

    fn push(&self, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
        match self.proto {
            Proto::File(_) => Err("сервер не принимает отдачу".into()),
            Proto::LibreSpeed(base) => http::push(&format!("{base}/empty.php"), bytes, stop),
            Proto::Cloudflare => http::push("https://speed.cloudflare.com/__up", bytes, stop),
            Proto::TwoIp => twoip::push(self, bytes, stop),
        }
    }
}

fn fixed(provider: &'static str, city: &str, country: &str, region: Region, host: &str, proto: Proto) -> Server {
    Server { provider, city: city.into(), country: country.into(), region, host: host.into(), port: 443, proto }
}

/// Все серверы: свои провайдеры и снимок списка 2ip.
pub fn servers() -> Vec<Server> {
    use Region::*;
    let mut all = vec![
        fixed("Яндекс", "", "RU", Ru, "mirror.yandex.ru", Proto::File("https://mirror.yandex.ru/ubuntu/ls-lR.gz")),
        fixed("Selectel", "", "RU", Ru, "speedtest.selectel.ru", Proto::LibreSpeed("https://speedtest.selectel.ru")),
        fixed("Vultr", "Франкфурт", "DE", Abroad, "fra-de-ping.vultr.com", Proto::File("https://fra-de-ping.vultr.com/vultr.com.100MB.bin")),
        fixed("OVH", "Рубе", "FR", Abroad, "proof.ovh.net", Proto::File("https://proof.ovh.net/files/100Mb.dat")),
        fixed("Hetzner", "Нюрнберг", "DE", Abroad, "nbg1-speed.hetzner.com", Proto::File("https://nbg1-speed.hetzner.com/100MB.bin")),
        fixed("Linode", "Франкфурт", "DE", Abroad, "speedtest.frankfurt.linode.com", Proto::File("https://speedtest.frankfurt.linode.com/100MB-frankfurt.bin")),
        fixed("Cloudflare", "", "", Abroad, "speed.cloudflare.com", Proto::Cloudflare),
    ];
    all.extend(twoip::servers());
    all
}

/* ---------- параметры --------------------------------------------------- */

/// Серверов 2ip на регион: ближайший по пингу не всегда самый быстрый.
const TWOIP_PER_REGION: usize = 2;
const REACH_THREADS: usize = 48;
/// С запасом на один повтор SYN: OVH иногда отвечает только со второго.
const REACH_TIMEOUT: Duration = Duration::from_millis(2500);
const PINGS: usize = 8;
const DOWN_STREAMS: usize = 16;
const UP_STREAMS: usize = 8;
const DOWN_TIME: Duration = Duration::from_secs(5);
const UP_TIME: Duration = Duration::from_secs(5);
/// Сколько отбросить в начале: у отдачи больше, буфер сокета сперва
/// заполняется быстрее, чем уходит в сеть.
const DOWN_GRACE: Duration = Duration::from_millis(1500);
const UP_GRACE: Duration = Duration::from_secs(2);
/// Соединение есть, а данных нет — так выглядит замедление ТСПУ. Ждать
/// весь замер незачем.
const STALL: Duration = Duration::from_secs(3);
/// Пинг под нагрузкой: как часто.
const LOADED_EVERY: Duration = Duration::from_millis(300);
/// Доля полосы прогресса на первичный обход серверов.
const REACH_SHARE: f64 = 0.06;

/* ---------- состояние --------------------------------------------------- */

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

pub fn cancel() -> bool {
    if running() {
        CANCEL.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::Acquire)
}

/* ---------- арифметика -------------------------------------------------- */

/// Мбит/с, десятичные мегабиты — как у Яндекса, Selectel и 2ip.
pub fn mbps(bytes: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / secs / 1e6
}

pub fn median(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    Some(if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 })
}

/// Пинг — медиана, джиттер — средний скачок между соседними замерами.
pub fn latency(samples: &[f64]) -> Option<(f64, f64)> {
    let median = median(samples)?;
    let n = samples.len();
    let jitter = if n < 2 { 0.0 } else { samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (n - 1) as f64 };
    Some((median, jitter))
}

/// Кого мерить: все свои провайдеры, что ответили, и по `per_region`
/// ближайших серверов 2ip в каждом регионе. За рубежом 2ip берутся из разных
/// стран: два сервера в Минске сказали бы о мире меньше, чем Минск и
/// Хельсинки. Сначала РФ, внутри — по пингу.
pub fn pick(servers: &[Server], rtt: &[Option<f64>], per_region: usize) -> Vec<usize> {
    let mut picked = Vec::new();
    for region in [Region::Ru, Region::Abroad] {
        let mut ours: Vec<usize> = (0..servers.len()).filter(|&i| servers[i].region == region && rtt[i].is_some()).collect();
        ours.sort_by(|&a, &b| rtt[a].unwrap().total_cmp(&rtt[b].unwrap()));
        let mut countries: Vec<&str> = Vec::new();
        for i in ours {
            let s = &servers[i];
            if s.proto != Proto::TwoIp {
                picked.push(i);
            } else if countries.len() < per_region && (region == Region::Ru || !countries.contains(&s.country.as_str())) {
                picked.push(i);
                countries.push(&s.country);
            }
        }
    }
    picked
}

/// Кому давать отдачу: всем, кто её принимает, кроме 2ip — у того только
/// лучший по загрузке в регионе (`down` — по индексам `cands`).
pub fn uploaders(servers: &[Server], cands: &[usize], down: &[Option<f64>]) -> Vec<usize> {
    let mut out = Vec::new();
    for region in [Region::Ru, Region::Abroad] {
        let mut best: Option<(usize, f64)> = None;
        for (k, &i) in cands.iter().enumerate() {
            let s = &servers[i];
            if s.region != region || !s.uploads() {
                continue;
            }
            if s.proto != Proto::TwoIp {
                out.push(i);
            } else if let Some(v) = down[k] {
                if best.is_none_or(|(_, b)| v > b) {
                    best = Some((i, v));
                }
            }
        }
        out.extend(best.map(|(i, _)| i));
    }
    out
}

/// Сколько отдач будет, ещё до загрузки: по одной на каждого не-2ip и одна
/// на регион, где есть кандидаты 2ip. Нужно только для полосы прогресса.
fn expected_uploads(servers: &[Server], cands: &[usize]) -> usize {
    let plain = cands.iter().filter(|&&i| servers[i].uploads() && servers[i].proto != Proto::TwoIp).count();
    let regions = [Region::Ru, Region::Abroad].iter().filter(|&&r| cands.iter().any(|&i| servers[i].region == r && servers[i].proto == Proto::TwoIp)).count();
    plain + regions
}

/* ---------- сеть -------------------------------------------------------- */

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host, port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or_else(|| "нет адреса".into())
}

/// Сетевые ошибки Windows длинные и одинаковые; в интерфейс — суть.
pub fn brief(error: &str) -> String {
    let e = error.to_lowercase();
    let timeout = e.contains("10060") || e.contains("timed out") || e.contains("timeout");
    if timeout && e.contains("tls") {
        "TLS-рукопожатие не проходит — похоже на блокировку".into()
    } else if timeout {
        "сервер не отвечает".into()
    } else if e.contains("10054") || e.contains("reset") {
        "соединение сбрасывается".into()
    } else if e.contains("10061") || e.contains("refused") {
        "сервер отклоняет соединение".into()
    } else if e.contains("429") {
        "сервер ограничил частоту запросов (429) — повторите позже".into()
    } else if e.contains("dns") || e.contains("11001") || e.contains("resolve") {
        "имя сервера не находится".into()
    } else {
        // `https://адрес: причина` — адрес и так виден в строке сервера.
        let tail = error.split_once(": ").filter(|(head, _)| head.contains("://")).map_or(error, |(_, t)| t);
        tail.chars().take(120).collect()
    }
}

fn connect_ms(addr: &SocketAddr, timeout: Duration) -> Result<f64, String> {
    let start = Instant::now();
    TcpStream::connect_timeout(addr, timeout).map_err(|e| e.to_string())?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

/// Одно соединение до каждого сервера, параллельно.
fn reach(app: &Host, servers: &[Server]) -> Vec<Option<f64>> {
    let out = Mutex::new(vec![None; servers.len()]);
    let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..REACH_THREADS.min(servers.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= servers.len() || cancelled() {
                    break;
                }
                let s = &servers[i];
                let ms = resolve(&s.host, s.port).and_then(|a| connect_ms(&a, REACH_TIMEOUT)).ok();
                out.lock().unwrap()[i] = ms;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 16 == 0 || n == servers.len() {
                    progress(app, "reach", "", n as f64 / servers.len() as f64 * REACH_SHARE, None);
                }
            });
        }
    });
    out.into_inner().unwrap()
}

fn progress(app: &Host, phase: &str, server: &str, frac: f64, mbps: Option<f64>) {
    let _ = app.emit("plasto:speed:progress", json!({ "phase": phase, "server": server, "frac": frac.clamp(0.0, 1.0), "mbps": mbps }));
}

/// Где в полосе прогресса шаг `step` из `steps`.
struct Slot<'a> {
    app: &'a Host,
    phase: &'static str,
    label: String,
    step: usize,
    steps: usize,
}

impl Slot<'_> {
    fn emit(&self, part: f64, mbps: Option<f64>) {
        let each = (1.0 - REACH_SHARE) / self.steps.max(1) as f64;
        progress(self.app, self.phase, &self.label, REACH_SHARE + each * (self.step as f64 + part.clamp(0.0, 1.0)), mbps);
    }
}

struct Measured {
    mbps: f64,
    loaded: Vec<f64>,
    /// Живая скорость по ходу замера, для графика: разгон и плато видны глазом.
    series: Vec<f64>,
}

/// Ряды для графиков: десятых хватает, а события по ходу замера не раздуваются.
fn tenths(values: &[f64]) -> Vec<f64> {
    values.iter().map(|v| (v * 10.0).round() / 10.0).collect()
}

/// Запускает `streams` потоков на `time` и считает скорость без первых
/// `grace`. Если задан `ping_to`, рядом меряет пинг под нагрузкой.
fn measure(
    slot: &Slot,
    streams: usize,
    time: Duration,
    grace: Duration,
    ping_to: Option<SocketAddr>,
    work: impl Fn(&AtomicU64, &AtomicBool) -> Result<(), String> + Sync,
) -> Result<Measured, String> {
    let bytes = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let failure = Mutex::new(None::<String>);
    let failed = AtomicUsize::new(0);
    let loaded = Mutex::new(Vec::new());
    let start = Instant::now();
    let mut at_grace = None;
    let mut stalled = false;
    let mut series = Vec::new();

    std::thread::scope(|scope| {
        for _ in 0..streams {
            scope.spawn(|| {
                if let Err(e) = work(&bytes, &stop) {
                    failed.fetch_add(1, Ordering::Relaxed);
                    failure.lock().unwrap().get_or_insert(e);
                }
            });
        }
        if let Some(addr) = ping_to {
            let (stop, loaded) = (&stop, &loaded);
            scope.spawn(move || {
                // Пока потоки разгоняются, очередь в буферах ещё не набралась.
                std::thread::sleep(grace);
                while !stop.load(Ordering::Acquire) {
                    if let Ok(ms) = connect_ms(&addr, Duration::from_secs(2)) {
                        loaded.lock().unwrap().push(ms);
                    }
                    std::thread::sleep(LOADED_EVERY);
                }
            });
        }
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let elapsed = start.elapsed();
            let got = bytes.load(Ordering::Relaxed);
            if at_grace.is_none() && elapsed >= grace {
                at_grace = Some((got, elapsed));
            }
            let live = match at_grace {
                Some((b, t)) if elapsed > t => mbps(got - b, (elapsed - t).as_secs_f64()),
                _ => mbps(got, elapsed.as_secs_f64()),
            };
            slot.emit(elapsed.as_secs_f64() / time.as_secs_f64(), Some(live));
            series.push(live);
            if got == 0 && elapsed >= STALL {
                stalled = true;
                break;
            }
            if elapsed >= time || cancelled() || failed.load(Ordering::Relaxed) == streams {
                break;
            }
        }
        stop.store(true, Ordering::Release);
    });

    let (elapsed, got) = (start.elapsed(), bytes.load(Ordering::Relaxed));
    if got == 0 {
        let error = failure.into_inner().unwrap();
        return Err(match (stalled, error) {
            (_, Some(e)) => brief(&e),
            (true, None) => "соединение есть, а данные не идут — похоже на замедление".into(),
            (false, None) => "ничего не получено".into(),
        });
    }
    let speed = match at_grace {
        Some((b, t)) if elapsed > t && got > b => mbps(got - b, (elapsed - t).as_secs_f64()),
        _ => mbps(got, elapsed.as_secs_f64()),
    };
    Ok(Measured { mbps: speed, loaded: loaded.into_inner().unwrap(), series: tenths(&series) })
}

/* ---------- замер ------------------------------------------------------- */

/// Строка результата по серверу; заполняется по ходу замера.
#[derive(Clone, Default)]
struct Row {
    ping: Option<f64>,
    jitter: Option<f64>,
    loaded: Option<f64>,
    down: Option<f64>,
    up: Option<f64>,
    down_error: Option<String>,
    up_error: Option<String>,
    /// Ряды для графиков: пинги в покое, пинги под нагрузкой, скорость по ходу.
    pings: Vec<f64>,
    loaded_series: Vec<f64>,
    down_series: Vec<f64>,
    up_series: Vec<f64>,
}

fn row_json(s: &Server, r: &Row) -> Value {
    json!({
        "label": s.label(), "provider": s.provider, "city": s.city, "country": s.country, "region": s.region.as_str(),
        "uploads": s.uploads(),
        "ping": r.ping, "jitter": r.jitter, "loaded": r.loaded, "down": r.down, "up": r.up,
        "downError": r.down_error, "upError": r.up_error,
        "pingSeries": r.pings, "loadedSeries": r.loaded_series, "downSeries": r.down_series, "upSeries": r.up_series,
    })
}

/// Итог по региону: лучший по загрузке сервер (с его пингом) и лучшая отдача.
fn summary(servers: &[Server], cands: &[usize], rows: &[Row], region: Region) -> Value {
    let ours = || cands.iter().zip(rows).filter(|(&i, _)| servers[i].region == region);
    let best_down = ours().filter(|(_, r)| r.down.is_some()).max_by(|a, b| a.1.down.unwrap().total_cmp(&b.1.down.unwrap()));
    let best_up = ours().filter(|(_, r)| r.up.is_some()).max_by(|a, b| a.1.up.unwrap().total_cmp(&b.1.up.unwrap()));
    match best_down {
        None => Value::Null,
        Some((&i, r)) => json!({
            "server": servers[i].label(), "down": r.down, "ping": r.ping, "jitter": r.jitter, "loaded": r.loaded,
            "up": best_up.and_then(|(_, r)| r.up), "upServer": best_up.map(|(&i, _)| servers[i].label()),
            // Графики итога: ряды того же сервера, чьи цифры показаны.
            "downSeries": r.down_series, "pingSeries": r.pings, "loadedSeries": r.loaded_series,
            "upSeries": best_up.map(|(_, r)| r.up_series.clone()).unwrap_or_default(),
        }),
    }
}

fn emit_rows(app: &Host, servers: &[Server], cands: &[usize], rows: &[Row]) {
    let list: Vec<Value> = cands.iter().zip(rows).map(|(&i, r)| row_json(&servers[i], r)).collect();
    let _ = app.emit("plasto:speed:rows", list);
}

pub fn run(app: &Host) -> Value {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return json!({ "error": "замер уже идёт" });
    }
    let _guard = Guard;
    CANCEL.store(false, Ordering::Release);
    let began = Instant::now();

    let servers = servers();
    progress(app, "reach", "", 0.0, None);
    let rtt = reach(app, &servers);
    let cands = pick(&servers, &rtt, TWOIP_PER_REGION);
    if cands.is_empty() {
        return json!({ "error": if cancelled() { "замер прерван" } else { "ни один сервер не ответил — проверьте подключение" } });
    }
    let silent: Vec<String> = servers.iter().zip(&rtt).filter(|(s, r)| r.is_none() && s.proto != Proto::TwoIp).map(|(s, _)| s.label()).collect();
    let twoip_total = servers.iter().filter(|s| s.proto == Proto::TwoIp).count();
    let twoip_alive = servers.iter().zip(&rtt).filter(|(s, r)| r.is_some() && s.proto == Proto::TwoIp).count();

    let mut rows = vec![Row::default(); cands.len()];
    let mut addrs = vec![None; cands.len()];
    emit_rows(app, &servers, &cands, &rows);
    let steps = cands.len() + expected_uploads(&servers, &cands);

    for (k, &i) in cands.iter().enumerate() {
        if cancelled() {
            break;
        }
        let s = &servers[i];
        let slot = Slot { app, phase: "down", label: s.label(), step: k, steps };
        slot.emit(0.0, None);
        let addr = resolve(&s.host, s.port).ok();
        addrs[k] = addr;
        if let Some(addr) = addr {
            let samples: Vec<f64> = (0..PINGS)
                .filter_map(|_| {
                    let ms = connect_ms(&addr, Duration::from_secs(2)).ok();
                    std::thread::sleep(Duration::from_millis(40));
                    ms
                })
                .collect();
            if let Some((p, j)) = latency(&samples) {
                rows[k].ping = Some(p);
                rows[k].jitter = Some(j);
            }
            rows[k].pings = tenths(&samples);
        }
        match measure(&slot, s.down_streams(), DOWN_TIME, DOWN_GRACE, addr, |b, st| s.pull(b, st)) {
            Ok(m) => {
                rows[k].down = Some(m.mbps);
                rows[k].loaded = median(&m.loaded);
                rows[k].loaded_series = tenths(&m.loaded);
                rows[k].down_series = m.series;
            }
            Err(e) => rows[k].down_error = Some(e),
        }
        emit_rows(app, &servers, &cands, &rows);
    }

    let down: Vec<Option<f64>> = rows.iter().map(|r| r.down).collect();
    for (n, i) in uploaders(&servers, &cands, &down).into_iter().enumerate() {
        if cancelled() {
            break;
        }
        let k = cands.iter().position(|&c| c == i).unwrap();
        let s = &servers[i];
        let slot = Slot { app, phase: "up", label: s.label(), step: cands.len() + n, steps };
        match measure(&slot, UP_STREAMS, UP_TIME, UP_GRACE, None, |b, st| s.push(b, st)) {
            Ok(m) => {
                rows[k].up = Some(m.mbps);
                rows[k].up_series = m.series;
            }
            Err(e) => rows[k].up_error = Some(e),
        }
        emit_rows(app, &servers, &cands, &rows);
    }

    json!({
        "ru": summary(&servers, &cands, &rows, Region::Ru),
        "abroad": summary(&servers, &cands, &rows, Region::Abroad),
        "servers": cands.iter().zip(&rows).map(|(&i, r)| row_json(&servers[i], r)).collect::<Vec<_>>(),
        "silent": silent,
        "twoip": { "alive": twoip_alive, "total": twoip_total },
        "cancelled": cancelled(),
        "durationMs": began.elapsed().as_millis() as u64,
        "finishedAt": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(provider: &'static str, region: Region, proto: Proto) -> Server {
        Server { provider, city: String::new(), country: String::new(), region, host: "h".into(), port: 443, proto }
    }

    #[test]
    fn мбиты_десятичные() {
        assert_eq!(mbps(125_000_000, 10.0), 100.0);
        assert_eq!(mbps(1, 0.0), 0.0);
    }

    #[test]
    fn пинг_медиана_джиттер_средний_скачок() {
        assert_eq!(latency(&[]), None);
        assert_eq!(latency(&[15.0]), Some((15.0, 0.0)));
        // Медиана по отсортированным, джиттер — по порядку замеров: |14-10|, |12-14|.
        assert_eq!(latency(&[10.0, 14.0, 12.0]), Some((12.0, 3.0)));
        assert_eq!(latency(&[10.0, 20.0]), Some((15.0, 10.0)));
    }

    #[test]
    fn в_списке_есть_оба_региона_и_отдача_в_каждом() {
        let all = servers();
        for region in [Region::Ru, Region::Abroad] {
            assert!(all.iter().any(|s| s.region == region && s.proto != Proto::TwoIp));
            assert!(all.iter().any(|s| s.region == region && s.uploads()));
        }
    }

    #[test]
    fn выбор_все_свои_и_по_два_ближайших_2ip_на_регион() {
        use Region::*;
        let s = vec![
            srv("Яндекс", Ru, Proto::File("u")),
            srv("2ip", Ru, Proto::TwoIp),
            srv("2ip", Ru, Proto::TwoIp),
            srv("2ip", Ru, Proto::TwoIp),
            srv("Vultr", Abroad, Proto::File("u")),
            srv("2ip", Abroad, Proto::TwoIp),
            srv("Hetzner", Abroad, Proto::File("u")),
        ];
        let rtt = [Some(30.0), Some(20.0), Some(10.0), Some(15.0), Some(50.0), None, None];
        // РФ по пингу: 2ip(10), 2ip(15), Яндекс(30); третий 2ip(20) лишний.
        // За рубежом молчат 2ip и Hetzner — остаётся Vultr.
        assert_eq!(pick(&s, &rtt, 2), vec![2, 3, 0, 4]);
        assert!(pick(&s, &[None; 7], 2).is_empty());
    }

    #[test]
    fn за_рубежом_2ip_из_разных_стран() {
        use Region::*;
        let mut s = vec![srv("2ip", Abroad, Proto::TwoIp), srv("2ip", Abroad, Proto::TwoIp), srv("2ip", Abroad, Proto::TwoIp)];
        s[0].country = "BY".into();
        s[1].country = "BY".into();
        s[2].country = "FI".into();
        assert_eq!(pick(&s, &[Some(27.0), Some(28.0), Some(40.0)], 2), vec![0, 2]);
    }

    #[test]
    fn ошибки_коротко() {
        assert_eq!(
            brief("https://nbg1-speed.hetzner.com/100MB.bin: Connection Failed: native_tls connect failed: Попытка установить соединение была безуспешной (os error 10060)"),
            "TLS-рукопожатие не проходит — похоже на блокировку"
        );
        assert_eq!(brief("Connection reset by peer"), "соединение сбрасывается");
        assert_eq!(brief("https://x.ru/a: сервер ответил 403"), "сервер ответил 403");
        assert_eq!(brief("сервер ответил 403"), "сервер ответил 403");
        assert!(brief("https://speed.cloudflare.com/__down?bytes=1: сервер ответил 429").contains("429"));
    }

    #[test]
    fn отдача_у_своих_и_у_лучшего_по_загрузке_2ip() {
        use Region::*;
        let s = vec![
            srv("Selectel", Ru, Proto::LibreSpeed("b")),
            srv("2ip", Ru, Proto::TwoIp),
            srv("2ip", Ru, Proto::TwoIp),
            srv("Яндекс", Ru, Proto::File("u")),
            srv("2ip", Abroad, Proto::TwoIp),
            srv("Cloudflare", Abroad, Proto::Cloudflare),
        ];
        let cands = vec![0, 1, 2, 3, 4, 5];
        let down = [Some(90.0), Some(200.0), Some(300.0), Some(500.0), None, Some(20.0)];
        // У зарубежного 2ip загрузка не удалась — отдачу ему не даём.
        assert_eq!(uploaders(&s, &cands, &down), vec![0, 2, 5]);
        assert_eq!(expected_uploads(&s, &cands), 4);
    }

    #[test]
    fn итог_региона_лучшая_загрузка_и_отдельно_лучшая_отдача() {
        use Region::*;
        let s = vec![srv("Яндекс", Ru, Proto::File("u")), srv("Selectel", Ru, Proto::LibreSpeed("b")), srv("Vultr", Abroad, Proto::File("u"))];
        let cands = vec![0, 1, 2];
        let rows = vec![
            Row { down: Some(500.0), ping: Some(15.0), pings: vec![14.0, 16.0], loaded_series: vec![40.0], down_series: vec![120.0, 480.0], ..Default::default() },
            Row { down: Some(90.0), up: Some(400.0), down_series: vec![90.0], up_series: vec![200.0, 400.0], ..Default::default() },
            Row { down_error: Some("нет".into()), ..Default::default() },
        ];
        let ru = summary(&s, &cands, &rows, Ru);
        assert_eq!(ru["server"], "Яндекс");
        assert_eq!(ru["down"], 500.0);
        assert_eq!(ru["ping"], 15.0);
        assert_eq!(ru["up"], 400.0);
        assert_eq!(ru["upServer"], "Selectel");
        // Графики — от тех же серверов, что и цифры: загрузка и пинг от Яндекса, отдача от Selectel.
        assert_eq!(ru["downSeries"], json!([120.0, 480.0]));
        assert_eq!(ru["pingSeries"], json!([14.0, 16.0]));
        assert_eq!(ru["loadedSeries"], json!([40.0]));
        assert_eq!(ru["upSeries"], json!([200.0, 400.0]));
        assert!(summary(&s, &cands, &rows, Abroad).is_null());
    }

    #[test]
    fn ряды_до_десятых() {
        assert_eq!(tenths(&[12.34, 0.05, 599.99]), vec![12.3, 0.1, 600.0]);
        assert!(tenths(&[]).is_empty());
    }

    /* Живая сеть: cargo test --lib -- --ignored --nocapture живой_замер */
    #[test]
    #[ignore = "живая сеть"]
    fn живой_замер() {
        let (host, _rx) = Host::new();
        println!("{:#}", run(&host));
    }
}

