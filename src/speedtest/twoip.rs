//! Серверы 2ip.io: свой протокол поверх WebSocket, без авторизации.
//!
//! Разобран по трафику их страницы:
//!   `PING <ts>` → `PONG`
//!   `DOWNLOAD <n>` → бинарный кадр ровно на n байт
//!   бинарный кадр от клиента → `OK`, когда сервер его принял
//! Страница держит два запроса в полёте на сокет и растит размер кадра,
//! пока кадры приходят быстро; так же делаем и мы.

use std::{
    collections::VecDeque,
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use tungstenite::{stream::MaybeTlsStream, Connector, Message, WebSocket};

use super::{Proto, Region, Server};

const LIST: &str = include_str!("../speed-2ip.txt");

const MIN_FRAME: usize = 5_000;
/// Больше этого страница 2ip сама не просит.
const DOWN_CAP: usize = 5_120_000;
const UP_CAP: usize = 2_000_000;
/// Нули для отдачи: статический буфер уходит в .bss и не раздувает exe.
static ZEROS: [u8; UP_CAP] = [0; UP_CAP];

/// Разбирает снимок списка: `id|страна|город|адрес`, `~порт` — их домен.
pub fn parse(text: &str) -> Vec<Server> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let mut f = line.split('|');
            let (id, cc, city, addr) = (f.next()?, f.next()?, f.next()?, f.next()?);
            let (host, port) = match addr.strip_prefix('~') {
                Some(port) => (format!("server-{id}.2ipcore.com"), port),
                None => {
                    let (h, p) = addr.rsplit_once(':')?;
                    (h.to_owned(), p)
                }
            };
            Some(Server {
                provider: "2ip",
                city: city.to_owned(),
                country: cc.to_owned(),
                region: if cc == "RU" { Region::Ru } else { Region::Abroad },
                host,
                port: port.parse().ok()?,
                proto: Proto::TwoIp,
            })
        })
        .collect()
}

pub fn servers() -> Vec<Server> {
    parse(LIST)
}

/// Следующий размер кадра: быстро пришёл — вчетверо больше, долго — вдвое
/// меньше. На медленном канале огромный кадр не успел бы прийти до конца
/// замера и не попал бы в счёт.
pub fn next_size(size: usize, took: Duration, cap: usize) -> usize {
    if took < Duration::from_millis(150) {
        (size * 4).min(cap)
    } else if took > Duration::from_millis(600) {
        (size / 2).max(MIN_FRAME)
    } else {
        size
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn open(server: &Server) -> Result<Socket, String> {
    let addr = (server.host.as_str(), server.port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("нет адреса")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(Duration::from_secs(4))).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(4))).map_err(|e| e.to_string())?;
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let url = format!("wss://{}:{}/ws", server.host, server.port);
    let (ws, _) = tungstenite::client_tls_with_config(url, tcp, None, Some(Connector::NativeTls(tls))).map_err(|e| e.to_string())?;
    Ok(ws)
}

fn io(e: tungstenite::Error) -> String {
    e.to_string()
}

pub fn pull(server: &Server, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let mut ws = open(server)?;
    let mut size = MIN_FRAME;
    for _ in 0..2 {
        ws.send(Message::text(format!("DOWNLOAD {size}"))).map_err(io)?;
    }
    let mut last = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match ws.read() {
            Ok(Message::Binary(b)) => {
                bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
                size = next_size(size, last.elapsed(), DOWN_CAP);
                last = Instant::now();
                ws.send(Message::text(format!("DOWNLOAD {size}"))).map_err(io)?;
            }
            Ok(_) => {}
            Err(_) if stop.load(Ordering::Acquire) => break,
            Err(e) => return Err(io(e)),
        }
    }
    let _ = ws.close(None);
    Ok(())
}

/// Отданное считается по `OK`: только то, что сервер действительно принял.
pub fn push(server: &Server, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let mut ws = open(server)?;
    let mut size = MIN_FRAME;
    let mut pending = VecDeque::new();
    for _ in 0..2 {
        ws.send(Message::binary(&ZEROS[..size])).map_err(io)?;
        pending.push_back(size);
    }
    let mut last = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match ws.read() {
            Ok(Message::Text(t)) if t.as_str() == "OK" => {
                if let Some(n) = pending.pop_front() {
                    bytes.fetch_add(n as u64, Ordering::Relaxed);
                }
                size = next_size(size, last.elapsed(), UP_CAP);
                last = Instant::now();
                ws.send(Message::binary(&ZEROS[..size])).map_err(io)?;
                pending.push_back(size);
            }
            Ok(_) => {}
            Err(_) if stop.load(Ordering::Acquire) => break,
            Err(e) => return Err(io(e)),
        }
    }
    let _ = ws.close(None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn снимок_списка_разбирается_целиком() {
        let all = servers();
        assert_eq!(all.len(), 233);
        assert!(all.iter().filter(|s| s.region == Region::Ru).count() > 100);
        assert!(all.iter().filter(|s| s.region == Region::Abroad).count() > 50);
        assert!(all.iter().all(|s| s.port > 0 && !s.host.is_empty() && !s.city.is_empty()));
    }

    #[test]
    fn адрес_с_тильдой_это_домен_2ipcore() {
        let s = parse("# комментарий\n\n1252|RU|Магнитогорск|~8002\n520|RU|Ковров|speedtest-2ip.kovrov.net:8001\n1504|FI|Хельсинки|~8828\nмусор\n");
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].host.as_str(), s[0].port), ("server-1252.2ipcore.com", 8002));
        assert_eq!((s[1].host.as_str(), s[1].port), ("speedtest-2ip.kovrov.net", 8001));
        assert_eq!(s[2].region, Region::Abroad);
        assert_eq!(s[2].country, "FI");
    }

    #[test]
    fn размер_кадра_растёт_и_падает_в_границах() {
        let fast = Duration::from_millis(10);
        let slow = Duration::from_secs(1);
        assert_eq!(next_size(5_000, fast, DOWN_CAP), 20_000);
        assert_eq!(next_size(5_000_000, fast, DOWN_CAP), DOWN_CAP);
        assert_eq!(next_size(80_000, slow, DOWN_CAP), 40_000);
        assert_eq!(next_size(MIN_FRAME, slow, DOWN_CAP), MIN_FRAME);
        assert_eq!(next_size(80_000, Duration::from_millis(300), DOWN_CAP), 80_000);
    }
}
