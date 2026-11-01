//! DoH-запрос (RFC 8484) к конкретному адресу.
//!
//! Нужен там, где обычный DNS закрыт: DNS-AI отвечает только по шифрованному
//! каналу, причём DoH отдаёт строго по HTTP/2 (на HTTP/1.1 — 505). Системный
//! `curl.exe` для этого не годится: в сборке Windows он без HTTP/2 (проверено
//! на curl 8.21 — «option --http2: the installed libcurl version does not
//! support this»). Поэтому здесь свой минимальный клиент на `h2`.
//!
//! Протокол выбирается ALPN в одном соединении: h2 если сервер умеет, иначе
//! HTTP/1.1 — так проверка не ложится на серверах, которым h2 не нужен.
//!
//! Подключаемся к IP, а не к имени, но SNI и проверка сертификата идут по
//! настоящему имени. По имени нельзя: после переключения системный DNS — это
//! сам проверяемый резолвер, и его собственное имя ему же пришлось бы
//! разрешать (курица и яйцо).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

/// Запрос A-записи в wire-формате: id=0, RD=1, один вопрос.
pub fn dns_query_a(host: &str) -> Vec<u8> {
    let mut q = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    q
}

/// base64url без заполнения — так RFC 8484 кладёт запрос в `?dns=`.
pub fn b64url(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

/// Ответ годится: HTTP 200, rcode 0 и хотя бы одна запись в ответе.
pub fn answer_ok(status: u16, body: &[u8]) -> bool {
    status == 200 && body.len() >= 12 && (body[3] & 0x0f) == 0 && u16::from_be_bytes([body[6], body[7]]) > 0
}

/// Разбор `Transfer-Encoding: chunked`. None — тело оборвано или испорчено.
pub fn dechunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n")?;
        let size_str = std::str::from_utf8(&b[..eol]).ok()?;
        let size = usize::from_str_radix(size_str.split(';').next()?.trim(), 16).ok()?;
        b = &b[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        if b.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// A-записи из ответа: вопросы пропускаем, из ответов берём тип 1 класса IN.
/// Сжатые имена (указатель 0xC0) занимают два байта — их не разворачиваем.
pub fn parse_a(body: &[u8]) -> Vec<Ipv4Addr> {
    fn skip_name(b: &[u8], mut i: usize) -> Option<usize> {
        loop {
            let len = *b.get(i)? as usize;
            if len == 0 {
                return Some(i + 1);
            }
            if len & 0xc0 == 0xc0 {
                return Some(i + 2);
            }
            i += 1 + len;
        }
    }
    let mut out = Vec::new();
    if body.len() < 12 {
        return out;
    }
    let qd = u16::from_be_bytes([body[4], body[5]]);
    let an = u16::from_be_bytes([body[6], body[7]]);
    let mut i = 12;
    for _ in 0..qd {
        let Some(n) = skip_name(body, i) else { return out };
        i = n + 4;
    }
    for _ in 0..an {
        let Some(n) = skip_name(body, i) else { return out };
        let Some(h) = body.get(n..n + 10) else { return out };
        let (kind, class, len) = (u16::from_be_bytes([h[0], h[1]]), u16::from_be_bytes([h[2], h[3]]), u16::from_be_bytes([h[8], h[9]]) as usize);
        let Some(data) = body.get(n + 10..n + 10 + len) else { return out };
        if kind == 1 && class == 1 && len == 4 {
            out.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        i = n + 10 + len;
    }
    out
}

/// Блокирующий фасад: один запрос с общим таймаутом.
pub fn doh_once(url: &str, ip: Option<&str>, host: &str, timeout: Duration) -> bool {
    doh_raw(url, ip, host, timeout).is_some_and(|(status, body)| answer_ok(status, &body))
}

/// A-записи имени через DoH; None — сервер не ответил как надо.
pub fn doh_a(url: &str, ip: Option<&str>, host: &str, timeout: Duration) -> Option<Vec<Ipv4Addr>> {
    let (status, body) = doh_raw(url, ip, host, timeout)?;
    answer_ok(status, &body).then(|| parse_a(&body))
}

fn doh_raw(url: &str, ip: Option<&str>, host: &str, timeout: Duration) -> Option<(u16, Vec<u8>)> {
    let u = Url::parse(url).ok().filter(|u| u.scheme() == "https")?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async { tokio::time::timeout(timeout, request(&u, ip, host)).await.ok().flatten() })
}

/// Отвечает ли DoH-сервер: по каждому IP, затем по имени.
pub fn probe_doh(doh: &str, servers: &[String], servers6: &[String], timeout: Duration) -> bool {
    if doh.is_empty() {
        return false;
    }
    for ip in servers.iter().chain(servers6.iter()) {
        if doh_once(doh, Some(ip), "example.com", timeout) {
            return true;
        }
    }
    doh_once(doh, None, "example.com", timeout)
}

async fn request(u: &Url, ip: Option<&str>, host: &str) -> Option<(u16, Vec<u8>)> {
    let hostname = u.host_str()?;
    let port = u.port_or_known_default().unwrap_or(443);

    let tcp = match ip {
        Some(ip) => TcpStream::connect(SocketAddr::new(ip.parse::<IpAddr>().ok()?, port)).await.ok()?,
        None => TcpStream::connect((hostname, port)).await.ok()?,
    };
    let connector = native_tls::TlsConnector::builder().request_alpns(&["h2", "http/1.1"]).build().ok()?;
    let tls = tokio_native_tls::TlsConnector::from(connector).connect(hostname, tcp).await.ok()?;
    let alpn = tls.get_ref().negotiated_alpn().ok().flatten();

    let authority = match u.port() {
        Some(p) => format!("{hostname}:{p}"),
        None => hostname.to_string(),
    };
    let path = format!("{}?dns={}", u.path(), b64url(&dns_query_a(host)));

    if alpn.as_deref() == Some(b"h2".as_slice()) {
        h2_get(tls, &authority, &path).await
    } else {
        h1_get(tls, &authority, &path).await
    }
}

async fn h2_get<T>(io: T, authority: &str, path: &str) -> Option<(u16, Vec<u8>)>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (client, conn) = h2::client::handshake(io).await.ok()?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}{path}"))
        .header("accept", "application/dns-message")
        .body(())
        .ok()?;
    let mut client = client.ready().await.ok()?;
    let (resp, _) = client.send_request(req, true).ok()?;
    let resp = resp.await.ok()?;
    let status = resp.status().as_u16();

    let mut body = resp.into_body();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.ok()?;
        let _ = body.flow_control().release_capacity(chunk.len());
        buf.extend_from_slice(&chunk);
        if buf.len() > 64 * 1024 {
            break; // DNS-ответ не бывает таким большим
        }
    }
    Some((status, buf))
}

async fn h1_get<T>(mut io: T, authority: &str, path: &str) -> Option<(u16, Vec<u8>)>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let req = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/dns-message\r\nConnection: close\r\n\r\n");
    io.write_all(req.as_bytes()).await.ok()?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match io.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > 64 * 1024 {
                    break;
                }
            }
        }
    }
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    let body = &raw[split + 4..];
    let body = if chunked { dechunk(body)? } else { body.to_vec() };
    Some((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_известные_векторы_rfc4648() {
        for (raw, enc) in [("", ""), ("f", "Zg"), ("fo", "Zm8"), ("foo", "Zm9v"), ("foob", "Zm9vYg"), ("fooba", "Zm9vYmE"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(b64url(raw.as_bytes()), enc, "{raw:?}");
        }
    }

    #[test]
    fn base64url_алфавит_url_безопасный_без_заполнения() {
        // 0xfb 0xff → «+/» в обычном base64, «-_» здесь
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert!(!b64url(&[1, 2, 3, 4, 5]).contains(['+', '/', '=']));
    }

    #[test]
    fn запрос_a_для_example_com_побайтно() {
        // тот же запрос, что руками собирали для curl: AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE
        assert_eq!(b64url(&dns_query_a("example.com")), "AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE");
    }

    #[test]
    fn ответ_годится_только_при_200_rcode0_и_записях() {
        let mut ok = vec![0u8; 12];
        ok[7] = 1; // ancount = 1
        assert!(answer_ok(200, &ok));
        assert!(!answer_ok(505, &ok), "статус");
        assert!(!answer_ok(200, &ok[..11]), "короткий ответ");
        let mut nx = ok.clone();
        nx[3] = 3; // NXDOMAIN
        assert!(!answer_ok(200, &nx), "rcode");
        let mut empty = ok.clone();
        empty[7] = 0;
        assert!(!answer_ok(200, &empty), "ни одной записи");
        assert!(answer_ok(200, &{ let mut b = ok.clone(); b[3] = 0x80; b }), "старшие биты флагов не rcode");
    }

    #[test]
    fn a_записи_достаются_и_со_сжатыми_именами() {
        // Вопрос example.com A IN, затем CNAME (пропускается) и две A по указателю на имя вопроса.
        let mut b = vec![0, 0, 0x81, 0x80, 0, 1, 0, 3, 0, 0, 0, 0];
        b.extend_from_slice(&dns_query_a("example.com")[12..]);
        b.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xc0, 12]);
        b.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
        b.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 5, 6, 7, 8]);
        assert_eq!(parse_a(&b), vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)]);
        assert!(parse_a(&b[..b.len() - 3]).len() == 1, "оборванная запись не читается за краем");
        assert!(parse_a(&[0; 5]).is_empty());
    }

    #[test]
    fn chunked_склеивается_и_оборванное_отвергается() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert_eq!(dechunk(b"A;ext=1\r\n0123456789\r\n0\r\n\r\n").unwrap(), b"0123456789", "расширения чанка игнорируются");
        assert!(dechunk(b"5\r\nab").is_none(), "оборвано");
        assert!(dechunk(b"zz\r\nab\r\n").is_none(), "не число");
        assert!(dechunk(b"").is_none());
    }

    #[test]
    fn мусорный_url_и_не_https_не_роняют() {
        let t = Duration::from_millis(300);
        assert!(!doh_once("не url", None, "example.com", t));
        assert!(!doh_once("http://example.com/dns-query", None, "example.com", t));
        assert!(!probe_doh("", &[], &[], t));
    }

    #[test]
    fn недоступный_адрес_это_false_за_разумное_время() {
        let t0 = std::time::Instant::now();
        assert!(!doh_once("https://dns.dns-ai.ru/dns-query", Some("203.0.113.9"), "example.com", Duration::from_millis(800)));
        assert!(t0.elapsed() < Duration::from_secs(3), "таймаут соблюдается");
    }

    #[test]
    #[ignore = "живая сеть: cargo test -- --ignored --nocapture живая_сеть_doh"]
    fn живая_сеть_doh() {
        let t = Duration::from_secs(8);
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        println!("dns-ai (h2 only)  {}", probe_doh("https://dns.dns-ai.ru/dns-query", &v(&["194.87.196.13", "45.151.102.219"]), &v(&["2a00:b700:5::3:62"]), t));
        println!("cloudflare        {}", probe_doh("https://cloudflare-dns.com/dns-query", &v(&["1.1.1.1", "1.0.0.1"]), &[], t));
        println!("google            {}", probe_doh("https://dns.google/dns-query", &v(&["8.8.8.8", "8.8.4.4"]), &[], t));
        println!("мусорный          {}", probe_doh("https://nonexistent.invalid/dns-query", &v(&["203.0.113.7"]), &[], Duration::from_secs(2)));
        println!("только dns-ai v6  {}", probe_doh("https://dns.dns-ai.ru/dns-query", &[], &v(&["2a00:b700:5::3:62"]), t));
        println!("zapret.moe        {}", probe_doh("https://dns.zapret.moe/dns-query", &v(&["83.217.211.149"]), &v(&["2a03:afc0:9::235d"]), t));
        println!("malw.link         {}", probe_doh("https://dns.malw.link/dns-query", &v(&["179.254.166.171", "164.5.224.81"]), &[], t));
        println!("commonnet         {}", probe_doh("https://dns.commonnet.work/dns-query", &v(&["94.181.44.39"]), &[], t));
        println!("bezmezhau         {}", probe_doh("https://dns.bezmezhau.com/dns-query", &v(&["181.215.40.49"]), &[], t));
        println!("geohide           {}", probe_doh("https://geohide.ru/dns-query", &v(&["193.233.112.67", "193.233.112.68"]), &[], t));
    }
}
