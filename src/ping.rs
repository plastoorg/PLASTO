//! Пинг до сервисов для главной: сколько миллисекунд до ответа сайта, по
//! медиане сайтов сервиса. Ответ с любым HTTP-кодом — сайт доступен; таймаут
//! и сброс соединения — нет.
use std::{sync::mpsc, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

use serde_json::{json, Map, Value};

use crate::host::Host;

const TIMEOUT: Duration = Duration::from_secs(6);

/// Сайты механизма. Для ИИ — по одному адресу от каждого включённого
/// сервиса, не больше трёх.
fn sites(mech: &str, cfg: &Map<String, Value>) -> Vec<String> {
    match mech {
        // Видео идёт с googlevideo: страница YouTube может открываться и без
        // обхода, а ролики — нет.
        "zapret" => vec!["https://www.youtube.com/generate_204".into(), "https://redirector.googlevideo.com/generate_204".into(), "https://discord.com/api/v9/gateway".into()],
        "ai" => {
            let on = |key: &str| cfg.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
            let ids: Vec<Value> = on("aiBypass").into_iter().chain(on("aiProxyServices")).collect();
            let mut out: Vec<String> = Vec::new();
            for svc in crate::ai::services().as_array().into_iter().flatten() {
                if !ids.contains(&svc["id"]) {
                    continue;
                }
                if let Some(domain) = svc["domains"].as_array().and_then(|d| d.first()).and_then(Value::as_str) {
                    let url = format!("https://{}/", domain.trim_start_matches("*."));
                    if !out.contains(&url) {
                        out.push(url);
                    }
                }
            }
            out.truncate(3);
            out
        }
        _ => Vec::new(),
    }
}

/// Имя сервиса для человека: «не открывается YouTube», а не адрес.
fn service_of(host: &str) -> String {
    match host {
        h if h.contains("youtube") || h.contains("googlevideo") => "YouTube".into(),
        h if h.contains("discord") => "Discord".into(),
        h if h.contains("telegram") => "Telegram".into(),
        h => h.to_owned(),
    }
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://").split('/').next().unwrap_or("").to_owned()
}

/// Один сайт: время до ответа или ошибка.
fn reach(url: &str) -> Result<u64, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let agent = ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).build();
    let began = Instant::now();
    match agent.head(url).set("Connection", "close").set("User-Agent", "Mozilla/5.0").timeout(TIMEOUT).call() {
        Ok(_) | Err(ureq::Error::Status(..)) => Ok(began.elapsed().as_millis() as u64),
        Err(e) => Err(if e.to_string().contains("timed out") { "таймаут".into() } else { "нет соединения".into() }),
    }
}

/// DNS: время, за которое системный резолвер отвечает на имя.
fn resolve(host: &str) -> Result<u64, String> {
    let began = Instant::now();
    let found = crate::hosts::lookup(host, TIMEOUT);
    if found.ok {
        Ok(began.elapsed().as_millis() as u64)
    } else {
        Err("имя не разрешилось".into())
    }
}

pub fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    Some(if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]).div_ceil(2) })
}

/// Пинг механизмов из `payload["mechs"]`; все сайты меряются параллельно.
pub fn run(app: &Host, cfg: &Map<String, Value>, payload: &Value) -> Value {
    let mechs: Vec<String> = payload["mechs"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let (tx, rx) = mpsc::channel();
    let mut count = 0;
    for mech in &mechs {
        // Telegram — через сам прокси: напрямую его адреса у провайдера
        // заблокированы, а прокси ходит в обход (WebSocket, Cloudflare).
        if mech == "telegram" {
            let tg = crate::tgws::snapshot(app);
            if let (Some(port), Some(secret)) = (tg["port"].as_u64().and_then(|p| u16::try_from(p).ok()), tg["secret"].as_str().filter(|_| tg["running"] == true)) {
                let (tx, mech, secret) = (tx.clone(), mech.clone(), secret.to_owned());
                count += 1;
                std::thread::spawn(move || {
                    let _ = tx.send((mech, "Telegram через прокси".into(), crate::tgws::probe(port, &secret, TIMEOUT)));
                });
            }
            continue;
        }
        let jobs: Vec<(String, bool)> = if mech == "dns" {
            ["www.youtube.com", "discord.com", "chatgpt.com"].iter().map(|h| (h.to_string(), true)).collect()
        } else {
            sites(mech, cfg).into_iter().map(|u| (u, false)).collect()
        };
        for (target, dns) in jobs {
            let (tx, mech) = (tx.clone(), mech.clone());
            count += 1;
            std::thread::spawn(move || {
                let result = if dns { resolve(&target) } else { reach(&target) };
                let host = if dns { target.clone() } else { host_of(&target) };
                let _ = tx.send((mech, host, result));
            });
        }
    }
    drop(tx);
    let mut out = Map::new();
    for mech in &mechs {
        out.insert(mech.clone(), json!({"ms":null,"sites":[]}));
    }
    for (mech, host, result) in rx.iter().take(count) {
        let entry = &mut out[&mech];
        let site = match &result {
            Ok(ms) => json!({"host":host,"ms":ms}),
            Err(e) => json!({"host":host,"error":e}),
        };
        if let Some(list) = entry["sites"].as_array_mut() {
            list.push(site);
        }
    }
    for entry in out.values_mut() {
        let mut times: Vec<u64> = entry["sites"].as_array().into_iter().flatten().filter_map(|s| s["ms"].as_u64()).collect();
        let sites = entry["sites"].as_array().map_or(0, Vec::len);
        // Работает, только если ответили все сайты: молчащий YouTube при живом
        // Discord — всё равно поломка.
        entry["ok"] = json!(sites > 0 && times.len() == sites);
        let mut down: Vec<String> = Vec::new();
        for site in entry["sites"].as_array().into_iter().flatten().filter(|s| s["ms"].is_null()) {
            let name = service_of(site["host"].as_str().unwrap_or(""));
            if !down.contains(&name) {
                down.push(name);
            }
        }
        entry["down"] = json!(down);
        entry["ms"] = json!(median(&mut times));
    }
    // Процесс winws жив, но какой-то из сайтов не ответил — «нет ответа».
    if let Some(z) = out.get("zapret") {
        if z["sites"].as_array().is_some_and(|s| !s.is_empty()) {
            let down: Vec<&str> = z["down"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            crate::runtime::set_health(app, &down);
        }
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    json!({"ts":ts,"mechs":out})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_rounds_up_between_two() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [40]), Some(40));
        assert_eq!(median(&mut [30, 10, 20]), Some(20));
        assert_eq!(median(&mut [10, 21]), Some(16));
    }

    #[test]
    fn ai_sites_follow_enabled_services_only() {
        let cfg = Map::new();
        assert!(sites("ai", &cfg).is_empty());
        let first = crate::ai::services()[0]["id"].clone();
        let cfg = json!({"aiBypass":[first]}).as_object().cloned().unwrap();
        assert_eq!(sites("ai", &cfg).len(), 1);
    }

    #[test]
    fn service_names_hosts() {
        assert_eq!(service_of("redirector.googlevideo.com"), "YouTube");
        assert_eq!(service_of("discord.com"), "Discord");
        assert_eq!(service_of("example.org"), "example.org");
    }

    #[test]
    fn host_is_taken_from_url() {
        assert_eq!(host_of("https://discord.com/api/v9/gateway"), "discord.com");
    }
}
