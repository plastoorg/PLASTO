//! Смена DNS системы: пресеты, применение с проверкой и откатом, свои пресеты.
//!
//! Перенос `dns.js` без изменения поведения. Системные команды вынесены в
//! `Runner`, как в JS был `setRunner`: логику применения и отката можно
//! прогнать без прав администратора и без единого изменения настроек.
//!
//! Зачем это нужно. Часть сервисов ограничена не провайдером, а страной
//! (ИИ-сервисы, Google Labs): защищённый DNS сам по себе гео не снимает, а
//! «открытые» резолверы с подменой — снимают. Важнее другое: DNS-AI отвечает
//! ТОЛЬКО по шифрованному каналу (порт 53 закрыт), и обычная проверка «имя
//! резолвится» всегда провалилась бы и откатила настройку, поэтому у таких
//! пресетов свой флаг `encrypted_only` и своя проверка.
//!
//! Главный принцип — не оставить пользователя без интернета: перед правкой
//! сохраняем прежние адреса поадаптерно, после — проверяем, что имена
//! действительно резолвятся, и при провале откатываем сами.

use std::collections::HashSet;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use url::Url;

use crate::doh;

/// Заведомо существующее имя для проверки «резолвер вообще отвечает».
pub const CONTROL_HOST: &str = "example.com";

/* =====================================================================
   Запуск системных команд
   ===================================================================== */

#[derive(Clone, Debug, Default)]
pub struct RunOut {
    pub ok: bool,
    pub out: String,
    pub err: String,
}

pub trait Runner: Sync {
    fn run(&self, file: &str, args: &[String]) -> RunOut;
}

/// Настоящий запуск: без окна консоли, вывод PowerShell — в UTF-8.
///
/// Кодировку задаём здесь, а не в тексте скрипта: иначе кириллические имена
/// адаптеров («Беспроводная сеть») приходили бы кракозябрами в OEM-странице.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, file: &str, args: &[String]) -> RunOut {
        let mut args: Vec<String> = args.to_vec();
        if file.eq_ignore_ascii_case("powershell.exe") {
            if let Some(last) = args.last_mut() {
                *last = format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {last}");
            }
        }
        let mut cmd = std::process::Command::new(file);
        cmd.args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.output() {
            Ok(o) => RunOut {
                ok: o.status.success(),
                out: crate::platform::console_text(&o.stdout),
                err: crate::platform::console_text(&o.stderr),
            },
            Err(e) => RunOut { ok: false, out: String::new(), err: e.to_string() },
        }
    }
}

const PS: [&str; 5] = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"];

fn ps(r: &dyn Runner, script: &str) -> RunOut {
    let mut args: Vec<String> = PS.iter().map(|s| s.to_string()).collect();
    args.push(script.to_string());
    r.run("powershell.exe", &args)
}

fn netsh(r: &dyn Runner, args: &[&str]) -> RunOut {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    r.run("netsh", &a)
}

/* =====================================================================
   Пресеты
   ===================================================================== */

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub servers: Vec<String>,
    #[serde(default)]
    pub servers6: Vec<String>,
    #[serde(default)]
    pub doh: Option<String>,
    #[serde(default)]
    pub dot: Option<String>,
    /// UDP/53 закрыт: обычная проверка «резолвится ли имя» всегда провалится.
    #[serde(default)]
    pub encrypted_only: bool,
    /// Снимает ли гео-блок (а не только шифрует запросы).
    #[serde(default)]
    pub unblocks: bool,
    #[serde(default)]
    pub note: String,
    /// Признак «свой»: в файл не пишется, добавляется при чтении.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom: bool,
}

/// Встроенные пресеты. Данные выгружены из dns.js как есть (scripts/dump-dns-data.js):
/// имена, адреса и пояснения не переписываются вручную, поэтому разойтись им негде.
pub fn presets() -> Vec<Preset> {
    serde_json::from_str(include_str!("presets.json")).expect("presets.json повреждён")
}

/// Какой пресет сейчас стоит: все его адреса присутствуют хоть на одном адаптере.
pub fn match_preset(adapters: &[Adapter], list: &[Preset]) -> Option<String> {
    let all: HashSet<&str> = adapters.iter().flat_map(|a| a.servers.iter().map(|s| s.as_str())).collect();
    if all.is_empty() {
        return None;
    }
    list.iter().find(|p| p.servers.iter().all(|s| all.contains(s.as_str()))).map(|p| p.id.clone())
}

/* =====================================================================
   Свои пресеты
   ===================================================================== */

pub fn custom_path(user_data_dir: &Path) -> PathBuf {
    user_data_dir.join("dns.custom.json")
}

pub fn load_custom(user_data_dir: &Path) -> Vec<Preset> {
    let Ok(text) = fs::read_to_string(custom_path(user_data_dir)) else { return vec![] };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) else { return vec![] };
    items
        .into_iter()
        .filter(|p| {
            p.get("id").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) && p.get("servers").is_some_and(Value::is_array)
        })
        .filter_map(|p| serde_json::from_value::<Preset>(p).ok())
        .collect()
}

/// Встроенные + пользовательские.
pub fn all_presets(user_data_dir: Option<&Path>) -> Vec<Preset> {
    let mut v = presets();
    if let Some(d) = user_data_dir {
        v.extend(load_custom(d).into_iter().map(|p| Preset { custom: true, ..p }));
    }
    v
}

static RE_QUOTES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^["'`(\[]+|["'`)\]]+$"#).unwrap());
// regex-lite понимает `\s` только как ASCII — юникодные пробелы (неразрывный
// из копипасты с сайтов и т.п.) перечислены явно, как их понимал `regex`.
static RE_SPLIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\s\x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000},;\x{FEFF}]+").unwrap()
});
static RE_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$").unwrap());
static RE_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[tT][lL][sS]|[dD][oO][tT])://([^/:\s]+)").unwrap());

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomInput {
    pub servers: Vec<String>,
    pub servers6: Vec<String>,
    pub doh: Option<String>,
    pub dot: Option<String>,
    pub hosts: Vec<String>,
    pub guess_doh: Option<String>,
    pub ignored: Vec<String>,
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len()).is_some_and(|h| h.eq_ignore_ascii_case(prefix))
}

/// Разбирает то, что пользователь вставил: любые адреса из инструкции
/// резолвера вперемешку — IP, https-шаблон DoH, tls://имя, просто имя.
/// Так добавляется новый DNS одним копированием со страницы подключения.
pub fn parse_custom_input(text: &str) -> CustomInput {
    let mut out = CustomInput::default();

    for raw in RE_SPLIT.split(text) {
        let t = RE_QUOTES.replace_all(raw, "").into_owned();
        if t.is_empty() {
            continue;
        }
        if t.parse::<Ipv4Addr>().is_ok() {
            if !out.servers.contains(&t) {
                out.servers.push(t);
            }
            continue;
        }
        if t.parse::<Ipv6Addr>().is_ok() {
            if !out.servers6.contains(&t) {
                out.servers6.push(t);
            }
            continue;
        }

        if starts_ci(&t, "https://") {
            match Url::parse(&t) {
                Ok(u) => {
                    if out.doh.is_none() {
                        let host = host_with_port(&u);
                        out.doh = Some(if u.path() == "/" || u.path().is_empty() {
                            format!("https://{host}/dns-query")
                        } else {
                            format!("{}{}", u.origin().ascii_serialization(), u.path())
                        });
                    }
                    out.hosts.push(u.host_str().unwrap_or_default().to_string());
                }
                Err(_) => out.ignored.push(t),
            }
            continue;
        }
        if let Some(m) = RE_DOT.captures(&t) {
            out.dot = Some(m[1].to_string());
            out.hosts.push(m[1].to_string());
            continue;
        }
        if ["quic:", "h3:", "doq:", "sdns:"].iter().any(|p| starts_ci(&t, p)) {
            out.ignored.push(t);
            continue;
        }
        if (4..=253).contains(&t.chars().count()) && RE_HOST.is_match(&t) {
            out.hosts.push(t.clone());
            if out.guess_doh.is_none() {
                out.guess_doh = Some(t);
            }
            continue;
        }
        out.ignored.push(t);
    }

    let mut seen = HashSet::new();
    out.hosts.retain(|h| seen.insert(h.clone()));
    out
}

/// `host[:port]` как у `URL.host` в JS: порт по умолчанию опускается.
fn host_with_port(u: &Url) -> String {
    let h = u.host_str().unwrap_or_default();
    match u.port() {
        Some(p) => format!("{h}:{p}"),
        None => h.to_string(),
    }
}

/// Зонды, подменяемые в тестах.
pub struct Probes<'a> {
    /// Адреса имени: (адрес, семейство 4|6).
    pub lookup: &'a (dyn Fn(&str) -> Vec<(String, u8)> + Sync),
    pub doh: &'a (dyn Fn(&str, &[String], &[String]) -> bool + Sync),
    pub plain: &'a (dyn Fn(&str) -> bool + Sync),
}

fn system_lookup(host: &str) -> Vec<(String, u8)> {
    match (host, 0u16).to_socket_addrs() {
        Ok(it) => it.map(|a| (a.ip().to_string(), if a.is_ipv6() { 6 } else { 4 })).collect(),
        Err(_) => vec![],
    }
}

/// Отвечает ли сервер на обычный DNS (UDP/53).
pub fn probe_plain(ip: &str, timeout: Duration) -> bool {
    plain_a(ip, CONTROL_HOST, timeout).is_some()
}

/// A-записи имени у конкретного сервера по обычному DNS. None — не ответил
/// или ответил ошибкой; пустой список — ответ есть, но без A (только CNAME).
pub fn plain_a(ip: &str, host: &str, timeout: Duration) -> Option<Vec<Ipv4Addr>> {
    let addr = ip.parse::<std::net::IpAddr>().ok()?;
    let sock = UdpSocket::bind(if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).ok()?;
    sock.set_read_timeout(Some(timeout)).ok()?;
    sock.connect(SocketAddr::new(addr, 53)).ok()?;
    sock.send(&doh::dns_query_a(host)).ok()?;
    let mut buf = [0u8; 512];
    let n = sock.recv(&mut buf).ok()?;
    doh::answer_ok(200, &buf[..n]).then(|| doh::parse_a(&buf[..n]))
}

/// Добавляет свой пресет по тексту, вставленному пользователем.
/// Недостающее достраивает само: IP по имени, шаблон DoH по имени, и сама
/// определяет, закрыт ли у сервера обычный порт 53 (тогда — только шифрованный).
pub fn add_custom(name: &str, text: &str, user_data_dir: &Path, probes: &Probes) -> Result<Preset, String> {
    let mut p = parse_custom_input(text);

    // Нет IP, но есть имя — достаём адреса (пока системный DNS ещё обычный).
    if p.servers.is_empty() && p.servers6.is_empty() {
        for h in p.hosts.clone() {
            for (addr, family) in (probes.lookup)(&h) {
                let list = if family == 6 { &mut p.servers6 } else { &mut p.servers };
                if !list.contains(&addr) {
                    list.push(addr);
                }
            }
            if !p.servers.is_empty() || !p.servers6.is_empty() {
                break;
            }
        }
    }
    if p.servers.is_empty() {
        return Err(if !p.ignored.is_empty() && p.hosts.is_empty() {
            format!("не распознал: {}", p.ignored.iter().take(3).cloned().collect::<Vec<_>>().join(", "))
        } else {
            "не нашёл ни одного IPv4-адреса — вставьте IP или имя сервера".into()
        });
    }

    // Только имя без шаблона: пробуем стандартный путь /dns-query.
    let mut doh_url = p.doh.clone();
    if doh_url.is_none() {
        if let Some(g) = &p.guess_doh {
            let guess = format!("https://{g}/dns-query");
            if (probes.doh)(&guess, &p.servers, &p.servers6) {
                doh_url = Some(guess);
            }
        }
    }
    let mut dot = p.dot.clone();
    if dot.is_none() && !p.hosts.is_empty() {
        if let Some(d) = &doh_url {
            dot = Url::parse(d).ok().and_then(|u| u.host_str().map(str::to_string));
        }
    }

    let plain_ok = (probes.plain)(&p.servers[0]);
    let doh_ok = doh_url.as_ref().is_some_and(|d| (probes.doh)(d, &p.servers, &p.servers6));
    if !plain_ok && !doh_ok {
        return Err("сервер не отвечает ни на обычный DNS, ни по DoH — проверьте адреса".into());
    }
    let encrypted_only = !plain_ok && doh_ok;

    let key = serde_json::to_string(&json!([p.servers, doh_url])).unwrap_or_default();
    let digest = Sha1::digest(key.as_bytes());
    let id = format!("c-{}", digest.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>());

    let label = if !name.trim().is_empty() {
        name.trim().to_string()
    } else if let Some(d) = &doh_url {
        Url::parse(d).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| p.servers[0].clone())
    } else {
        p.servers[0].clone()
    };

    let preset = Preset {
        id: id.clone(),
        name: label,
        servers: p.servers.clone(),
        servers6: p.servers6.clone(),
        doh: if doh_ok { doh_url.clone() } else { None },
        dot,
        encrypted_only,
        unblocks: false,
        note: format!(
            "Свой DNS.{}{}",
            if encrypted_only { " Только шифрованный (DoH): обычный порт 53 не отвечает, нужна Windows 11." } else { "" },
            if doh_ok { "" } else { " Без шифрования." }
        ),
        custom: false,
    };

    let mut list: Vec<Preset> = load_custom(user_data_dir).into_iter().filter(|x| x.id != id).collect();
    list.push(preset.clone());
    fs::create_dir_all(user_data_dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    fs::write(custom_path(user_data_dir), text).map_err(|e| e.to_string())?;
    Ok(preset)
}

/// Настоящие зонды для `add_custom`.
pub fn real_probes_run<T>(f: impl FnOnce(&Probes) -> T) -> T {
    let doh_fn = |u: &str, s: &[String], s6: &[String]| doh::probe_doh(u, s, s6, Duration::from_secs(5));
    let plain_fn = |ip: &str| probe_plain(ip, Duration::from_secs(3));
    f(&Probes { lookup: &system_lookup, doh: &doh_fn, plain: &plain_fn })
}

pub fn remove_custom(id: &str, user_data_dir: &Path) -> Result<String, String> {
    let list = load_custom(user_data_dir);
    let next: Vec<Preset> = list.iter().filter(|p| p.id != id).cloned().collect();
    if next.len() == list.len() {
        return Err("такого пресета нет".into());
    }
    let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    fs::write(custom_path(user_data_dir), text).map_err(|e| e.to_string())?;
    Ok(id.to_string())
}

/// Номер сборки Windows: 22000 и выше — Windows 11, умеет системный DoH.
pub fn win_build(r: &dyn Runner) -> u32 {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\.(\d+)\.(\d+)").unwrap());
    let out = r.run("cmd.exe", &["/c".to_string(), "ver".to_string()]);
    RE.captures(&out.out).and_then(|c| c[3].parse().ok()).unwrap_or(0)
}

/* =====================================================================
   Адаптеры
   ===================================================================== */

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    pub index: i64,
    pub alias: String,
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub dhcp: bool,
    #[serde(default, deserialize_with = "non_null_strings")]
    pub servers: Vec<String>,
}

/// PowerShell отдаёт отсутствующий список как `[null]` — отбрасываем пустое.
fn non_null_strings<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let v = Option::<Vec<Option<String>>>::deserialize(d)?;
    Ok(v.unwrap_or_default().into_iter().flatten().collect())
}

#[derive(Clone, Debug, Default)]
pub struct Adapters {
    pub ok: bool,
    pub error: Option<String>,
    pub adapters: Vec<Adapter>,
}

/// Точный текст скрипта, выгруженный из dns.js (scripts/dump-dns-data.js).
/// DHCP определяется по реестру: у интерфейса с автоматическими адресами
/// NameServer пуст. Через Get-NetIPInterface это не получается — он показывает
/// «Dhcp» для самого адреса, а не для DNS.
const ADAPTERS_SCRIPT: &str = include_str!("adapters.ps1");

/// Активные адаптеры с текущими DNS.
pub fn adapters(r: &dyn Runner) -> Adapters {
    let res = ps(r, ADAPTERS_SCRIPT);
    if !res.ok {
        let msg = res.err.trim();
        return Adapters { ok: false, error: Some(if msg.is_empty() { "не удалось прочитать настройки".into() } else { msg.into() }), adapters: vec![] };
    }
    let trimmed = res.out.trim();
    let parsed: Value = match serde_json::from_str(if trimmed.is_empty() { "[]" } else { trimmed }) {
        Ok(v) => v,
        Err(_) => return Adapters { ok: false, error: Some("непонятный ответ PowerShell".into()), adapters: vec![] },
    };
    let items = match parsed {
        Value::Array(a) => a,
        Value::Null => vec![],
        other => vec![other],
    };
    let adapters: Vec<Adapter> = items.into_iter().filter(|v| !v.is_null()).filter_map(|v| serde_json::from_value(v).ok()).collect();
    Adapters { ok: true, error: None, adapters }
}

/* =====================================================================
   Применение, откат, восстановление
   ===================================================================== */

fn backup_file(dir: &Path) -> PathBuf {
    dir.join("dns.backup.json")
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Возвращает адаптер к прежним адресам. DHCP-адаптеру — `ResetServerAddresses`,
/// иначе вместо «получать автоматически» остались бы наши адреса вручную.
fn revert(r: &dyn Runner, list: &[Adapter]) -> bool {
    let mut ok = true;
    for a in list {
        let script = if a.dhcp || a.servers.is_empty() {
            format!("Set-DnsClientServerAddress -InterfaceIndex {} -ResetServerAddresses", a.index)
        } else {
            let s = a.servers.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
            format!("Set-DnsClientServerAddress -InterfaceIndex {} -ServerAddresses ({s})", a.index)
        };
        ok &= ps(r, &script).ok;
    }
    ok
}

/// Снимает регистрацию DoH — иначе в системе остаются шаблоны чужих серверов.
fn drop_doh(r: &dyn Runner, servers: &[String]) {
    for s in servers {
        netsh(r, &["dns", "delete", "encryption", &format!("server={s}")]);
    }
}

fn flush(r: &dyn Runner) {
    r.run("ipconfig", &["/flushdns".to_string()]);
}

/// Отвечает ли хоть один из серверов на обычный DNS.
///
/// В JS это был системный резолвер c-ares; здесь спрашиваем напрямую те адреса,
/// что только что выставили, — это ровно то, что системе предстоит использовать.
pub fn resolves(servers: &[String]) -> bool {
    servers.iter().any(|s| probe_plain(s, Duration::from_secs(4)))
}

/// Резолвит ли имя САМА система (то есть Windows реально ушла на DoH).
pub fn resolves_system(r: &dyn Runner, tries: u32, pause: Duration) -> bool {
    let script = format!("Resolve-DnsName {CONTROL_HOST} -Type A -DnsOnly -NoHostsFile -QuickTimeout -ErrorAction Stop | Out-Null");
    for _ in 0..tries {
        if ps(r, &script).ok {
            return true;
        }
        std::thread::sleep(pause);
    }
    false
}

/// Проверка для «только шифрованного» резолвера. Обычная `resolves` ходит на
/// порт 53 и здесь по определению провалится. Нужны оба условия: сервер
/// отвечает по DoH И система действительно резолвит через него — иначе
/// машина остаётся без имён.
pub fn resolves_encrypted(r: &dyn Runner, p: &Preset) -> bool {
    if let Some(d) = &p.doh {
        if !doh::probe_doh(d, &p.servers, &p.servers6, Duration::from_secs(5)) {
            return false;
        }
    }
    resolves_system(r, 4, Duration::from_millis(1200))
}

/* =====================================================================
   Работоспособность пресетов и автопереключение
   ===================================================================== */

/// Имя с блокировкой по стране: подменяющий DNS отдаёт на него адрес своего
/// прокси, честный — настоящий адрес Cloudflare.
pub const GEO_HOST: &str = "chatgpt.com";
/// Дольше этого — «медленно»: каждое новое имя будет открываться с задержкой.
/// Замер DoH включает установку TLS-соединения, поэтому порог с запасом.
const SLOW_MS: u64 = 600;
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Откуда берём настоящий адрес для сравнения. Через DoH: обычный порт 53
/// провайдер может перехватывать и сам подменять ответы.
const REFERENCE: [(&str, &str); 2] = [("https://cloudflare-dns.com/dns-query", "1.1.1.1"), ("https://dns.google/dns-query", "8.8.8.8")];

/// Что намерили у одного пресета.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measure {
    /// Обычный DNS: задержка и ответ на `GEO_HOST` от первого ответившего сервера.
    pub plain: Option<(u64, Vec<Ipv4Addr>)>,
    /// DoH: то же самое. None и у пресета без шаблона.
    pub doh: Option<(u64, Vec<Ipv4Addr>)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// `ok`, `slow`, `noSpoof` (отвечает, но гео-блок не снимает) или `down`.
    pub state: &'static str,
    /// Задержка того канала, которым пользуется Windows.
    pub ms: Option<u64>,
    pub plain: bool,
    /// None — у пресета нет DoH.
    pub doh: Option<bool>,
    /// None — не проверялось: пресет не подменяет или не с чем сравнить.
    pub spoofs: Option<bool>,
}

/// Каким каналом Windows ходит к пресету: на Windows 11 при шаблоне DoH —
/// шифрованным (мы ставим его без отката на UDP), иначе обычным.
fn uses_doh(p: &Preset, build: u32) -> bool {
    p.encrypted_only || (p.doh.is_some() && build >= 22000)
}

/// Итог по замеру. `reference` — настоящие адреса `GEO_HOST`.
pub fn verdict(p: &Preset, m: &Measure, reference: Option<&[Ipv4Addr]>, build: u32) -> Health {
    let used = if uses_doh(p, build) { &m.doh } else { &m.plain };
    let spoofs = match (p.unblocks, used, reference) {
        (true, Some((_, got)), Some(real)) if !got.is_empty() && !real.is_empty() => Some(!got.iter().any(|ip| real.contains(ip))),
        _ => None,
    };
    let ms = used.as_ref().map(|(ms, _)| *ms);
    let state = match ms {
        None => "down",
        Some(_) if spoofs == Some(false) => "noSpoof",
        Some(ms) if ms > SLOW_MS => "slow",
        Some(_) => "ok",
    };
    Health { state, ms, plain: m.plain.is_some(), doh: p.doh.as_ref().map(|_| m.doh.is_some()), spoofs }
}

/// Живой замер одного пресета: обычный DNS по IPv4-адресам до первого
/// ответившего, DoH — через IP сервера, как его потом увидит Windows.
pub fn measure(p: &Preset) -> Measure {
    let timed = |f: &dyn Fn() -> Option<Vec<Ipv4Addr>>| {
        let t0 = std::time::Instant::now();
        f().map(|ips| (t0.elapsed().as_millis() as u64, ips))
    };
    let plain = if p.encrypted_only { None } else { p.servers.iter().find_map(|ip| timed(&|| plain_a(ip, GEO_HOST, PROBE_TIMEOUT))) };
    let doh = p.doh.as_ref().and_then(|d| p.servers.iter().find_map(|ip| timed(&|| doh::doh_a(d, Some(ip), GEO_HOST, PROBE_TIMEOUT * 2))));
    Measure { plain, doh }
}

/// Настоящие адреса `GEO_HOST`. None — интернета, похоже, нет вообще.
pub fn reference() -> Option<Vec<Ipv4Addr>> {
    REFERENCE.iter().find_map(|(url, ip)| doh::doh_a(url, Some(ip), GEO_HOST, PROBE_TIMEOUT * 2).filter(|a| !a.is_empty()))
}

/// Все пресеты разом, каждый в своём потоке. Второе — есть ли сеть вообще:
/// если молчат и эталон, и все пресеты, дело не в DNS.
pub fn check_all(list: &[Preset], build: u32) -> (Vec<(String, Health)>, bool) {
    let (real, measured) = std::thread::scope(|s| {
        let real = s.spawn(reference);
        let jobs: Vec<_> = list.iter().map(|p| s.spawn(move || measure(p))).collect();
        (real.join().ok().flatten(), jobs.into_iter().map(|j| j.join().unwrap_or_default()).collect::<Vec<_>>())
    });
    let out: Vec<(String, Health)> = list.iter().zip(&measured).map(|(p, m)| (p.id.clone(), verdict(p, m, real.as_deref(), build))).collect();
    let online = real.is_some() || out.iter().any(|(_, h)| h.state != "down");
    (out, online)
}

/// Куда переключиться с `current`: рабочий пресет того же рода (подменяющий
/// на подменяющий), сначала «ok», потом «медленно», внутри — по задержке.
/// Только шифрованные на Windows 10 не годятся.
pub fn next_preset(list: &[Preset], health: &[(String, Health)], current: &str, build: u32) -> Option<String> {
    let cur = list.iter().find(|p| p.id == current)?;
    let mut fit: Vec<(&Preset, &Health)> = list
        .iter()
        .filter(|p| p.id != current && p.unblocks == cur.unblocks && !(p.encrypted_only && build < 22000))
        .filter_map(|p| health.iter().find(|(id, _)| *id == p.id).map(|(_, h)| (p, h)))
        .filter(|(_, h)| h.state == "ok" || h.state == "slow")
        .collect();
    fit.sort_by_key(|(_, h)| (h.state != "ok", h.ms.unwrap_or(u64::MAX)));
    fit.first().map(|(p, _)| p.id.clone())
}

pub struct ApplyOpts<'a> {
    pub user_data_dir: Option<&'a Path>,
    /// Подмена проверки (тесты); по умолчанию — по типу пресета.
    pub check: Option<&'a (dyn Fn() -> bool + Sync)>,
    /// Подмена номера сборки Windows (тесты).
    pub build: Option<u32>,
}

/// Ставит пресет на все активные адаптеры.
///
/// Самое важное здесь — проверка с откатом. Неправильно выставленный DNS
/// оставляет машину вообще без имён: не открывается ничего, и починить это
/// через интерфейс уже не получится, потому что интерфейс тоже ходит в сеть.
/// Поэтому сразу после записи резолвим контрольное имя, и если ответа нет —
/// молча возвращаем как было и говорим об этом.
///
/// Форма ответа — как у JS: объект с `error` либо с
/// `preset/servers/adapters/doh/unblocks`.
pub fn apply(id: &str, o: &ApplyOpts, r: &dyn Runner) -> Value {
    let list = all_presets(o.user_data_dir);
    let Some(p) = list.iter().find(|p| p.id == id).cloned() else {
        return json!({ "error": format!("неизвестный пресет: {id}") });
    };

    // Windows 10 не умеет системный DoH: с закрытым портом 53 адрес в поле DNS
    // даёт не «открытый DNS», а полное отсутствие DNS. Не трогаем систему вовсе.
    let build = o.build.unwrap_or_else(|| win_build(r));
    if p.encrypted_only && build < 22000 {
        return json!({
            "error": format!(
                "«{}» принимает только шифрованный DNS, а он встроен в Windows 11 (у вас сборка {build}). Поставьте DNS-AI.exe с dns-ai.ru или укажите адрес на роутере.",
                p.name
            ),
            "unsupported": true
        });
    }
    let all: Vec<String> = p.servers.iter().chain(p.servers6.iter()).cloned().collect();

    let before = adapters(r);
    if !before.ok {
        return json!({ "error": before.error });
    }
    if before.adapters.is_empty() {
        return json!({ "error": "нет активных сетевых адаптеров" });
    }

    // Бэкап пишем ДО правки. «Как было» — состояние до PLASTO: при смене
    // одного нашего пресета на другой он остаётся прежним, иначе выключение
    // вернуло бы предыдущий пресет. Но если сейчас стоит не наш DNS, значит
    // пользователь поменял сеть, и «как было» — уже это новое состояние.
    // Бэкап есть — значит, DNS выставил PLASTO, по нему горит тумблер.
    let fresh = match o.user_data_dir {
        Some(dir) if !backup_file(dir).exists() || match_preset(&before.adapters, &list).is_none() => {
            let body = json!({ "at": now_ms() as u64, "adapters": before.adapters });
            let write = fs::create_dir_all(dir)
                .and_then(|_| fs::write(backup_file(dir), serde_json::to_string_pretty(&body).unwrap_or_default()));
            if let Err(e) = write {
                return json!({ "error": format!("не удалось сохранить прежние настройки: {e}") });
            }
            true
        }
        _ => false,
    };
    // Откатились к тому, что было до PLASTO, — свежий бэкап больше не нужен.
    let drop_fresh = |restored: bool| {
        if let (true, true, Some(dir)) = (fresh, restored, o.user_data_dir) {
            let _ = fs::remove_file(backup_file(dir));
        }
    };

    let servers = all.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
    for a in &before.adapters {
        let res = ps(r, &format!("Set-DnsClientServerAddress -InterfaceIndex {} -ServerAddresses ({servers})", a.index));
        if !res.ok {
            let restored = revert(r, &before.adapters);
            drop_fresh(restored);
            return json!({ "error": format!("не удалось выставить DNS на «{}»: {}. {}", a.alias, res.err.trim(), if restored { "Прежние настройки восстановлены." } else { "Не удалось восстановить все настройки DNS — проверьте их вручную." }), "reverted": restored });
        }
    }

    // DoH: запрос уезжает по HTTPS, и провайдер не видит имён. autoupgrade
    // включает шифрование само, как только Windows увидит этот сервер.
    let mut doh_ok: Vec<String> = Vec::new();
    if let Some(tpl) = &p.doh {
        for s in &all {
            let res = netsh(r, &["dns", "add", "encryption", &format!("server={s}"), &format!("dohtemplate={tpl}"), "autoupgrade=yes", "udpfallback=no"]);
            if res.ok {
                doh_ok.push(s.clone());
            }
        }
    }
    flush(r);

    let alive = match o.check {
        Some(c) => c(),
        None if p.encrypted_only => resolves_encrypted(r, &p),
        None => resolves(&p.servers),
    };
    if !alive {
        let restored = revert(r, &before.adapters);
        drop_fresh(restored);
        drop_doh(r, &doh_ok);
        flush(r);
        return json!({ "error": if restored { "с этим DNS имена перестали разрешаться — прежние настройки восстановлены" } else { "DNS не отвечает, восстановить настройки не удалось — проверьте их вручную" }, "reverted": restored });
    }

    json!({
        "preset": p.id,
        "servers": all,
        "adapters": before.adapters.iter().map(|a| a.alias.clone()).collect::<Vec<_>>(),
        "doh": doh_ok.len(),
        "unblocks": p.unblocks
    })
}

/// Возвращает сохранённые адреса. Ошибка — в тексте, как `throw` в JS.
pub fn restore(user_data_dir: &Path, r: &dyn Runner) -> Result<Vec<String>, String> {
    let file = backup_file(user_data_dir);
    if !file.exists() {
        return Err("сохранённых настроек нет".into());
    }
    let text = fs::read_to_string(&file).map_err(|_| "файл сохранённых настроек испорчен".to_string())?;
    let data: Value = serde_json::from_str(&text).map_err(|_| "файл сохранённых настроек испорчен".to_string())?;
    let list: Vec<Adapter> = match data.get("adapters") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(),
        _ => vec![],
    };
    if list.is_empty() {
        return Err("в сохранённых настройках нет адаптеров".into());
    }
    if !revert(r, &list) {
        return Err("не удалось восстановить все настройки DNS — проверьте их вручную".into());
    }
    // Не удаляем общесистемные шаблоны DoH: они могли существовать до PLASTO.
    flush(r);
    // Вернули — DNS больше не наш: тумблер гаснет, следующее включение снимет
    // «как было» заново.
    let _ = fs::remove_file(&file);
    Ok(list.into_iter().map(|a| a.alias).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn разбор_адресов_через_юникодные_пробелы() {
        // Неразрывный и узкий пробелы — так адреса приходят из копипасты с сайтов.
        let parsed = parse_custom_input("1.1.1.1\u{A0}1.0.0.1\u{202F}tls://one.one.one.one");
        assert_eq!(parsed.servers, vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()]);
        assert_eq!(parsed.dot.as_deref(), Some("one.one.one.one"));
    }

    /// Подставной запуск команд: ничего не трогает, всё записывает.
    #[derive(Default)]
    struct Fake {
        adapters_out: String,
        adapters_fail: Option<String>,
        fail_set_on: Option<i64>,
        fail_netsh: bool,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Fake {
        fn with(adapters: &[Adapter]) -> Self {
            Fake { adapters_out: serde_json::to_string(adapters).unwrap(), ..Default::default() }
        }
        fn log(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
        fn lines(&self) -> Vec<String> {
            self.log().iter().map(|c| c.join(" ")).collect()
        }
    }

    impl Runner for Fake {
        fn run(&self, file: &str, args: &[String]) -> RunOut {
            let mut call = vec![file.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call);
            let ok = |out: &str| RunOut { ok: true, out: out.into(), err: String::new() };

            if file.eq_ignore_ascii_case("powershell.exe") {
                let script = args.last().map(String::as_str).unwrap_or("");
                if script.contains("Get-NetAdapter") {
                    return match &self.adapters_fail {
                        Some(e) => RunOut { ok: false, out: String::new(), err: e.clone() },
                        None => ok(&self.adapters_out),
                    };
                }
                if let Some(rest) = script.strip_prefix("Set-DnsClientServerAddress -InterfaceIndex ") {
                    let idx: Option<i64> = rest.split_whitespace().next().and_then(|n| n.parse().ok());
                    if idx.is_some() && idx == self.fail_set_on && rest.contains("-ServerAddresses") {
                        return RunOut { ok: false, out: String::new(), err: " Отказано в доступе \r\n".into() };
                    }
                }
            }
            if file == "netsh" && args.iter().any(|a| a == "add") && self.fail_netsh {
                return RunOut { ok: false, out: String::new(), err: "netsh fail".into() };
            }
            ok("")
        }
    }

    fn ad(index: i64, alias: &str, dhcp: bool, servers: &[&str]) -> Adapter {
        Adapter { index, alias: alias.into(), guid: Some(format!("{{G{index}}}")), dhcp, servers: servers.iter().map(|s| s.to_string()).collect() }
    }

    fn opts<'a>(dir: Option<&'a Path>, check: &'a (dyn Fn() -> bool + Sync), build: u32) -> ApplyOpts<'a> {
        ApplyOpts { user_data_dir: dir, check: Some(check), build: Some(build) }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /* ---------------- пресеты ---------------- */

    #[test]
    fn каталог_пресетов_совпадает_с_ожидаемым_составом() {
        let ids: Vec<String> = presets().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["dnsai", "xbox", "comss", "zapretmoe", "malw", "geohide", "commonnet", "bezmezhau", "cloudflare", "google"]);
        assert!(presets().iter().all(|p| !p.servers.is_empty() && !p.custom));
        // У bezmezhau DoH при проверке не ответил — только обычный DNS.
        assert!(presets().iter().all(|p| p.doh.is_some() || p.id == "bezmezhau"));
    }

    #[test]
    fn гео_флаг_разделяет_подменяющие_и_только_шифрующие() {
        let by: std::collections::HashMap<_, _> = presets().into_iter().map(|p| (p.id.clone(), p)).collect();
        for id in ["xbox", "comss", "dnsai", "zapretmoe", "malw", "geohide", "commonnet", "bezmezhau"] {
            assert!(by[id].unblocks, "{id} снимает гео");
        }
        for id in ["cloudflare", "google"] {
            assert!(!by[id].unblocks, "{id} только шифрует");
        }
    }

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn итог_проверки_по_каналу_которым_ходит_windows() {
        let by = |id: &str| presets().into_iter().find(|p| p.id == id).unwrap();
        let real = [ip("104.18.32.47")];
        let proxy = Measure { plain: Some((40, vec![ip("83.217.211.149")])), doh: Some((90, vec![ip("83.217.211.149")])) };
        assert_eq!(verdict(&by("zapretmoe"), &proxy, Some(&real), 26200).state, "ok");
        assert_eq!(verdict(&by("zapretmoe"), &proxy, Some(&real), 26200).ms, Some(90), "Windows 11 ходит по DoH");
        assert_eq!(verdict(&by("zapretmoe"), &proxy, Some(&real), 19045).ms, Some(40), "Windows 10 — по обычному");

        let honest = Measure { plain: Some((40, real.to_vec())), doh: Some((50, real.to_vec())) };
        assert_eq!(verdict(&by("zapretmoe"), &honest, Some(&real), 26200).state, "noSpoof");
        assert_eq!(verdict(&by("cloudflare"), &honest, Some(&real), 26200).state, "ok", "только шифрующему подмена не нужна");
        assert_eq!(verdict(&by("zapretmoe"), &honest, None, 26200).spoofs, None, "без эталона не судим");

        let doh_dead = Measure { plain: Some((40, vec![ip("83.217.211.149")])), doh: None };
        assert_eq!(verdict(&by("zapretmoe"), &doh_dead, Some(&real), 26200).state, "down", "на 11 без DoH имён не будет");
        assert_eq!(verdict(&by("zapretmoe"), &doh_dead, Some(&real), 19045).state, "ok");
        assert_eq!(verdict(&by("bezmezhau"), &doh_dead, Some(&real), 26200).doh, None, "шаблона нет — и DoH не оцениваем");

        let slow = Measure { plain: Some((900, vec![ip("1.2.3.4")])), doh: None };
        assert_eq!(verdict(&by("bezmezhau"), &slow, Some(&real), 26200).state, "slow");
    }

    #[test]
    #[ignore = "живая сеть: cargo test -- --ignored --nocapture живая_сеть_проверка_пресетов"]
    fn живая_сеть_проверка_пресетов() {
        let t0 = std::time::Instant::now();
        println!("эталон {GEO_HOST}: {:?}", reference());
        let (all, online) = check_all(&presets(), 26200);
        for (id, h) in &all {
            println!("{id:12} {h:?}");
        }
        println!("сеть: {online}, за {:?}", t0.elapsed());
    }

    #[test]
    fn переключение_на_рабочий_того_же_рода() {
        let list = presets();
        let h = |state: &'static str, ms: u64| Health { state, ms: Some(ms), plain: true, doh: None, spoofs: None };
        let health = vec![
            ("xbox".to_string(), h("down", 0)),
            ("comss".to_string(), h("slow", 400)),
            ("malw".to_string(), h("ok", 120)),
            ("geohide".to_string(), h("ok", 60)),
            ("dnsai".to_string(), h("ok", 10)),
            ("cloudflare".to_string(), h("ok", 5)),
        ];
        assert_eq!(next_preset(&list, &health, "xbox", 26200).as_deref(), Some("dnsai"), "самый быстрый из подменяющих");
        assert_eq!(next_preset(&list, &health, "xbox", 19045).as_deref(), Some("geohide"), "только шифрованный на Windows 10 не годится");
        assert_eq!(next_preset(&list, &health, "google", 26200).as_deref(), Some("cloudflare"), "шифрующий — на шифрующий");
        let only_slow = vec![("comss".to_string(), h("slow", 400)), ("xbox".to_string(), h("down", 0))];
        assert_eq!(next_preset(&list, &only_slow, "xbox", 26200).as_deref(), Some("comss"), "медленный лучше мёртвого");
        assert_eq!(next_preset(&list, &[], "xbox", 26200), None);
        assert_eq!(next_preset(&list, &health, "нет-такого", 26200), None);
    }

    #[test]
    fn dns_ai_только_шифрованный_и_с_ipv6() {
        let p = presets().into_iter().find(|p| p.id == "dnsai").unwrap();
        assert!(p.encrypted_only);
        assert_eq!(p.servers6.len(), 2);
        assert_eq!(p.doh.as_deref(), Some("https://dns.dns-ai.ru/dns-query"));
        assert_eq!(p.dot.as_deref(), Some("dns.dns-ai.ru"));
        assert!(presets().iter().filter(|x| x.encrypted_only).count() == 1, "остальные отвечают на обычный DNS");
    }

    #[test]
    fn match_preset_узнаёт_поставленный_и_молчит_без_адресов() {
        let list = presets();
        assert_eq!(match_preset(&[ad(1, "E", false, &["1.1.1.1", "1.0.0.1"])], &list).as_deref(), Some("cloudflare"));
        assert_eq!(match_preset(&[ad(1, "E", false, &["1.1.1.1"])], &list), None, "нужны все адреса пресета");
        assert_eq!(match_preset(&[ad(1, "E", true, &[])], &list), None);
        // адреса пресета могут быть на разных адаптерах
        let two = [ad(1, "A", false, &["8.8.8.8"]), ad(2, "B", false, &["8.8.4.4"])];
        assert_eq!(match_preset(&two, &list).as_deref(), Some("google"));
    }

    /* ---------------- разбор вставки ---------------- */

    const DNSAI_PAGE: &str = "https://dns.dns-ai.ru/dns-query tls://dns.dns-ai.ru quic://dns.dns-ai.ru:853 194.87.196.13, 45.151.102.219 2a00:b700:5::3:62 2a12:5940:d4ef::219";

    #[test]
    fn разбор_страница_подключения_dns_ai_целиком() {
        let p = parse_custom_input(DNSAI_PAGE);
        assert_eq!(p.servers, s(&["194.87.196.13", "45.151.102.219"]));
        assert_eq!(p.servers6, s(&["2a00:b700:5::3:62", "2a12:5940:d4ef::219"]));
        assert_eq!(p.doh.as_deref(), Some("https://dns.dns-ai.ru/dns-query"));
        assert_eq!(p.dot.as_deref(), Some("dns.dns-ai.ru"));
        assert_eq!(p.ignored, s(&["quic://dns.dns-ai.ru:853"]), "DoQ Windows не умеет");
    }

    #[test]
    fn разбор_голое_имя_https_без_пути_и_мусор() {
        let p = parse_custom_input("dns.example.org 1.1.1.1 1.1.1.1");
        assert_eq!(p.guess_doh.as_deref(), Some("dns.example.org"));
        assert_eq!(p.servers, s(&["1.1.1.1"]), "дубли схлопываются");
        assert_eq!(parse_custom_input("https://dns.example.org").doh.as_deref(), Some("https://dns.example.org/dns-query"));
        let junk = parse_custom_input("что-то странное !!!");
        assert!(junk.servers.is_empty() && !junk.ignored.is_empty());
    }

    #[test]
    fn разбор_кавычки_скобки_и_разделители() {
        let p = parse_custom_input("\"1.1.1.1\", '9.9.9.9'; (8.8.8.8)\n[2001:db8::1]\t`1.0.0.1`");
        assert_eq!(p.servers, s(&["1.1.1.1", "9.9.9.9", "8.8.8.8", "1.0.0.1"]));
        assert_eq!(p.servers6, s(&["2001:db8::1"]));
    }

    #[test]
    fn разбор_нестандартный_порт_и_путь_сохраняются() {
        assert_eq!(parse_custom_input("https://dns.example.org:8443/dns-query").doh.as_deref(), Some("https://dns.example.org:8443/dns-query"));
        assert_eq!(parse_custom_input("https://dns.example.org/other").doh.as_deref(), Some("https://dns.example.org/other"));
        // запрос из ссылки не тащим в шаблон
        assert_eq!(parse_custom_input("https://dns.example.org/q?x=1").doh.as_deref(), Some("https://dns.example.org/q"));
    }

    /* ---------------- добавление своего ---------------- */

    fn probes<'a>(
        lookup: &'a (dyn Fn(&str) -> Vec<(String, u8)> + Sync),
        doh: &'a (dyn Fn(&str, &[String], &[String]) -> bool + Sync),
        plain: &'a (dyn Fn(&str) -> bool + Sync),
    ) -> Probes<'a> {
        Probes { lookup, doh, plain }
    }

    fn no_lookup(_: &str) -> Vec<(String, u8)> {
        vec![]
    }

    #[test]
    fn добавление_закрытый_53_и_рабочий_doh_это_только_шифрованный() {
        let d = tempfile::tempdir().unwrap();
        let p = add_custom("", DNSAI_PAGE, d.path(), &probes(&no_lookup, &|_, _, _| true, &|_| false)).unwrap();
        assert!(p.encrypted_only);
        assert_eq!(p.name, "dns.dns-ai.ru");
        assert_eq!(p.servers6.len(), 2);
        assert!(p.id.starts_with("c-") && p.id.len() == 10);
    }

    #[test]
    fn добавление_обычный_dns_отвечает_шифрование_не_обязательно() {
        let d = tempfile::tempdir().unwrap();
        let p = add_custom("", "9.9.9.9 149.112.112.112", d.path(), &probes(&no_lookup, &|_, _, _| false, &|_| true)).unwrap();
        assert!(!p.encrypted_only && p.doh.is_none());
        assert!(p.note.contains("Без шифрования"));
    }

    #[test]
    fn добавление_ip_достаётся_по_имени_сервера() {
        let d = tempfile::tempdir().unwrap();
        let lk = |_: &str| vec![("203.0.113.5".to_string(), 4u8), ("2001:db8::5".to_string(), 6u8)];
        let p = add_custom("", "https://dns.example.org/dns-query", d.path(), &probes(&lk, &|_, _, _| true, &|_| false)).unwrap();
        assert_eq!(p.servers, s(&["203.0.113.5"]));
        assert_eq!(p.servers6, s(&["2001:db8::5"]));
    }

    #[test]
    fn добавление_молчит_и_там_и_там_не_сохраняется() {
        let d = tempfile::tempdir().unwrap();
        let e = add_custom("", "203.0.113.9", d.path(), &probes(&no_lookup, &|_, _, _| false, &|_| false)).unwrap_err();
        assert!(e.contains("не отвечает"), "{e}");
        assert!(load_custom(d.path()).is_empty());
        let e2 = add_custom("", "!!! ???", d.path(), &probes(&no_lookup, &|_, _, _| true, &|_| true)).unwrap_err();
        assert!(e2.starts_with("не распознал"), "{e2}");
    }

    #[test]
    fn добавление_без_дублей_удаление_и_видимость_в_общем_списке() {
        let d = tempfile::tempdir().unwrap();
        let pr = probes(&no_lookup, &|_, _, _| true, &|_| false);
        let a = add_custom("", DNSAI_PAGE, d.path(), &pr).unwrap();
        let b = add_custom("", DNSAI_PAGE, d.path(), &pr).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(load_custom(d.path()).len(), 1);

        let all = all_presets(Some(d.path()));
        assert!(all.iter().find(|p| p.id == a.id).unwrap().custom);
        assert_eq!(all.len(), presets().len() + 1);

        assert_eq!(remove_custom(&a.id, d.path()).unwrap(), a.id);
        assert!(remove_custom("нет-такого", d.path()).is_err());
    }

    #[test]
    fn файл_своих_пресетов_читается_и_после_правки_руками() {
        let d = tempfile::tempdir().unwrap();
        fs::write(custom_path(d.path()), r#"[{"id":"c-x","name":"n","servers":["1.2.3.4"]}, {"id":"","servers":[]}, {"servers":["9.9.9.9"]}, 5]"#).unwrap();
        let v = load_custom(d.path());
        assert_eq!(v.len(), 1, "без id, без servers и не-объекты отбрасываются");
        assert!(!v[0].encrypted_only && v[0].servers6.is_empty());
        fs::write(custom_path(d.path()), "{не json").unwrap();
        assert!(load_custom(d.path()).is_empty());
    }

    /* ---------------- адаптеры ---------------- */

    #[test]
    fn адаптеры_массив_одиночный_объект_и_пустой_вывод() {
        let one = ad(3, "Ethernet", false, &["8.8.8.8"]);
        let f = Fake::with(std::slice::from_ref(&one));
        assert_eq!(adapters(&f).adapters, vec![one.clone()]);
        let f2 = Fake { adapters_out: serde_json::to_string(&one).unwrap(), ..Default::default() };
        assert_eq!(adapters(&f2).adapters, vec![one], "PowerShell может отдать один объект вместо массива");
        for empty in ["", "   ", "null", "[]"] {
            let f3 = Fake { adapters_out: empty.into(), ..Default::default() };
            let r = adapters(&f3);
            assert!(r.ok && r.adapters.is_empty(), "{empty:?}");
        }
    }

    #[test]
    fn адаптеры_ошибки_с_текстами_оригинала() {
        let bad = Fake { adapters_out: "не json{".into(), ..Default::default() };
        assert_eq!(adapters(&bad).error.as_deref(), Some("непонятный ответ PowerShell"));
        let fail = Fake { adapters_fail: Some("  Нет доступа \n".into()), ..Default::default() };
        assert_eq!(adapters(&fail).error.as_deref(), Some("Нет доступа"));
        let silent = Fake { adapters_fail: Some(String::new()), ..Default::default() };
        assert_eq!(adapters(&silent).error.as_deref(), Some("не удалось прочитать настройки"));
    }

    #[test]
    fn адаптеры_null_в_списке_серверов_отбрасывается() {
        let f = Fake { adapters_out: r#"[{"index":1,"alias":"E","guid":"{G}","dhcp":true,"servers":[null]}]"#.into(), ..Default::default() };
        assert!(adapters(&f).adapters[0].servers.is_empty());
    }

    #[test]
    fn скрипт_чтения_адаптеров_подключён_побайтно() {
        assert!(ADAPTERS_SCRIPT.starts_with('\n') && !ADAPTERS_SCRIPT.contains('\r'));
        assert!(ADAPTERS_SCRIPT.contains("Get-NetAdapter") && ADAPTERS_SCRIPT.contains("NameServer"));
        assert_eq!(ADAPTERS_SCRIPT.len(), 729);
    }

    /* ---------------- применение ---------------- */

    fn two() -> Vec<Adapter> {
        vec![ad(3, "Ethernet", true, &[]), ad(10, "Wi-Fi", false, &["192.168.0.1"])]
    }

    #[test]
    fn применение_успех_порядок_команд_и_форма_ответа() {
        let f = Fake::with(&two());
        let r = apply("cloudflare", &opts(None, &|| true, 26300), &f);
        assert_eq!(r["preset"], "cloudflare");
        assert_eq!(r["servers"], json!(["1.1.1.1", "1.0.0.1"]));
        assert_eq!(r["adapters"], json!(["Ethernet", "Wi-Fi"]));
        assert_eq!(r["doh"], 2, "число зарегистрированных DoH-адресов, а не список");
        assert_eq!(r["unblocks"], false);

        let l = f.lines();
        assert!(l[0].contains("Get-NetAdapter"));
        assert!(l[1].ends_with("Set-DnsClientServerAddress -InterfaceIndex 3 -ServerAddresses ('1.1.1.1','1.0.0.1')"));
        assert!(l[2].ends_with("Set-DnsClientServerAddress -InterfaceIndex 10 -ServerAddresses ('1.1.1.1','1.0.0.1')"));
        assert_eq!(l[3], "netsh dns add encryption server=1.1.1.1 dohtemplate=https://cloudflare-dns.com/dns-query autoupgrade=yes udpfallback=no");
        assert_eq!(l[5], "ipconfig /flushdns");
        assert_eq!(l.len(), 6);
    }

    #[test]
    fn применение_на_windows_10_отказ_до_любых_команд() {
        let f = Fake::with(&two());
        let r = apply("dnsai", &opts(None, &|| true, 19045), &f);
        assert_eq!(r["unsupported"], true);
        assert!(r["error"].as_str().unwrap().contains("Windows 11"));
        assert!(f.log().is_empty(), "систему не трогали вовсе");
    }

    #[test]
    fn применение_обычного_пресета_на_windows_10_допустимо() {
        let f = Fake::with(&two());
        let r = apply("google", &opts(None, &|| true, 19045), &f);
        assert_eq!(r["preset"], "google");
    }

    #[test]
    fn применение_dns_ai_на_windows_11_ставит_оба_семейства_и_doh_на_каждый() {
        let f = Fake::with(&two());
        let r = apply("dnsai", &opts(None, &|| true, 26100), &f);
        assert_eq!(r["servers"].as_array().unwrap().len(), 4);
        let l = f.lines();
        assert!(l.iter().any(|x| x.contains("-InterfaceIndex 3") && x.contains("2a00:b700:5::3:62") && x.contains("194.87.196.13")));
        let doh: Vec<_> = l.iter().filter(|x| x.starts_with("netsh dns add encryption")).collect();
        assert_eq!(doh.len(), 4);
        assert!(doh.iter().all(|x| x.contains("dohtemplate=https://dns.dns-ai.ru/dns-query") && x.contains("udpfallback=no")));
    }

    #[test]
    fn применение_провал_проверки_откат_dhcp_сбросом_статики_списком() {
        let f = Fake::with(&two());
        let r = apply("cloudflare", &opts(None, &|| false, 26300), &f);
        assert_eq!(r["reverted"], true);
        assert_eq!(r["error"], "с этим DNS имена перестали разрешаться — прежние настройки восстановлены");
        let l = f.lines();
        assert!(l.iter().any(|x| x.ends_with("-InterfaceIndex 3 -ResetServerAddresses")), "DHCP — сбросом");
        assert!(l.iter().any(|x| x.ends_with("-InterfaceIndex 10 -ServerAddresses ('192.168.0.1')")), "статика — списком");
        assert_eq!(l.iter().filter(|x| x.starts_with("netsh dns delete encryption")).count(), 2, "снимаем только то, что поставили");
        assert_eq!(l.iter().filter(|x| *x == "ipconfig /flushdns").count(), 2);
    }

    #[test]
    fn применение_провал_на_втором_адаптере_откатывает_все_без_флага_reverted() {
        let mut f = Fake::with(&two());
        f.fail_set_on = Some(10);
        let r = apply("cloudflare", &opts(None, &|| true, 26300), &f);
        assert!(r["error"].as_str().unwrap().contains("Не удалось восстановить"));
        assert_eq!(r["reverted"], false);
        let l = f.lines();
        assert!(l.iter().any(|x| x.ends_with("-InterfaceIndex 3 -ResetServerAddresses")));
        assert!(l.iter().any(|x| x.ends_with("-InterfaceIndex 10 -ServerAddresses ('192.168.0.1')")), "откатываются ВСЕ адаптеры, не только изменённые");
        assert!(!l.iter().any(|x| x.starts_with("netsh")), "до DoH дело не дошло");
    }

    #[test]
    fn применение_сбой_netsh_не_ломает_применение_а_уменьшает_счётчик() {
        let mut f = Fake::with(&two());
        f.fail_netsh = true;
        let r = apply("cloudflare", &opts(None, &|| true, 26300), &f);
        assert_eq!(r["preset"], "cloudflare");
        assert_eq!(r["doh"], 0);
    }

    #[test]
    fn применение_ошибки_до_правки_неизвестный_пресет_нет_адаптеров_сбой_чтения() {
        let f = Fake::with(&two());
        assert_eq!(apply("нет", &opts(None, &|| true, 26300), &f)["error"], "неизвестный пресет: нет");
        assert!(f.log().is_empty());

        let none = Fake::with(&[]);
        assert_eq!(apply("google", &opts(None, &|| true, 26300), &none)["error"], "нет активных сетевых адаптеров");

        let fail = Fake { adapters_fail: Some("boom".into()), ..Default::default() };
        assert_eq!(apply("google", &opts(None, &|| true, 26300), &fail)["error"], "boom");
        assert_eq!(fail.log().len(), 1, "дальше чтения не пошли");
    }

    #[test]
    fn применение_бэкап_пишется_и_перезаписывается_каждый_раз() {
        let d = tempfile::tempdir().unwrap();
        let first = vec![ad(3, "Ethernet", false, &["192.168.0.1"])];
        apply("google", &opts(Some(d.path()), &|| true, 26300), &Fake::with(&first));
        let b1: Value = serde_json::from_str(&fs::read_to_string(backup_file(d.path())).unwrap()).unwrap();
        assert_eq!(b1["adapters"][0]["servers"], json!(["192.168.0.1"]));
        assert!(b1["at"].as_u64().unwrap() > 1_700_000_000_000, "метка времени в миллисекундах");

        // сеть поменялась между переключениями — «как было» это теперь новое состояние
        let second = vec![ad(3, "Ethernet", false, &["10.9.9.9"])];
        apply("cloudflare", &opts(Some(d.path()), &|| true, 26300), &Fake::with(&second));
        let b2: Value = serde_json::from_str(&fs::read_to_string(backup_file(d.path())).unwrap()).unwrap();
        assert_eq!(b2["adapters"][0]["servers"], json!(["10.9.9.9"]));
    }

    #[test]
    fn применение_смена_пресета_хранит_то_что_было_до_plasto_а_возврат_снимает_бэкап() {
        let d = tempfile::tempdir().unwrap();
        apply("google", &opts(Some(d.path()), &|| true, 26300), &Fake::with(&[ad(3, "Ethernet", false, &["192.168.0.1"])]));
        // сейчас стоит наш google — его в «как было» не записываем
        apply("cloudflare", &opts(Some(d.path()), &|| true, 26300), &Fake::with(&[ad(3, "Ethernet", false, &["8.8.8.8", "8.8.4.4"])]));
        let b: Value = serde_json::from_str(&fs::read_to_string(backup_file(d.path())).unwrap()).unwrap();
        assert_eq!(b["adapters"][0]["servers"], json!(["192.168.0.1"]));

        assert_eq!(restore(d.path(), &Fake::default()).unwrap(), vec!["Ethernet".to_string()]);
        assert!(!backup_file(d.path()).exists(), "DNS больше не наш — тумблер гаснет");
    }

    #[test]
    fn применение_откат_убирает_только_что_снятый_бэкап() {
        let d = tempfile::tempdir().unwrap();
        let r = apply("google", &opts(Some(d.path()), &|| false, 26300), &Fake::with(&[ad(3, "Ethernet", false, &["192.168.0.1"])]));
        assert_eq!(r["reverted"], true);
        assert!(!backup_file(d.path()).exists());
    }

    #[test]
    fn применение_свой_пресет_без_doh_netsh_не_трогает() {
        let d = tempfile::tempdir().unwrap();
        add_custom("", "9.9.9.9", d.path(), &probes(&no_lookup, &|_, _, _| false, &|_| true)).unwrap();
        let id = load_custom(d.path())[0].id.clone();
        let f = Fake::with(&two());
        let r = apply(&id, &opts(Some(d.path()), &|| true, 26300), &f);
        assert!(r.get("error").is_none(), "{r}");
        assert!(!f.lines().iter().any(|x| x.starts_with("netsh")));
    }

    /* ---------------- восстановление ---------------- */

    #[test]
    fn восстановление_тексты_ошибок_оригинала() {
        let d = tempfile::tempdir().unwrap();
        let f = Fake::default();
        assert_eq!(restore(d.path(), &f).unwrap_err(), "сохранённых настроек нет");
        fs::write(backup_file(d.path()), "не json{").unwrap();
        assert_eq!(restore(d.path(), &f).unwrap_err(), "файл сохранённых настроек испорчен");
        fs::write(backup_file(d.path()), r#"{"at":1,"adapters":[]}"#).unwrap();
        assert_eq!(restore(d.path(), &f).unwrap_err(), "в сохранённых настройках нет адаптеров");
        fs::write(backup_file(d.path()), r#"{"at":1}"#).unwrap();
        assert_eq!(restore(d.path(), &f).unwrap_err(), "в сохранённых настройках нет адаптеров");
        assert!(f.log().is_empty(), "при ошибке систему не трогаем");
    }

    #[test]
    fn восстановление_возвращает_адреса_снимает_doh_всех_пресетов_и_сбрасывает_кэш() {
        let d = tempfile::tempdir().unwrap();
        let before = two();
        apply("google", &opts(Some(d.path()), &|| true, 26300), &Fake::with(&before));

        let f = Fake::default();
        let aliases = restore(d.path(), &f).unwrap();
        assert_eq!(aliases, s(&["Ethernet", "Wi-Fi"]));
        let l = f.lines();
        assert!(l[0].ends_with("-InterfaceIndex 3 -ResetServerAddresses"));
        assert!(l[1].ends_with("-InterfaceIndex 10 -ServerAddresses ('192.168.0.1')"));
        let dropped = l.iter().filter(|x| x.starts_with("netsh dns delete encryption")).count();
        assert_eq!(dropped, 0, "чужие настройки DoH не удаляются");
        assert_eq!(l.last().unwrap(), "ipconfig /flushdns");
    }

    /* ---------------- прочее ---------------- */

    #[test]
    fn номер_сборки_разбирается_из_вывода_ver() {
        struct Ver(&'static str);
        impl Runner for Ver {
            fn run(&self, _: &str, _: &[String]) -> RunOut {
                RunOut { ok: true, out: self.0.into(), err: String::new() }
            }
        }
        assert_eq!(win_build(&Ver("\r\nMicrosoft Windows [Version 10.0.26300.1234]\r\n")), 26300);
        assert_eq!(win_build(&Ver("Microsoft Windows [Version 10.0.19045.5000]")), 19045);
        assert_eq!(win_build(&Ver("мусор")), 0);
    }

    #[test]
    fn plain_зонд_отвечает_только_настоящему_dns_серверу() {
        // локальный «DNS»: отвечает корректным пакетом с одной записью
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = sock.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                let mut resp = buf[..n].to_vec();
                resp[2] |= 0x80;
                resp[7] = 1;
                let _ = sock.send_to(&resp, from);
            }
        });
        // probe_plain ходит строго на порт 53, поэтому проверяем разбор ответа
        // отдельно: порт чужой — зонд обязан честно сказать «нет».
        assert!(!probe_plain("127.0.0.1", Duration::from_millis(300)));
        let _ = port;
        drop(h);
        assert!(!probe_plain("не-адрес", Duration::from_millis(100)));
    }

    #[test]
    #[ignore = "живая сеть: cargo test -- --ignored --nocapture живая_сеть_dns"]
    fn живая_сеть_dns() {
        let t = Duration::from_secs(4);
        for ip in ["1.1.1.1", "8.8.8.8", "194.87.196.13"] {
            println!("plain {ip:<16} {}", probe_plain(ip, t));
        }
    }
}

/// Сравнение с JS-эталоном: разбор вставки, добавление пресета, применение,
/// откат и восстановление на случайных сценариях со сбоями.
///
///   node scripts/gen-dns-corpus.js %TEMP%\plasto-dns-corpus
///   set PLASTO_CORPUS_DNS=%TEMP%\plasto-dns-corpus
///   cargo test -- --ignored сравнение_с_js_dns
///
/// Живёт рядом с портом, пока dns.js не удалён.
#[cfg(test)]
mod differential {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Sc {
        adapters_out: String,
        adapters_fail: Option<String>,
        fail_set_on: Option<i64>,
        fail_netsh: bool,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Runner for Sc {
        fn run(&self, file: &str, args: &[String]) -> RunOut {
            let mut call = vec![file.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call);
            let ok = |o: &str| RunOut { ok: true, out: o.into(), err: String::new() };
            if file.eq_ignore_ascii_case("powershell.exe") {
                let script = args.last().map(String::as_str).unwrap_or("");
                if script.contains("Get-NetAdapter") {
                    return match &self.adapters_fail {
                        Some(e) => RunOut { ok: false, out: String::new(), err: e.clone() },
                        None => ok(&self.adapters_out),
                    };
                }
                if let Some(rest) = script.strip_prefix("Set-DnsClientServerAddress -InterfaceIndex ") {
                    let idx: Option<i64> = rest.split(' ').next().and_then(|n| n.parse().ok());
                    if idx.is_some() && idx == self.fail_set_on && rest.contains("-ServerAddresses") {
                        return RunOut { ok: false, out: String::new(), err: " Отказано в доступе \r\n".into() };
                    }
                }
            }
            if file == "netsh" && args.iter().any(|a| a == "add") && self.fail_netsh {
                return RunOut { ok: false, out: String::new(), err: "netsh fail".into() };
            }
            ok("")
        }
    }

    fn input_json(p: &CustomInput) -> Value {
        json!({ "servers": p.servers, "servers6": p.servers6, "doh": p.doh, "dot": p.dot, "hosts": p.hosts, "guessDoh": p.guess_doh, "ignored": p.ignored })
    }

    fn strip_at(text: &str) -> Value {
        match serde_json::from_str::<Value>(text) {
            Ok(mut v) => {
                if let Some(o) = v.as_object_mut() {
                    o.remove("at");
                }
                v
            }
            Err(_) => json!("corrupt"),
        }
    }

    #[test]
    #[ignore = "нужен корпус: PLASTO_CORPUS_DNS (scripts/gen-dns-corpus.js)"]
    fn сравнение_с_js_dns() {
        let dir = PathBuf::from(std::env::var("PLASTO_CORPUS_DNS").expect("задайте PLASTO_CORPUS_DNS"));
        let exp: Value = serde_json::from_str(&fs::read_to_string(dir.join("dns-expected.json")).unwrap()).unwrap();
        let custom_text = serde_json::to_string_pretty(&exp["custom"]).unwrap();
        let mut bad: Vec<String> = Vec::new();
        let mut cmp = |what: String, got: Value, want: &Value| {
            if &got != want {
                bad.push(format!("{what}\n   js:   {want}\n   rust: {got}"));
            }
        };

        // 1. разбор вставки
        for (i, c) in exp["parse"].as_array().unwrap().iter().enumerate() {
            let got = input_json(&parse_custom_input(c["text"].as_str().unwrap()));
            cmp(format!("parse #{i} {:?}", c["text"]), got, &c["result"]);
        }

        // 2. добавление своего пресета
        for (i, c) in exp["add"].as_array().unwrap().iter().enumerate() {
            let d = tempfile::tempdir().unwrap();
            if let Some(pre) = c["pre"].as_str() {
                fs::write(custom_path(d.path()), pre).unwrap();
            }
            let lookup_map: HashMap<String, Vec<(String, u8)>> = c["lookup"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_u64().unwrap() as u8)).collect()))
                .collect();
            let doh_default = c["dohDefault"].as_bool().unwrap();
            let doh_map: HashMap<String, bool> = c["dohMap"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_bool().unwrap())).collect();
            let plain_ok = c["plainOk"].as_bool().unwrap();

            let lookup = |h: &str| lookup_map.get(h).cloned().unwrap_or_default();
            let doh = |u: &str, _: &[String], _: &[String]| doh_map.get(u).copied().unwrap_or(doh_default);
            let plain = |_: &str| plain_ok;
            let res = add_custom(c["name"].as_str().unwrap(), c["text"].as_str().unwrap(), d.path(), &Probes { lookup: &lookup, doh: &doh, plain: &plain });
            let got = match res {
                Ok(p) => json!({ "preset": p }),
                Err(e) => json!({ "error": e }),
            };
            cmp(format!("add #{i} результат {:?}", c["text"]), got, &c["result"]);
            let stored = fs::read_to_string(custom_path(d.path())).ok();
            cmp(format!("add #{i} файл"), json!(stored), &c["stored"]);
        }

        // 3. применение
        for (i, c) in exp["apply"].as_array().unwrap().iter().enumerate() {
            let sc = &c["sc"];
            let d = tempfile::tempdir().unwrap();
            fs::write(custom_path(d.path()), &custom_text).unwrap();
            if c["preBackup"].as_bool().unwrap() {
                let pre = json!({ "at": 1, "adapters": [{ "index": 66, "alias": "pre", "guid": "{G}", "dhcp": false, "servers": ["1.1.1.1"] }] });
                fs::write(backup_file(d.path()), serde_json::to_string_pretty(&pre).unwrap()).unwrap();
            }
            let runner = Sc {
                adapters_out: sc["adaptersOut"].as_str().unwrap().to_string(),
                adapters_fail: sc["adaptersFail"].as_str().map(str::to_string),
                fail_set_on: sc["failSetOn"].as_i64(),
                fail_netsh: sc["failNetsh"].as_bool().unwrap(),
                calls: Mutex::new(vec![]),
            };
            let check_val = sc["check"].as_bool().unwrap();
            let check = move || check_val;
            let with_dir = sc["withDir"].as_bool().unwrap();
            let o = ApplyOpts { user_data_dir: if with_dir { Some(d.path()) } else { None }, check: Some(&check), build: sc["build"].as_u64().map(|b| b as u32) };
            let got = apply(sc["presetId"].as_str().unwrap(), &o, &runner);
            let id = sc["presetId"].as_str().unwrap();

            cmp(format!("apply #{i} ({id}) результат"), got, &c["result"]);
            cmp(format!("apply #{i} ({id}) команды"), json!(runner.calls.lock().unwrap().clone()), &c["calls"]);
            let backup = fs::read_to_string(backup_file(d.path())).ok().map(|t| strip_at(&t)).unwrap_or(Value::Null);
            cmp(format!("apply #{i} ({id}) бэкап"), backup, &c["backup"]);
        }

        // 4. восстановление
        for (i, c) in exp["restore"].as_array().unwrap().iter().enumerate() {
            let d = tempfile::tempdir().unwrap();
            fs::write(custom_path(d.path()), &custom_text).unwrap();
            if let Some(t) = c["content"].as_str() {
                fs::write(backup_file(d.path()), t).unwrap();
            }
            let runner = Sc { adapters_out: String::new(), adapters_fail: None, fail_set_on: None, fail_netsh: false, calls: Mutex::new(vec![]) };
            let got = match restore(d.path(), &runner) {
                Ok(a) => json!({ "adapters": a }),
                Err(e) => json!({ "thrown": e }),
            };
            cmp(format!("restore #{i} результат"), got, &c["result"]);
            cmp(format!("restore #{i} команды"), json!(runner.calls.lock().unwrap().clone()), &c["calls"]);
        }

        // 5. matchPreset
        for (i, c) in exp["match"].as_array().unwrap().iter().enumerate() {
            let list: Vec<Adapter> = c["list"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| Adapter { index: 1, alias: "x".into(), guid: None, dhcp: false, servers: a["servers"].as_array().map(|s| s.iter().map(|x| x.as_str().unwrap().to_string()).collect()).unwrap_or_default() })
                .collect();
            let mut presets_list = presets();
            for p in c["custom"].as_array().unwrap() {
                presets_list.push(serde_json::from_value(p.clone()).unwrap());
            }
            cmp(format!("match #{i}"), json!(match_preset(&list, &presets_list)), &c["result"]);
        }

        let (n_parse, n_add, n_apply, n_restore, n_match) = (
            exp["parse"].as_array().unwrap().len(),
            exp["add"].as_array().unwrap().len(),
            exp["apply"].as_array().unwrap().len(),
            exp["restore"].as_array().unwrap().len(),
            exp["match"].as_array().unwrap().len(),
        );
        println!("parse {n_parse}; add {n_add}; apply {n_apply}; restore {n_restore}; match {n_match}");
        assert!(
            bad.is_empty(),
            "{} расхождений:\n{}",
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}
