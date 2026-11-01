//! Сетевые проверки SNI-прокси.
//!
//! Перенос сетевой половины `hosts.js`. Всё блокирующее и на потоках — без
//! async-рантайма: проверок десятки, а не тысячи, и тянуть ради них tokio в
//! бинарь, который мы режем по размеру, незачем.
//!
//! TLS — системный (SChannel через `native-tls`): свой набор корневых
//! сертификатов не везём. Отличие от Node в том, что доверие определяет
//! хранилище Windows, а не встроенный в Node список Mozilla; для вопроса
//! «отдаёт ли прокси настоящий сертификат сервиса» это одно и то же.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use native_tls::{TlsConnector, TlsStream};
use regex_lite::Regex;

use crate::hosts::Entry;

const FETCH_LIMIT: usize = 8 * 1024 * 1024; // файл ~750 КБ; запас на рост, но не безлимит

/* ---------------------------------------------------------------------
   TCP
   --------------------------------------------------------------------- */

/// Отвечает ли адрес на порту.
///
/// Голый TCP, без TLS: нужен факт «до хоста доходит SYN». SNI в SYN нет,
/// поэтому DPI тут ни при чём — молчание означает, что адрес мёртв или
/// заблокирован по IP, и никакой обход этого не лечит.
pub fn alive(ip: &str, port: u16, timeout: Duration) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(addr) => TcpStream::connect_timeout(&SocketAddr::new(addr, port), timeout).is_ok(),
        Err(_) => false,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Alive {
    pub entries: Vec<Entry>,
    pub dead: Vec<String>,
    pub dropped: Vec<Entry>,
}

/// Отсеивает записи, чей прокси не отвечает.
///
/// Запись в hosts перебивает DNS — она читается раньше резолвера. Значит
/// прописать мёртвый адрес ХУЖЕ, чем не прописывать ничего: без записи
/// сработал бы smart-DNS со своими живыми прокси для тех же имён.
///
/// Адреса в каталоге повторяются, поэтому проверяем уникальные и параллельно —
/// иначе на 200 записях это минуты.
pub fn filter_alive(entries: &[Entry], port: u16, timeout: Duration) -> Alive {
    let mut ips: Vec<&str> = Vec::new();
    for e in entries {
        if !ips.contains(&e.ip.as_str()) {
            ips.push(&e.ip);
        }
    }
    let ok: HashMap<&str, bool> = std::thread::scope(|s| {
        let hs: Vec<_> = ips.iter().map(|ip| s.spawn(move || (*ip, alive(ip, port, timeout)))).collect();
        hs.into_iter().map(|h| h.join().expect("поток проверки адреса упал")).collect()
    });

    Alive {
        dead: ips.iter().filter(|ip| !ok[**ip]).map(|s| s.to_string()).collect(),
        entries: entries.iter().filter(|e| ok[e.ip.as_str()]).cloned().collect(),
        dropped: entries.iter().filter(|e| !ok[e.ip.as_str()]).cloned().collect(),
    }
}

/* ---------------------------------------------------------------------
   TLS
   --------------------------------------------------------------------- */

/// TCP на `ip:443` и TLS-рукопожатие с SNI `host`. Имя в сертификате
/// проверяется штатно: «не его имя» (сброс, EPROTO, чужой сертификат)
/// выглядит одинаково — рукопожатие не состоялось.
fn tls_to(ip: &str, host: &str, timeout: Duration) -> Option<TlsStream<TcpStream>> {
    let addr = SocketAddr::new(ip.parse::<IpAddr>().ok()?, 443);
    let tcp = TcpStream::connect_timeout(&addr, timeout).ok()?;
    tcp.set_read_timeout(Some(timeout)).ok()?;
    tcp.set_write_timeout(Some(timeout)).ok()?;
    TlsConnector::new().ok()?.connect(host, tcp).ok()
}

/// Проксирует ли адрес именно это имя.
///
/// `alive()` отвечает лишь на «доходит ли SYN». Этого мало: у каждого
/// SNI-прокси свой список имён, и на чужое он рвёт соединение или отдаёт
/// собственный сертификат. Расшифровать поток прокси не может — ключей Google
/// у него нет, значит валидный сертификат на выходе и есть доказательство,
/// что он именно пересылает трафик, а не притворяется сервисом.
pub fn serves(ip: &str, host: &str, timeout: Duration) -> bool {
    tls_to(ip, host, timeout).is_some()
}

/* ---------------------------------------------------------------------
   Не «спалён» ли прокси у Google
   --------------------------------------------------------------------- */

static RE_SORRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)google\.[a-z.]+/sorry").unwrap());
static RE_GOOGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\.)(google\.com|googleapis\.com|withgoogle\.com|google)$").unwrap());

pub fn is_google_host(host: &str) -> bool {
    RE_GOOGLE.is_match(&host.to_lowercase())
}

/// Разбор начала ответа: капча — это 429 или редирект на google.com/sorry.
/// None — ответ не разобрать (оборван до статусной строки).
pub fn judge_head(head: &str) -> Option<bool> {
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let location = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    Some(status == 429 || RE_SORRY.is_match(location))
}

/// Один запрос `GET /` через прокси. Читаем только заголовки: у нормальной
/// страницы Gemini они огромные (Node спотыкался об это — HPE_HEADER_OVERFLOW),
/// но решает здесь статус и Location, а у капчи ответ маленький.
fn flagged_once(ip: &str, host: &str, timeout: Duration) -> Option<bool> {
    let mut s = tls_to(ip, host, timeout)?;
    let req = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130 Safari/537.36\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 1024 * 1024 {
                    break;
                }
            }
            Err(_) if buf.is_empty() => return None,
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    judge_head(head)
}

const FLAG_TTL: Duration = Duration::from_secs(5 * 60);
static FLAG_CACHE: LazyLock<Mutex<HashMap<String, (Instant, bool)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Some(true) — Google отдаёт этому прокси капчу; Some(false) — нет; None —
/// выяснить не удалось (сеть). Выходной IP у прокси один на все имена,
/// поэтому кэшируем по адресу, а не по имени.
pub fn flagged(ip: &str, host: &str, timeout: Duration) -> Option<bool> {
    if let Some((at, v)) = FLAG_CACHE.lock().unwrap().get(ip) {
        if at.elapsed() < FLAG_TTL {
            return Some(*v);
        }
    }
    let v = flagged_once(ip, host, timeout)?;
    FLAG_CACHE.lock().unwrap().insert(ip.to_string(), (Instant::now(), v));
    Some(v)
}

/// Прокси подходит для имени: отдаёт его и (для Google) не помечен.
pub fn serves_clean(ip: &str, host: &str, timeout: Duration) -> bool {
    if !serves(ip, host, timeout) {
        return false;
    }
    !is_google_host(host) || flagged(ip, host, Duration::from_secs(10)) != Some(true)
}

/// Адреса среди google-записей, которым Google сейчас отдаёт капчу.
pub fn flagged_ips(entries: &[Entry]) -> Vec<String> {
    let mut rep: Vec<(&str, &str)> = Vec::new();
    for e in entries {
        if is_google_host(&e.host) && !rep.iter().any(|(ip, _)| *ip == e.ip) {
            rep.push((&e.ip, &e.host));
        }
    }
    std::thread::scope(|s| {
        let hs: Vec<_> = rep
            .iter()
            .map(|(ip, host)| s.spawn(move || (*ip, flagged(ip, host, Duration::from_secs(10)))))
            .collect();
        hs.into_iter()
            .map(|h| h.join().expect("поток проверки капчи упал"))
            .filter(|(_, f)| *f == Some(true))
            .map(|(ip, _)| ip.to_string())
            .collect()
    })
}

/* ---------------------------------------------------------------------
   HTTP: каталог и геолокация
   ---------------------------------------------------------------------
   Здесь ходим по имени, а не по IP, поэтому берём готовый клиент: он сам
   разбирает chunked и редиректы.                                         */

fn agent(timeout: Duration) -> Result<ureq::Agent, String> {
    let tls = TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new().tls_connector(Arc::new(tls)).timeout(timeout).user_agent("PLASTO").build())
}

/// Скачивает каталог. Ошибку не глотаем: молчаливый пустой список хуже.
pub fn fetch_source(url: &str, timeout: Duration) -> Result<String, String> {
    let resp = match agent(timeout)?.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("источник ответил {code}")),
        Err(e) => {
            let m = e.to_string();
            return Err(if m.contains("timed out") { "источник не отвечает".into() } else { m });
        }
    };
    let mut body: Vec<u8> = Vec::new();
    resp.into_reader().take(FETCH_LIMIT as u64 + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() > FETCH_LIMIT {
        return Err("список неправдоподобно большой".into());
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Страна адреса — единственный способ узнать, снимет ли прокси гео-блок.
/// Ошибка сети здесь не повод падать: это справочная подпись.
///
/// Одна повторная попытка: запрос уходит сразу после десятков TLS-проверок, и
/// первый ответ регулярно теряется. Молчание здесь не безобидно — без страны
/// приложение промолчит и про гео, то есть недоскажет правду.
pub fn geo_of(ip: &str, timeout: Duration) -> Option<String> {
    geo_once(ip, timeout).or_else(|| {
        std::thread::sleep(Duration::from_millis(500));
        geo_once(ip, timeout)
    })
}

fn geo_once(ip: &str, timeout: Duration) -> Option<String> {
    let url = format!("https://ipinfo.io/{ip}/json");
    let body = agent(timeout).ok()?.get(&url).call().ok()?.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("country")?.as_str().map(str::to_string).filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn e(ip: &str, host: &str) -> Entry {
        Entry::new(ip, host)
    }

    const T: Duration = Duration::from_millis(1500);

    /* ---------------- TCP ---------------- */

    #[test]
    fn alive_отвечает_на_открытом_порту_и_молчит_на_закрытом() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(alive("127.0.0.1", port, T));
        drop(l);
        assert!(!alive("127.0.0.1", port, T), "порт закрыт");
    }

    #[test]
    fn alive_мусор_вместо_адреса_это_нет_а_не_паника() {
        assert!(!alive("не-адрес", 443, T));
        assert!(!alive("", 443, T));
        assert!(!alive("999.1.1.1", 443, T));
    }

    #[test]
    fn filter_alive_разделяет_живые_и_мёртвые_сохраняя_порядок() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // 127.0.0.2 на том же порту никто не слушает (слушатель привязан к .1)
        let entries = vec![e("127.0.0.1", "a.com"), e("127.0.0.2", "b.com"), e("127.0.0.1", "c.com"), e("127.0.0.2", "d.com")];
        let r = filter_alive(&entries, port, T);
        assert_eq!(r.entries, vec![e("127.0.0.1", "a.com"), e("127.0.0.1", "c.com")]);
        assert_eq!(r.dropped, vec![e("127.0.0.2", "b.com"), e("127.0.0.2", "d.com")]);
        assert_eq!(r.dead, vec!["127.0.0.2"], "адрес в списке мёртвых один раз");
    }

    #[test]
    fn filter_alive_пустой_вход_и_все_мёртвые() {
        assert_eq!(filter_alive(&[], 443, T), Alive::default());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let r = filter_alive(&[e("127.0.0.1", "x.com")], port, T);
        assert!(r.entries.is_empty() && r.dead == vec!["127.0.0.1"]);
    }

    /* ---------------- капча ---------------- */

    #[test]
    fn капча_редирект_на_sorry_и_429() {
        assert_eq!(judge_head("HTTP/1.1 302 Found\r\nLocation: https://www.google.com/sorry/index?continue=x\r\nServer: GFE"), Some(true));
        assert_eq!(judge_head("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60"), Some(true));
        assert_eq!(judge_head("HTTP/1.1 302 Found\r\nlocation: https://google.ru/sorry/index"), Some(true), "регистр заголовка и зона");
    }

    #[test]
    fn капча_нормальные_ответы_её_не_дают() {
        assert_eq!(judge_head("HTTP/1.1 200 OK\r\nContent-Type: text/html"), Some(false));
        assert_eq!(judge_head("HTTP/1.1 302 Found\r\nLocation: https://gemini.google.com/app"), Some(false), "обычный редирект");
        assert_eq!(judge_head("HTTP/1.1 404 Not Found"), Some(false));
    }

    #[test]
    fn капча_оборванный_и_мусорный_ответ_это_неизвестно() {
        assert_eq!(judge_head(""), None);
        assert_eq!(judge_head("мусор"), None);
        assert_eq!(judge_head("HTTP/1.1"), None);
        assert_eq!(judge_head("HTTP/1.1 abc OK"), None);
    }

    #[test]
    fn google_имена_распознаются_строго_по_суффиксу() {
        for h in ["gemini.google.com", "aistudio.google.com", "x.googleapis.com", "labs.google", "aitestkitchen.withgoogle.com", "GOOGLE.COM"] {
            assert!(is_google_host(h), "{h}");
        }
        for h in ["notgoogle.com", "google.com.evil.ru", "chatgpt.com", "googleapis.com.ru"] {
            assert!(!is_google_host(h), "{h}");
        }
    }

    /* ---------------- сеть (вручную) ----------------
       Живые прокси меняются каждую неделю, поэтому проверки против реальной
       сети не входят в обычный прогон: их сравнивают с Node вручную.        */

    #[test]
    #[ignore = "живая сеть: cargo test -- --ignored --nocapture живая_сеть"]
    fn живая_сеть_сводка() {
        let t = Duration::from_secs(8);
        for ip in ["84.38.189.217", "217.60.179.6", "179.254.166.174"] {
            println!("alive   {ip:<16} {}", alive(ip, 443, t));
        }
        for (ip, host) in [
            ("84.38.189.217", "gemini.google.com"),
            ("84.38.189.217", "chatgpt.com"),
            ("84.38.189.217", "example.org"),
            ("217.60.179.6", "gemini.google.com"),
        ] {
            println!("serves  {ip:<16} {host:<22} {}", serves(ip, host, t));
        }
        for ip in ["84.38.189.217", "217.60.179.6"] {
            println!("flagged {ip:<16} {:?}", flagged(ip, "gemini.google.com", Duration::from_secs(10)));
        }
        for ip in ["84.38.189.217", "217.60.179.6", "8.8.8.8"] {
            println!("geo     {ip:<16} {:?}", geo_of(ip, t));
        }
        match fetch_source(crate::hosts::SOURCE_URL, Duration::from_secs(20)) {
            Ok(s) => {
                let p = crate::hosts::parse(&s);
                println!("каталог: {} байт, прокси {}, блоклист {}, обновлён {:?}", s.len(), p.unblock.len(), p.blocklist.len(), p.updated);
            }
            Err(e) => println!("каталог: ОШИБКА {e}"),
        }
    }
}
