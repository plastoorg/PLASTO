//! SNI-прокси: подмена IP через файл hosts.
//!
//! Перенос `hosts.js` без изменения поведения. Сеть вынесена в `probe`;
//! здесь разбор каталога, правка файла и выбор прокси.
//!
//! Зачем это отдельно от zapret. Провайдер режет по SNI: имя хоста едет в
//! ClientHello открытым текстом, и обход прячет именно его. Но сам сервис
//! (OpenAI, Anthropic, Google Labs) смотрит на IP и отвечает «недоступно в
//! вашей стране». Пакеты доходят, TLS встаёт — zapret тут ни при чём.
//!
//! SNI-прокси решает вторую задачу: читает SNI из ClientHello и пересылает
//! поток как есть. Ключей у него нет, расшифровать он ничего не может —
//! трафик остаётся end-to-end, меняется только IP, который видит сервис.
//!
//! Соединение ДО прокси по-прежнему несёт настоящий SNI, и провайдер его
//! режет. Поэтому hosts НЕ заменяет обход, а дополняет его.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex_lite::Regex;

pub const SOURCE_URL: &str = "https://raw.githubusercontent.com/ImMALWARE/dns.malw.link/master/hosts";

pub const MARK_BEGIN: &str = "# >>> PLASTO SNI — блок управляется приложением, правки внутри перезапишутся";
pub const MARK_END: &str = "# <<< PLASTO SNI";

/// Канарейка: единственный надёжный способ узнать, что Windows вообще читает
/// hosts. Зона .invalid зарезервирована RFC 2606 и публично не резолвится,
/// значит ответ на это имя может прийти ТОЛЬКО из файла. Нужна потому, что
/// «записали успешно» и «работает» — разные вещи.
pub const CANARY_HOST: &str = "plasto-hosts-check.invalid";
pub const CANARY_IP: &str = "127.0.0.1";

/// Системный hosts. SystemRoot читаем из окружения — Windows не всегда на C:.
pub fn hosts_path() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&root).join("System32").join("drivers").join("etc").join("hosts")
}

/// Копию держим в профиле приложения, а не рядом с hosts: каталог
/// `drivers\etc` под присмотром антивирусов, лишние файлы там ни к чему.
pub fn backup_path(user_data_dir: &Path) -> PathBuf {
    user_data_dir.join("hosts.backup")
}

/* ---------------------------------------------------------------------
   Совместимость с JS-строками. `trim()` и `\s` в JS считают пробелом и
   BOM (U+FEFF); в Rust его нет среди пробельных. Файл hosts с BOM — не
   выдумка (Блокнот), поэтому расхождение здесь стоило бы целой строки.
   --------------------------------------------------------------------- */

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `text.split(/\r?\n/)`: последняя пустая строка сохраняется (в отличие от
/// `str::lines`), одинокий `\r` не разделитель.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

fn tokens(t: &str) -> Vec<&str> {
    t.split(is_space).filter(|x| !x.is_empty()).collect()
}

/// `(\r\n){3,}` → пустая строка между блоками, затем trim и один перевод.
fn tidy(lines: &[String]) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\r\n){3,}").unwrap());
    let joined = lines.join("\r\n");
    let squeezed = RE.replace_all(&joined, "\r\n\r\n");
    format!("{}\r\n", js_trim(&squeezed))
}

/* =====================================================================
   Разбор
   ===================================================================== */

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub ip: String,
    pub host: String,
}

impl Entry {
    pub fn new(ip: &str, host: &str) -> Self {
        Entry { ip: ip.into(), host: host.into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub unblock: Vec<Entry>,
    pub blocklist: Vec<Entry>,
    pub updated: Option<String>,
}

static RE_IPV4: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$").unwrap());
static RE_UPDATED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Последнее обновление:\s*(.+)$").unwrap());

/// Разбирает hosts на две принципиально разные части.
///
/// unblock   — `193.233.112.68 chatgpt.com`: адрес SNI-прокси.
/// blocklist — `0.0.0.0 <домен>`: реклама, скам, IP-логгеры. Это отдельная
///             функция файла, поэтому и в интерфейсе она отдельным тумблером.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();

    for line in split_lines(text) {
        let t = js_trim(line);
        if t.is_empty() {
            continue;
        }
        if t.starts_with('#') {
            if let Some(m) = RE_UPDATED.captures(t) {
                out.updated = Some(js_trim(&m[1]).to_string());
            }
            continue;
        }
        let toks = tokens(t);
        let (ip, rest) = (toks[0], &toks[1..]);
        if !RE_IPV4.is_match(ip) || rest.is_empty() {
            continue;
        }
        for host in rest {
            if host.starts_with('#') {
                break;
            }
            let e = Entry { ip: ip.into(), host: host.to_lowercase() };
            if ip == "0.0.0.0" {
                out.blocklist.push(e);
            } else {
                out.unblock.push(e);
            }
        }
    }
    out
}

/// Имена, которые НЕ отправляем через SNI-прокси: файлы, вложения и статика.
///
/// Гео-блок стоит на API и самом чате; хранилище отдаёт подписанные ссылки и
/// страну не проверяет. Измерено 2026-09-29: 5 МБ напрямую — стабильно
/// 3.7 МБ/с, через общий прокси — от 2 до 0.24 МБ/с (до 20 с). Провайдер
/// режет их по SNI — это по-прежнему закрывает zapret, hosts здесь вредит.
pub fn is_direct_host(host: &str) -> bool {
    static RES: LazyLock<[Regex; 3]> = LazyLock::new(|| {
        [
            Regex::new(r"(^|\.)oaiusercontent\.com$").unwrap(),
            Regex::new(r"(^|\.)oaistatic\.com$").unwrap(),
            Regex::new(r"^cdn\.(platform\.)?openai\.com$").unwrap(),
        ]
    });
    let h = host.to_lowercase();
    RES.iter().any(|re| re.is_match(&h))
}

/// Записи прокси, относящиеся к указанным доменам.
///
/// hosts не знает подстановок: `openai.com` не накрывает `api.openai.com`,
/// каждое имя выписывается отдельно. Поэтому сопоставляем по суффиксу сами.
pub fn entries_for_domains(unblock: &[Entry], domains: &[String]) -> Vec<Entry> {
    let want: Vec<String> = domains.iter().map(|d| d.to_lowercase()).collect();
    unblock
        .iter()
        .filter(|e| {
            !is_direct_host(&e.host)
                && want.iter().any(|d| e.host == *d || e.host.ends_with(&format!(".{d}")))
        })
        .cloned()
        .collect()
}

/// Какие из наших сервисов вообще покрыты источником: id → число записей.
pub fn coverage(unblock: &[Entry], services: &[(String, Vec<String>)]) -> Vec<(String, usize)> {
    services.iter().map(|(id, d)| (id.clone(), entries_for_domains(unblock, d).len())).collect()
}

/* =====================================================================
   Правка файла
   ===================================================================== */

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Split {
    pub before: Vec<String>,
    pub block: Vec<String>,
    pub after: Vec<String>,
    pub had_block: bool,
}

/// Делит файл на «чужое» и «наш блок».
pub fn split(text: &str) -> Split {
    let mut s = Split::default();
    let mut at = 0;
    for line in split_lines(text) {
        if at == 0 && js_trim(line) == MARK_BEGIN {
            at = 1;
            continue;
        }
        if at == 1 && js_trim(line) == MARK_END {
            at = 2;
            continue;
        }
        match at {
            0 => s.before.push(line.into()),
            1 => s.block.push(line.into()),
            _ => s.after.push(line.into()),
        }
    }
    s.had_block = at == 2;
    s
}

pub fn read(file: &Path) -> String {
    fs::read(file).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Current {
    pub had_block: bool,
    pub unblock: Vec<Entry>,
    pub blocklist: Vec<Entry>,
    pub proxies: usize,
    pub blocked: usize,
}

/// Что сейчас лежит в нашем блоке. Канарейку не считаем — она служебная.
pub fn current(file: &Path) -> Current {
    let sp = split(&read(file));
    let parsed = parse(&sp.block.join("\n"));
    let unblock: Vec<Entry> = parsed.unblock.into_iter().filter(|e| e.host != CANARY_HOST).collect();
    Current {
        had_block: sp.had_block,
        proxies: unblock.len(),
        blocked: parsed.blocklist.len(),
        unblock,
        blocklist: parsed.blocklist,
    }
}

/// Копия исходного hosts — один раз, до первой правки. Повторно не
/// перезаписываем: иначе «восстановить» вернуло бы уже наш файл с блоком.
pub fn backup(user_data_dir: &Path, file: &Path) -> io::Result<(PathBuf, bool)> {
    let dst = backup_path(user_data_dir);
    if dst.exists() {
        return Ok((dst, false));
    }
    if let Some(dir) = dst.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&dst, read(file))?;
    Ok((dst, true))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub file: PathBuf,
    pub proxies: usize,
    pub blocked: usize,
    pub shadowed: usize,
    pub changed: bool,
}

/// Пишет блок в hosts.
pub fn apply(
    entries: &[Entry],
    blocklist: &[Entry],
    user_data_dir: Option<&Path>,
    file: &Path,
) -> io::Result<Applied> {
    if let Some(dir) = user_data_dir {
        backup(dir, file)?;
    }

    let parts = split(&read(file));
    let fmt = |e: &Entry| format!("{} {}", e.ip, e.host);
    let mut body: Vec<String> = Vec::new();

    if !entries.is_empty() || !blocklist.is_empty() {
        body.extend([
            MARK_BEGIN.to_string(),
            format!("# Источник: {SOURCE_URL}"),
            "# Записи прокси подменяют IP, который видит сервис. Обход они не заменяют:".into(),
            "# провайдер режет по SNI, а до прокси пакет идёт с настоящим SNI.".into(),
            String::new(),
            "# Служебная строка: по ней приложение проверяет, что Windows читает".into(),
            "# этот файл. Зона .invalid не существует в DNS, поэтому ответ на неё".into(),
            "# возможен только отсюда. Удалять не нужно.".into(),
            format!("{CANARY_IP} {CANARY_HOST}"),
        ]);
        if !entries.is_empty() {
            body.push(String::new());
            body.push(format!("# SNI-прокси — {} имён", entries.len()));
            body.extend(entries.iter().map(fmt));
        }
        if !blocklist.is_empty() {
            body.push(String::new());
            body.push(format!("# Блоклист — {} имён (реклама, скам, IP-логгеры)", blocklist.len()));
            body.extend(blocklist.iter().map(fmt));
        }
        body.push(MARK_END.to_string());
    }

    // body.slice(1, -1): без маркеров; у пустого body это пустой срез.
    let inner = if body.len() >= 2 { body[1..body.len() - 1].join("\n") } else { String::new() };
    let same = parts.had_block && js_trim(&parts.block.join("\n")) == js_trim(&inner);

    // Наш блок идёт ПЕРВЫМ, и это не косметика. Windows берёт первое
    // совпадение по имени, а у пользователя вполне может лежать старая копия
    // того же каталога с протухшими адресами — тогда наш блок в конце файла
    // просто не работает, и понять это по виду файла нельзя.
    let outside: Vec<String> = parts.before.iter().chain(parts.after.iter()).cloned().collect();
    let all: Vec<String> = body.iter().cloned().chain(outside.iter().cloned()).collect();
    write_hosts(file, &tidy(&all))?;

    let ours: Vec<Entry> = entries.iter().chain(blocklist.iter()).cloned().collect();
    Ok(Applied {
        file: file.to_path_buf(),
        proxies: entries.len(),
        blocked: blocklist.len(),
        // Сколько наших имён продублировано ниже чужими строками: они теперь
        // перекрыты и не действуют, но пользователю стоит про них знать.
        shadowed: conflicts(&outside, &ours).len(),
        changed: !same,
    })
}

/// Имена вне нашего блока, конфликтующие с теми, что мы прописали
/// (в порядке первого появления).
pub fn conflicts(outside: &[String], entries: &[Entry]) -> Vec<String> {
    let ours: HashSet<&str> = entries.iter().map(|e| e.host.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();

    for line in outside {
        let t = js_trim(line);
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let toks = tokens(t);
        if !RE_IPV4.is_match(toks[0]) {
            continue;
        }
        for h in &toks[1..] {
            if h.starts_with('#') {
                break;
            }
            let host = h.to_lowercase();
            if ours.contains(host.as_str()) && seen.insert(host.clone()) {
                out.push(host);
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stale {
    pub hosts: Vec<String>,
    pub count: usize,
    /// Заголовок чужой копии каталога — признак, что это именно дамп, а не
    /// пара строк, добавленных руками.
    pub looks_like_catalog: bool,
}

/// Ищет чужую копию того же каталога ВНЕ нашего блока. Само по себе это
/// ничего не ломает (наш блок первый), но адреса там протухшие, и при откате
/// блока они оживут. Поэтому показываем и предлагаем убрать, а не чистим
/// молча: это строки пользователя.
pub fn find_stale_catalog(file: &Path) -> Stale {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^###\s*dns\.malw\.link").unwrap());
    let sp = split(&read(file));
    let outside: Vec<String> = sp.before.iter().chain(sp.after.iter()).cloned().collect();
    let hosts = conflicts(&outside, &current(file).unblock);
    Stale {
        count: hosts.len(),
        hosts,
        looks_like_catalog: outside.iter().any(|l| RE.is_match(js_trim(l))),
    }
}

/// Удаляет только конфликтующие строки вне блока. Остальное не трогает.
/// Возвращает число убранных строк.
pub fn drop_stale(file: &Path, managed_names: &[String]) -> io::Result<usize> {
    let parts = split(&read(file));
    // «Наши» — и прокси, и блоклист: иначе чужая копия каталога вычищалась бы
    // частично, а её строки 0.0.0.0 оставались дублями.
    let cur = current(file);
    let ours: HashSet<String> = cur.unblock.iter().chain(cur.blocklist.iter()).map(|e| e.host.clone()).collect();

    // Имена выключенных сервисов в блоке уже не лежат, но чужая копия
    // каталога их помнит — и, лишившись нашего перекрытия, оживает. Чистим и
    // по списку, которым приложение управляет вообще (по суффиксу).
    let managed: Vec<String> = managed_names.iter().map(|h| h.to_lowercase()).collect();
    let is_ours = |h: &str| ours.contains(h) || managed.iter().any(|d| h == d || h.ends_with(&format!(".{d}")));

    let strip = |arr: &[String]| -> Vec<String> {
        arr.iter()
            .filter(|line| {
                let t = js_trim(line);
                if t.is_empty() || t.starts_with('#') {
                    return true;
                }
                let toks = tokens(t);
                if !RE_IPV4.is_match(toks[0]) {
                    return true;
                }
                // Строку убираем, только если ВСЕ её имена — наши. Строку вида
                // «1.2.3.4 chatgpt.com my-own.host» трогать нельзя.
                let names: Vec<String> =
                    toks[1..].iter().filter(|h| !h.starts_with('#')).map(|h| h.to_lowercase()).collect();
                names.is_empty() || !names.iter().all(|n| is_ours(n))
            })
            .cloned()
            .collect()
    };

    let before = strip(&parts.before);
    let after = strip(&parts.after);
    let removed = (parts.before.len() - before.len()) + (parts.after.len() - after.len());
    if removed == 0 {
        return Ok(0);
    }

    let mut lines: Vec<String> = Vec::new();
    if parts.had_block {
        lines.push(MARK_BEGIN.into());
        lines.extend(parts.block.iter().cloned());
        lines.push(MARK_END.into());
    }
    lines.extend(before);
    lines.extend(after);
    write_hosts(file, &tidy(&lines))?;
    Ok(removed)
}

/// Убирает наш блок, остальное оставляет как есть. true — блок был.
pub fn clear(file: &Path) -> io::Result<bool> {
    let parts = split(&read(file));
    if !parts.had_block {
        return Ok(false);
    }
    let lines: Vec<String> = parts.before.into_iter().chain(parts.after).collect();
    write_hosts(file, &tidy(&lines))?;
    Ok(true)
}

/// Возвращает файл из резервной копии целиком.
pub fn restore(user_data_dir: &Path, file: &Path) -> Result<PathBuf, String> {
    let src = backup_path(user_data_dir);
    if !src.exists() {
        return Err("резервной копии нет".into());
    }
    let bytes = fs::read(&src).map_err(|e| e.to_string())?;
    write_hosts(file, &String::from_utf8_lossy(&bytes)).map_err(|e| e.to_string())?;
    Ok(src)
}

/// Запись строго НА МЕСТО, без временного файла и rename.
///
/// Напрашивается «атомарная» запись: во временный файл и rename поверх hosts.
/// Но move переносит файл ВМЕСТЕ с его ACL — файл из %TEMP% приезжает с
/// правами «только текущий пользователь». Служба Dnscache работает от
/// NetworkService, теряет доступ на чтение, и Windows молча перестаёт
/// учитывать hosts ЦЕЛИКОМ: не резолвятся даже чужие записи. Диагностируется
/// тем, что имя, существующее только в hosts, не резолвится вообще.
///
/// Запись в существующий файл открывает тот же файловый объект и усекает его —
/// дескриптор безопасности остаётся прежним. Атомарности нет, но на этот
/// случай есть резервная копия, а испорченные права чинятся только руками.
pub fn write_hosts(file: &Path, text: &str) -> io::Result<()> {
    // Атрибут «только чтение» ставят некоторые «оптимизаторы».
    if let Ok(m) = fs::metadata(file) {
        let mut p = m.permissions();
        p.set_readonly(false);
        let _ = fs::set_permissions(file, p);
    }
    match fs::OpenOptions::new().write(true).truncate(true).open(file) {
        Ok(mut f) => io::Write::write_all(&mut f, text.as_bytes()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::write(file, text),
        Err(e) => Err(e),
    }
}

/* =====================================================================
   Системные вызовы (только Windows)
   ===================================================================== */

#[cfg(windows)]
fn hidden(program: &str, args: &[&str]) -> io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(program).args(args).creation_flags(CREATE_NO_WINDOW).output()
}

#[cfg(not(windows))]
fn hidden(program: &str, args: &[&str]) -> io::Result<std::process::Output> {
    std::process::Command::new(program).args(args).output()
}

/// Права на чтение для всех — то, без чего Dnscache не увидит файл.
/// Возвращаем к наследуемым от каталога, как у соседних lmhosts.sam и protocol.
pub fn repair_acl(file: &Path) -> (bool, String) {
    match hidden("icacls", &[&file.to_string_lossy(), "/reset"]) {
        Ok(o) => (o.status.success(), crate::platform::console_text(&o.stdout).trim().to_string()),
        Err(e) => (false, e.to_string()),
    }
}

/// Читается ли файл службой DNS. Точной проверки ACL не сделать, поэтому
/// смотрим на реально сломавшийся признак: нет группы «Пользователи» с правом
/// чтения. S-1-5-32-545 — BUILTIN\Users в любой локализации.
pub fn acl_looks_broken(file: &Path) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)S-1-5-32-545|\(RX\)").unwrap());
    match hidden("icacls", &[&file.to_string_lossy()]) {
        Ok(o) if o.status.success() => {
            let text: String = o.stdout.iter().map(|&b| b as char).collect(); // latin1
            !RE.is_match(&text)
        }
        _ => false, // не смогли проверить — не пугаем
    }
}

/// Без сброса кэша Windows продолжит отдавать старые адреса.
pub fn flush_dns() -> bool {
    hidden("ipconfig", &["/flushdns"]).map(|o| o.status.success()).unwrap_or(false)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub ok: bool,
    pub addresses: Vec<String>,
    pub error: Option<String>,
}

/// Резолв через системный резолвер.
///
/// Именно getaddrinfo (как `dns.lookup` в Node), а не прямой запрос к DNS:
/// первый учитывает hosts, второй файл игнорирует. Перепутать их — значит
/// получить «всё хорошо» там, где ничего не работает.
pub fn lookup(host: &str, timeout: Duration) -> Lookup {
    use std::net::ToSocketAddrs;
    let (tx, rx) = std::sync::mpsc::channel();
    let name = format!("{host}:0");
    std::thread::spawn(move || {
        let _ = tx.send(name.to_socket_addrs().map(|it| {
            it.filter(|a| a.is_ipv4()).map(|a| a.ip().to_string()).collect::<Vec<_>>()
        }));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(a)) => Lookup { ok: true, addresses: a, error: None },
        Ok(Err(e)) => Lookup { ok: false, addresses: vec![], error: Some(e.to_string()) },
        Err(_) => Lookup { ok: false, addresses: vec![], error: Some("timeout".into()) },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verify {
    pub active: bool,
    pub reason: &'static str,
}

/// Действует ли файл прямо сейчас — по канарейке. `ignored` значит: файл
/// записан, но Windows его не читает (чаще всего Dnscache держит состояние,
/// снятое до починки прав, и лечится только перезагрузкой).
///
/// `resolver` подменяется в тестах: имя канарейки одно на всю систему, и если
/// PLASTO уже применён на этой машине, настоящий резолв ответит про системный
/// hosts, а не про проверяемый файл.
pub fn verify_hosts(file: &Path, resolver: impl Fn(&str) -> Lookup) -> Verify {
    if !read(file).contains(CANARY_HOST) {
        return Verify { active: false, reason: "absent" };
    }
    let r = resolver(CANARY_HOST);
    if r.ok && r.addresses.iter().any(|a| a == CANARY_IP) {
        Verify { active: true, reason: "ok" }
    } else {
        Verify { active: false, reason: "ignored" }
    }
}

/* =====================================================================
   Выбор прокси
   ===================================================================== */

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swap {
    pub from: String,
    pub to: String,
    pub hosts: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Substituted {
    pub entries: Vec<Entry>,
    pub swaps: Vec<Swap>,
    pub orphan: Vec<Entry>,
}

/// Подменяет мёртвый адрес каталога живым из того же каталога.
///
/// Каталог держит все google-имена на одном адресе; когда он умирает,
/// приложение остаётся без Google совсем. Между тем в этом же каталоге лежат
/// живые прокси, и часть из них google-имена обслуживает.
///
/// Каждое имя проверяем отдельно, и это не перестраховка: у прокси свои
/// списки, и один адрес спокойно обслуживает часть имён, рвя остальные.
/// Выбор по первому имени группы поставил бы заведомо нерабочий адрес
/// остальным.
///
/// Порядок `pool` важен: берём первого подошедшего, поэтому зарубежные адреса
/// идут первыми — только они снимают гео-блок.
///
/// `probe(ip, host)` подменяется в тестах: ходить в сеть ради логики выбора
/// незачем. Имена независимы и проверяются параллельно; кандидаты внутри
/// имени — по очереди, чтобы остановиться на первом подошедшем.
pub fn substitute(dropped: &[Entry], pool: &[String], probe: &(dyn Fn(&str, &str) -> bool + Sync)) -> Substituted {
    let found: Vec<(Entry, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = dropped
            .iter()
            .map(|e| {
                s.spawn(move || {
                    let to = pool.iter().find(|c| **c != e.ip && probe(c, &e.host)).cloned();
                    (e.clone(), to)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("поток проверки прокси упал")).collect()
    });

    let mut out = Substituted::default();
    let mut index: HashMap<String, usize> = HashMap::new();

    for (entry, to) in found {
        let Some(to) = to else {
            out.orphan.push(entry);
            continue;
        };
        out.entries.push(Entry { ip: to.clone(), host: entry.host.clone() });
        let key = format!("{}>{}", entry.ip, to);
        let i = *index.entry(key).or_insert_with(|| {
            out.swaps.push(Swap { from: entry.ip.clone(), to: to.clone(), hosts: vec![] });
            out.swaps.len() - 1
        });
        out.swaps[i].hosts.push(entry.host);
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ordered {
    pub order: Vec<String>,
    pub geo: Vec<(String, Option<String>)>,
}

/// Сортирует адреса так, чтобы зарубежные шли первыми.
///
/// Не косметика, а единственное, что отделяет «обошли провайдера» от «обошли
/// и провайдера, и блок по стране». Сервис смотрит на IP того, кто к нему
/// пришёл, то есть на прокси: через российский он видит Россию и отказывает
/// после логина. Адреса с неизвестной страной ставим между: хуже заведомо
/// зарубежного, лучше заведомо российского.
pub fn prefer_foreign(ips: &[String], country: &(dyn Fn(&str) -> Option<String> + Sync)) -> Ordered {
    let mut rank: Vec<(String, Option<String>, u8)> = std::thread::scope(|s| {
        let handles: Vec<_> = ips
            .iter()
            .map(|ip| {
                s.spawn(move || {
                    let c = country(ip);
                    let r = match c.as_deref() {
                        None => 1,
                        Some("RU") => 2,
                        Some(_) => 0,
                    };
                    (ip.clone(), c, r)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("поток геолокации упал")).collect()
    });
    rank.sort_by_key(|x| x.2); // стабильная, как Array.sort
    Ordered {
        order: rank.iter().map(|x| x.0.clone()).collect(),
        geo: rank.into_iter().map(|x| (x.0, x.1)).collect(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub entries: Vec<Entry>,
    pub swaps: Vec<Swap>,
    /// Имена без подходящего прокси: в hosts не пишутся, остаются на DNS.
    pub skipped: Vec<String>,
    /// Имена, оставленные на российском прокси за неимением зарубежного:
    /// провайдера он обходит, а блок по стране — нет.
    pub domestic: Vec<String>,
    pub geo: Vec<(String, Option<String>)>,
}

/// Что писать в hosts: каждое имя — на прокси, который его отдаёт, и по
/// возможности зарубежный.
///
/// Запись каталога годится как есть, если прокси отдаёт имя и стоит не в
/// России. Иначе ищем замену среди остальных адресов `pool`, российские в
/// замену не берём: ради них имя переносить незачем. Не нашли — российский
/// прокси, который имя всё же отдаёт, оставляем (провайдера он обходит), но
/// сообщаем; мёртвый или чужой — выкидываем: запись в hosts перебивает DNS.
///
/// `serves(ip, host)` и `country(ip)` подменяются в тестах. Страна
/// спрашивается один раз на адрес.
pub fn choose(
    wanted: &[Entry],
    pool: &[String],
    serves: &(dyn Fn(&str, &str) -> bool + Sync),
    country: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Choice {
    let mut ips: Vec<String> = Vec::new();
    for ip in pool.iter().chain(wanted.iter().map(|e| &e.ip)) {
        if !ips.contains(ip) {
            ips.push(ip.clone());
        }
    }
    let ordered = prefer_foreign(&ips, country);
    let geo: HashMap<&str, Option<&str>> = ordered.geo.iter().map(|(ip, c)| (ip.as_str(), c.as_deref())).collect();
    let domestic_ip = |ip: &str| geo.get(ip).copied().flatten() == Some("RU");
    let foreign: Vec<String> = ordered.order.iter().filter(|ip| !domestic_ip(ip)).cloned().collect();

    let served: Vec<bool> = std::thread::scope(|s| {
        let hs: Vec<_> = wanted.iter().map(|e| s.spawn(move || serves(&e.ip, &e.host))).collect();
        hs.into_iter().map(|h| h.join().expect("поток проверки прокси упал")).collect()
    });

    let mut out = Choice::default();
    let mut need: Vec<Entry> = Vec::new();
    for (e, ok) in wanted.iter().zip(&served) {
        if *ok && !domestic_ip(&e.ip) {
            out.entries.push(e.clone());
        } else {
            need.push(e.clone());
        }
    }
    let sub = substitute(&need, &foreign, serves);
    out.entries.extend(sub.entries);
    out.swaps = sub.swaps;
    for e in sub.orphan {
        let ok = wanted.iter().zip(&served).any(|(w, ok)| *ok && *w == e);
        if ok {
            out.domestic.push(e.host.clone());
            out.entries.push(e);
        } else {
            out.skipped.push(e.host);
        }
    }
    out.geo = ordered.geo;
    out
}

/// Записи для своего SNI-прокси: все имена выбранных сервисов на один адрес.
///
/// hosts не знает подстановок, поэтому одних доменов из списка сервисов мало:
/// `anthropic.com` не накроет `api.anthropic.com`. Поддомены берём из каталога —
/// его адреса тут не нужны, только имена, которые он уже собрал.
pub fn own_entries(ip: &str, domains: &[String], catalog: &[Entry]) -> Vec<Entry> {
    let mut seen: HashSet<String> = HashSet::new();
    domains
        .iter()
        .map(|d| d.to_lowercase())
        .chain(entries_for_domains(catalog, domains).into_iter().map(|e| e.host))
        .filter(|h| !is_direct_host(h) && seen.insert(h.clone()))
        .map(|h| Entry { ip: ip.into(), host: h })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("hosts");
        (d, f)
    }

    fn e(ip: &str, host: &str) -> Entry {
        Entry::new(ip, host)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// Кусок настоящего файла: шапка, прокси, блоклист.
    const SAMPLE: &str = "### dns.malw.link: hosts file\n# Последнее обновление: 29 августа 2026\n\n# OpenAI\n\
193.233.112.68 chatgpt.com\n193.233.112.68 auth.openai.com\n193.233.112.67 auth0.openai.com\n\n\
# Google\n193.233.112.68 gemini.google.com\n\n# Блок\n0.0.0.0 ads.example.com\n0.0.0.0 tracker.example.net\n";

    /* ---------------- разбор ---------------- */

    #[test]
    fn разбор_делит_на_прокси_и_блоклист() {
        let p = parse(SAMPLE);
        assert_eq!(p.unblock.len(), 4);
        assert_eq!(p.blocklist.len(), 2);
        assert!(p.unblock.iter().all(|x| x.ip != "0.0.0.0"));
    }

    #[test]
    fn разбор_вытаскивает_дату_обновления() {
        assert_eq!(parse(SAMPLE).updated.as_deref(), Some("29 августа 2026"));
        assert_eq!(parse("1.2.3.4 a.com\n").updated, None);
    }

    #[test]
    fn разбор_комментарии_и_мусор_не_попадают_в_записи() {
        let p = parse("# 1.2.3.4 hidden.com\nnot-an-ip host.com\n999 x.com\n1.2.3.4\n\n1.2.3.4 real.com\n");
        assert_eq!(p.unblock, vec![e("1.2.3.4", "real.com")]);
    }

    #[test]
    fn разбор_несколько_имён_в_строке_и_комментарий_в_конце() {
        let p = parse("1.2.3.4 A.com b.com # хвост c.com\n0.0.0.0 x.com y.com\n");
        assert_eq!(p.unblock, vec![e("1.2.3.4", "a.com"), e("1.2.3.4", "b.com")]);
        assert_eq!(p.blocklist.len(), 2);
    }

    #[test]
    fn разбор_переносит_crlf_и_bom_в_начале() {
        let p = parse("\u{FEFF}1.2.3.4 a.com\r\n5.6.7.8 b.com\r\n");
        assert_eq!(p.unblock.len(), 2, "BOM не должен съедать первую строку");
    }

    /* ---------------- домены ---------------- */

    #[test]
    fn суффикс_накрывает_поддомены() {
        let u = parse(SAMPLE).unblock;
        let got = entries_for_domains(&u, &s(&["openai.com"]));
        let hosts: Vec<&str> = got.iter().map(|x| x.host.as_str()).collect();
        assert_eq!(hosts, ["auth.openai.com", "auth0.openai.com"]);
    }

    #[test]
    fn чужой_домен_не_цепляется_по_вхождению() {
        let u = vec![e("1.1.1.1", "notopenai.com"), e("1.1.1.1", "openai.com.evil.ru"), e("1.1.1.1", "api.openai.com")];
        let got = entries_for_domains(&u, &s(&["openai.com"]));
        assert_eq!(got, vec![e("1.1.1.1", "api.openai.com")]);
    }

    #[test]
    fn хранилище_и_статика_идут_напрямую() {
        for h in ["files.oaiusercontent.com", "oaiusercontent.com", "cdn.oaistatic.com", "cdn.openai.com", "cdn.platform.openai.com", "CDN.OPENAI.COM"] {
            assert!(is_direct_host(h), "{h}");
        }
        for h in ["api.openai.com", "chatgpt.com", "cdn.example.com", "evil-oaistatic.com", "openai.com"] {
            assert!(!is_direct_host(h), "{h}");
        }
        let u = vec![e("1.1.1.1", "api.openai.com"), e("1.1.1.1", "files.oaiusercontent.com")];
        assert_eq!(entries_for_domains(&u, &s(&["openai.com", "oaiusercontent.com"])).len(), 1);
    }

    #[test]
    fn покрытие_считается_по_каждому_сервису() {
        let u = parse(SAMPLE).unblock;
        let cov = coverage(&u, &[("openai".into(), s(&["openai.com"])), ("none".into(), s(&["nothing.example"]))]);
        assert_eq!(cov, vec![("openai".to_string(), 2), ("none".to_string(), 0)]);
    }

    /* ---------------- правка файла ---------------- */

    #[test]
    fn чужие_строки_переживают_запись() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n::1 localhost\r\n# моя заметка\r\n10.0.0.5 nas.home\r\n").unwrap();
        apply(&[e("9.9.9.9", "api.openai.com")], &[], None, &f).unwrap();
        let t = read(&f);
        for keep in ["127.0.0.1 localhost", "::1 localhost", "# моя заметка", "10.0.0.5 nas.home"] {
            assert!(t.contains(keep), "потеряно: {keep}");
        }
        assert!(t.contains("9.9.9.9 api.openai.com"));
    }

    #[test]
    fn повторная_запись_не_плодит_блоки_и_сообщает_changed_false() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        let ent = [e("9.9.9.9", "a.com")];
        let first = apply(&ent, &[], None, &f).unwrap();
        let second = apply(&ent, &[], None, &f).unwrap();
        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(read(&f).matches(MARK_BEGIN).count(), 1);
    }

    #[test]
    fn блоклист_пишется_только_когда_его_просят() {
        let (_d, f) = tmp();
        fs::write(&f, "").unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert!(!read(&f).contains("0.0.0.0"));
        let r = apply(&[e("9.9.9.9", "a.com")], &[e("0.0.0.0", "ads.com")], None, &f).unwrap();
        assert!(read(&f).contains("0.0.0.0 ads.com"));
        assert_eq!((r.proxies, r.blocked), (1, 1));
    }

    #[test]
    fn current_читает_обратно_то_что_записали_без_канарейки() {
        let (_d, f) = tmp();
        fs::write(&f, "").unwrap();
        apply(&[e("9.9.9.9", "a.com"), e("8.8.8.8", "b.com")], &[e("0.0.0.0", "x.com")], None, &f).unwrap();
        let c = current(&f);
        assert!(c.had_block);
        assert_eq!((c.proxies, c.blocked), (2, 1));
        assert!(c.unblock.iter().all(|x| x.host != CANARY_HOST), "канарейка служебная");
    }

    #[test]
    fn clear_убирает_блок_и_оставляет_остальное() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert!(clear(&f).unwrap());
        let t = read(&f);
        assert!(t.contains("127.0.0.1 localhost") && !t.contains(MARK_BEGIN) && !t.contains("a.com"));
        assert!(!clear(&f).unwrap(), "второй раз убирать нечего");
    }

    #[test]
    fn наш_блок_оказывается_выше_чужих_строк_и_позиция_стабильна() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n1.1.1.1 a.com\r\n").unwrap();
        let ent = [e("9.9.9.9", "a.com")];
        apply(&ent, &[], None, &f).unwrap();
        let t = read(&f);
        assert!(t.find(MARK_BEGIN).unwrap() < t.find("1.1.1.1 a.com").unwrap(), "блок должен идти первым");
        apply(&ent, &[], None, &f).unwrap();
        assert_eq!(read(&f), t, "повторная запись не двигает блок");
    }

    #[test]
    fn перекрытые_дубли_считаются() {
        let (_d, f) = tmp();
        fs::write(&f, "1.1.1.1 a.com\r\n2.2.2.2 b.com c.com\r\n").unwrap();
        let r = apply(&[e("9.9.9.9", "a.com"), e("9.9.9.9", "c.com"), e("9.9.9.9", "z.com")], &[], None, &f).unwrap();
        assert_eq!(r.shadowed, 2);
    }

    #[test]
    fn conflicts_не_путает_похожие_имена_и_не_дублирует() {
        let outside = s(&["1.1.1.1 a.com a.com", "2.2.2.2 xa.com a.com.ru", "# 3.3.3.3 a.com", "не ip a.com"]);
        assert_eq!(conflicts(&outside, &[e("9.9.9.9", "a.com")]), vec!["a.com"]);
    }

    #[test]
    fn запись_сохраняет_тот_же_файловый_объект() {
        // Через жёсткую ссылку: если бы запись шла через temp+rename, ссылка
        // осталась бы на старом содержимом, а права (ACL) уехали бы вместе с файлом.
        let (d, f) = tmp();
        fs::write(&f, "old\r\n").unwrap();
        let link = d.path().join("link");
        fs::hard_link(&f, &link).unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert!(fs::read_to_string(&link).unwrap().contains("9.9.9.9 a.com"));
    }

    #[test]
    fn запись_в_файл_только_для_чтения_проходит() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        let mut p = fs::metadata(&f).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&f, p).unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert!(read(&f).contains("9.9.9.9 a.com"));
    }

    #[test]
    fn пустой_набор_не_оставляет_маркеров() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        apply(&[], &[], None, &f).unwrap();
        let t = read(&f);
        assert!(!t.contains("PLASTO") && t.contains("127.0.0.1 localhost"));
    }

    /* ---------------- резервная копия ---------------- */

    #[test]
    fn копия_снимается_до_первой_правки_и_не_перезаписывается() {
        let (d, f) = tmp();
        fs::write(&f, "ORIGINAL\r\n").unwrap();
        let ud = d.path().join("profile");
        apply(&[e("9.9.9.9", "a.com")], &[], Some(&ud), &f).unwrap();
        apply(&[e("8.8.8.8", "b.com")], &[], Some(&ud), &f).unwrap();
        assert_eq!(fs::read_to_string(backup_path(&ud)).unwrap(), "ORIGINAL\r\n");
    }

    #[test]
    fn restore_возвращает_исходный_файл_а_без_копии_ругается() {
        let (d, f) = tmp();
        fs::write(&f, "ORIGINAL\r\n").unwrap();
        let ud = d.path().join("profile");
        assert_eq!(restore(&ud, &f).unwrap_err(), "резервной копии нет");
        apply(&[e("9.9.9.9", "a.com")], &[], Some(&ud), &f).unwrap();
        restore(&ud, &f).unwrap();
        assert_eq!(read(&f), "ORIGINAL\r\n");
    }

    /* ---------------- канарейка ---------------- */

    #[test]
    fn в_блок_попадает_служебная_строка() {
        let (_d, f) = tmp();
        fs::write(&f, "").unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert!(read(&f).contains(&format!("{CANARY_IP} {CANARY_HOST}")));
    }

    #[test]
    fn пустой_блок_канарейку_не_оставляет() {
        let (_d, f) = tmp();
        fs::write(&f, "").unwrap();
        apply(&[], &[], None, &f).unwrap();
        assert!(!read(&f).contains(CANARY_HOST));
    }

    #[test]
    fn верификация_по_канарейке_три_исхода() {
        let (_d, f) = tmp();
        fs::write(&f, "").unwrap();
        let ok = |_: &str| Lookup { ok: true, addresses: vec![CANARY_IP.into()], error: None };
        let bad = |_: &str| Lookup { ok: false, addresses: vec![], error: Some("ENOTFOUND".into()) };

        assert_eq!(verify_hosts(&f, ok).reason, "absent", "строки в файле нет — резолвить нечего");
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        assert_eq!(verify_hosts(&f, ok), Verify { active: true, reason: "ok" });
        assert_eq!(verify_hosts(&f, bad), Verify { active: false, reason: "ignored" });
    }

    /* ---------------- устаревшая копия ---------------- */

    fn stale_file(f: &Path) {
        fs::write(
            f,
            "### dns.malw.link: hosts file\r\n1.1.1.1 chatgpt.com\r\n1.1.1.1 old.openai.com\r\n0.0.0.0 ads.example.com\r\n127.0.0.1 localhost\r\n",
        )
        .unwrap();
    }

    #[test]
    fn находит_конфликтующие_имена_вне_блока_и_признак_каталога() {
        let (_d, f) = tmp();
        stale_file(&f);
        apply(&[e("9.9.9.9", "chatgpt.com")], &[e("0.0.0.0", "ads.example.com")], None, &f).unwrap();
        let st = find_stale_catalog(&f);
        // Как в JS: сверяются только прокси-записи, блоклист (0.0.0.0) не считается,
        // хотя drop_stale его чистит. Поведение сохранено дословно — см. отчёт о порте.
        assert_eq!(st.hosts, vec!["chatgpt.com"]);
        assert_eq!(st.count, 1);
        assert!(st.looks_like_catalog);
    }

    #[test]
    fn чистый_файл_ничего_не_находит() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        apply(&[e("9.9.9.9", "chatgpt.com")], &[], None, &f).unwrap();
        let st = find_stale_catalog(&f);
        assert_eq!((st.count, st.looks_like_catalog), (0, false));
    }

    #[test]
    fn drop_stale_убирает_дубли_и_по_списку_управляемых_имён() {
        let (_d, f) = tmp();
        stale_file(&f);
        apply(&[e("9.9.9.9", "chatgpt.com")], &[e("0.0.0.0", "ads.example.com")], None, &f).unwrap();
        // old.openai.com в блоке нет, но openai.com приложение «ведёт»
        let removed = drop_stale(&f, &s(&["openai.com"])).unwrap();
        assert_eq!(removed, 3, "chatgpt.com, ads.example.com и old.openai.com");
        let t = read(&f);
        assert!(t.contains("127.0.0.1 localhost"), "чужое не тронуто");
        assert!(!t.contains("1.1.1.1") && !t.contains("0.0.0.0 ads.example.com\r\n127"));
        assert!(t.contains(MARK_BEGIN), "наш блок остался");
    }

    #[test]
    fn drop_stale_не_трогает_строку_со_смешанными_именами() {
        let (_d, f) = tmp();
        fs::write(&f, "1.2.3.4 chatgpt.com my-own.host\r\n").unwrap();
        apply(&[e("9.9.9.9", "chatgpt.com")], &[], None, &f).unwrap();
        assert_eq!(drop_stale(&f, &[]).unwrap(), 0);
        assert!(read(&f).contains("1.2.3.4 chatgpt.com my-own.host"));
    }

    #[test]
    fn drop_stale_когда_убирать_нечего_файл_не_переписывается() {
        let (_d, f) = tmp();
        fs::write(&f, "127.0.0.1 localhost\r\n").unwrap();
        apply(&[e("9.9.9.9", "a.com")], &[], None, &f).unwrap();
        let before = read(&f);
        assert_eq!(drop_stale(&f, &[]).unwrap(), 0);
        assert_eq!(read(&f), before);
    }

    /* ---------------- выбор прокси ---------------- */

    #[test]
    fn подмена_каждое_имя_проверяется_отдельно() {
        // у 103.* работает gemini, но рвётся flow — один адрес на всю группу нельзя
        let dropped = [e("6.6.6.6", "gemini.google.com"), e("6.6.6.6", "flow.google.com"), e("6.6.6.6", "labs.google")];
        let pool = s(&["103.27.157.38", "193.233.112.68"]);
        let probe = |ip: &str, host: &str| match (ip, host) {
            ("103.27.157.38", "gemini.google.com") => true,
            ("193.233.112.68", _) => host != "labs.google",
            _ => false,
        };
        let r = substitute(&dropped, &pool, &probe);
        assert_eq!(
            r.entries,
            vec![e("103.27.157.38", "gemini.google.com"), e("193.233.112.68", "flow.google.com")]
        );
        assert_eq!(r.orphan, vec![e("6.6.6.6", "labs.google")]);
        assert_eq!(r.swaps.len(), 2);
        assert_eq!(r.swaps[0], Swap { from: "6.6.6.6".into(), to: "103.27.157.38".into(), hosts: s(&["gemini.google.com"]) });
    }

    #[test]
    fn подмена_берёт_первого_подошедшего_и_не_подставляет_тот_же_адрес() {
        let dropped = [e("1.1.1.1", "a.com"), e("1.1.1.1", "b.com")];
        let pool = s(&["1.1.1.1", "2.2.2.2", "3.3.3.3"]);
        let calls = Mutex::new(Vec::<String>::new());
        let probe = |ip: &str, _h: &str| {
            calls.lock().unwrap().push(ip.to_string());
            true
        };
        let r = substitute(&dropped, &pool, &probe);
        assert!(r.entries.iter().all(|x| x.ip == "2.2.2.2"));
        assert_eq!(r.swaps.len(), 1, "оба имени уехали на один адрес — одна запись о подмене");
        assert_eq!(r.swaps[0].hosts, s(&["a.com", "b.com"]));
        assert!(!calls.lock().unwrap().contains(&"1.1.1.1".to_string()), "мёртвый адрес не проверяется");
    }

    #[test]
    fn зарубежные_адреса_идут_первыми_неизвестные_посередине() {
        let ips = s(&["ru1", "unk", "de1", "ru2", "us1"]);
        let country = |ip: &str| match ip {
            "ru1" | "ru2" => Some("RU".to_string()),
            "unk" => None,
            "de1" => Some("DE".into()),
            _ => Some("US".into()),
        };
        let r = prefer_foreign(&ips, &country);
        assert_eq!(r.order, ["de1", "us1", "unk", "ru1", "ru2"], "порядок внутри группы стабилен");
        assert_eq!(r.geo[0], ("de1".to_string(), Some("DE".to_string())));
    }

    fn geo_ru_de(ip: &str) -> Option<String> {
        match ip {
            "ru1" | "ru2" => Some("RU".into()),
            "unk" => None,
            _ => Some("DE".into()),
        }
    }

    #[test]
    fn выбор_российский_прокси_меняется_на_зарубежный() {
        let wanted = [e("ru1", "claude.ai"), e("de1", "chatgpt.com")];
        let pool = s(&["ru1", "de1", "de2"]);
        let r = choose(&wanted, &pool, &|_ip: &str, _h: &str| true, &geo_ru_de);
        assert!(r.entries.contains(&e("de1", "chatgpt.com")), "зарубежная запись каталога остаётся как есть");
        assert!(r.entries.contains(&e("de1", "claude.ai")), "имя уехало с российского адреса");
        assert_eq!(r.swaps, vec![Swap { from: "ru1".into(), to: "de1".into(), hosts: s(&["claude.ai"]) }]);
        assert!(r.domestic.is_empty() && r.skipped.is_empty());
    }

    #[test]
    fn выбор_российский_без_замены_остаётся_и_помечается() {
        let wanted = [e("ru1", "claude.ai")];
        let pool = s(&["ru1", "ru2", "de1"]);
        // de1 claude.ai не отдаёт, ru2 отдаёт, но российский в замену не идёт
        let serves = |ip: &str, _h: &str| ip != "de1";
        let r = choose(&wanted, &pool, &serves, &geo_ru_de);
        assert_eq!(r.entries, vec![e("ru1", "claude.ai")]);
        assert_eq!(r.domestic, s(&["claude.ai"]));
        assert!(r.swaps.is_empty());
    }

    #[test]
    fn выбор_мёртвый_без_замены_выкидывается_неизвестная_страна_не_мешает() {
        let wanted = [e("dead", "a.com"), e("unk", "b.com")];
        let pool = s(&["dead", "unk"]);
        let serves = |ip: &str, h: &str| ip == "unk" && h == "b.com";
        let r = choose(&wanted, &pool, &serves, &|ip: &str| if ip == "unk" { None } else { Some("NL".into()) });
        assert_eq!(r.entries, vec![e("unk", "b.com")], "адрес с неизвестной страной берётся");
        assert_eq!(r.skipped, s(&["a.com"]));
    }

    #[test]
    fn свой_прокси_берёт_домены_сервиса_и_поддомены_из_каталога() {
        let catalog = [e("9.9.9.9", "api.anthropic.com"), e("9.9.9.9", "claude.ai"), e("9.9.9.9", "chatgpt.com"), e("9.9.9.9", "cdn.oaistatic.com")];
        let r = own_entries("203.0.113.10", &s(&["claude.ai", "Anthropic.com", "oaistatic.com"]), &catalog);
        assert_eq!(r, vec![e("203.0.113.10", "claude.ai"), e("203.0.113.10", "anthropic.com"), e("203.0.113.10", "api.anthropic.com")]);
    }
}

/// Сравнение с JS-эталоном: полная цепочка правок hosts на случайных файлах.
///
///   node scripts/gen-hosts-corpus.js %TEMP%\plasto-hosts-corpus
///   set PLASTO_CORPUS_HOSTS=%TEMP%\plasto-hosts-corpus
///   cargo test -- --ignored сравнение_с_js_hosts
///
/// Живёт рядом с портом, пока hosts.js не удалён.
#[cfg(test)]
mod differential {
    use super::*;
    use serde_json::{json, Value};

    fn ents(v: &[Entry]) -> Value {
        Value::Array(v.iter().map(|e| json!({ "ip": e.ip, "host": e.host })).collect())
    }

    fn strs(v: &[String]) -> Value {
        json!(v)
    }

    fn from_json(v: &Value) -> Vec<Entry> {
        v.as_array().unwrap().iter().map(|e| Entry::new(e["ip"].as_str().unwrap(), e["host"].as_str().unwrap())).collect()
    }

    fn text_of(f: &Path) -> String {
        String::from_utf8_lossy(&fs::read(f).unwrap()).into_owned()
    }

    #[test]
    #[ignore = "нужен корпус: PLASTO_CORPUS_HOSTS (scripts/gen-hosts-corpus.js)"]
    fn сравнение_с_js_hosts() {
        let dir = PathBuf::from(std::env::var("PLASTO_CORPUS_HOSTS").expect("задайте PLASTO_CORPUS_HOSTS"));
        let exp: Vec<Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("hosts-expected.json")).unwrap()).unwrap();
        assert!(!exp.is_empty());

        let work = tempfile::tempdir().unwrap();
        let mut bad: Vec<String> = Vec::new();
        let mut cmp = |i: usize, what: &str, got: Value, want: &Value| {
            if &got != want {
                bad.push(format!("#{i} {what}\n   js:   {want}\n   rust: {got}"));
            }
        };

        for (i, r) in exp.iter().enumerate() {
            let initial = r["initial"].as_str().unwrap();
            let entries = from_json(&r["entries"]);
            let blocklist = from_json(&r["blocklist"]);
            let managed: Vec<String> = r["managed"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().into()).collect();

            // чистые функции
            let p = parse(initial);
            cmp(i, "parse", json!({ "unblock": ents(&p.unblock), "blocklist": ents(&p.blocklist), "updated": p.updated }), &r["parse"]);
            let sp = split(initial);
            cmp(i, "split", json!({ "before": strs(&sp.before), "block": strs(&sp.block), "after": strs(&sp.after), "hadBlock": sp.had_block }), &r["split"]);
            cmp(i, "entriesForDomains", ents(&entries_for_domains(&p.unblock, &managed)), &r["forDomains"]);

            // цепочка над файлом
            let f = work.path().join(format!("h{i}"));
            fs::write(&f, initial).unwrap();

            let a = apply(&entries, &blocklist, None, &f).unwrap();
            cmp(i, "apply", json!({ "proxies": a.proxies, "blocked": a.blocked, "shadowed": a.shadowed, "changed": a.changed, "text": text_of(&f) }), &r["apply"]);

            let c = current(&f);
            cmp(i, "current", json!({ "hadBlock": c.had_block, "unblock": ents(&c.unblock), "blocklist": ents(&c.blocklist), "proxies": c.proxies, "blocked": c.blocked }), &r["current"]);

            let st = find_stale_catalog(&f);
            cmp(i, "findStaleCatalog", json!({ "hosts": strs(&st.hosts), "count": st.count, "looksLikeCatalog": st.looks_like_catalog }), &r["stale"]);

            let removed = drop_stale(&f, &managed).unwrap();
            cmp(i, "dropStale", json!({ "removed": removed, "text": text_of(&f) }), &r["drop"]);

            let changed = clear(&f).unwrap();
            cmp(i, "clear", json!({ "changed": changed, "text": text_of(&f) }), &r["clear"]);

            // отдельная ветка: apply пустого набора на исходном файле
            fs::write(&f, initial).unwrap();
            apply(&[], &[], None, &f).unwrap();
            cmp(i, "applyEmpty", json!(text_of(&f)), &r["applyEmpty"]);
        }

        println!("сценариев: {}", exp.len());
        assert!(
            bad.is_empty(),
            "{} расхождений:\n{}",
            bad.len(),
            bad.iter().take(5).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}
