//! Чужие копии того, что запускает PLASTO: другой zapret (служба или процесс
//! winws) и другой Telegram-прокси или программа на его порту. Здесь их
//! находят и, по согласию человека, останавливают.
use std::{process::Command, time::{Duration, Instant}};

use serde_json::{json, Value};

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}

fn output(file: &str, args: &[&str]) -> Result<String, String> {
    let out = hidden(Command::new(file).args(args)).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(crate::platform::console_text(&out.stdout))
    } else {
        Err(format!("{file}: код {}", out.status.code().unwrap_or(-1)))
    }
}

/// PowerShell отдаёт один объект без массива, ноль — пустую строку.
fn as_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        one => vec![one],
    }
}

/* ---------- другой zapret ------------------------------------------------- */

/// Службы, чей exe — winws, и процессы winws, кроме нашего (`own`).
pub fn winws(own: Option<u32>) -> Value {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
        $s = @(Get-CimInstance Win32_Service | Where-Object { $_.PathName -match 'winws2?\\.exe' } | Select-Object Name,DisplayName,State,StartMode,PathName); \
        $p = @(Get-CimInstance Win32_Process -Filter \"Name='winws.exe' OR Name='winws2.exe'\" | Select-Object ProcessId,Name,ExecutablePath); \
        @{services=$s; processes=$p} | ConvertTo-Json -Depth 3 -Compress";
    let text = match output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", script]) {
        Ok(t) => t,
        Err(e) => return json!({"error":format!("не удалось проверить запущенные программы: {e}")}),
    };
    let found: Value = serde_json::from_str(text.trim()).unwrap_or(Value::Null);
    let services: Vec<Value> = as_list(found["services"].clone())
        .into_iter()
        .map(|s| json!({"name":s["Name"],"title":s["DisplayName"],"running":s["State"]=="Running","auto":s["StartMode"]=="Auto","path":s["PathName"]}))
        .collect();
    let processes: Vec<Value> = as_list(found["processes"].clone())
        .into_iter()
        .filter(|p| own.is_none_or(|own| p["ProcessId"].as_u64() != Some(own as u64)))
        .map(|p| json!({"pid":p["ProcessId"],"name":p["Name"],"path":p["ExecutablePath"]}))
        .collect();
    json!({"services":services,"processes":processes})
}

fn sc(args: &[&str]) -> Result<(), String> {
    output("sc.exe", args).map(|_| ())
}

/// Процесс вместе с дочерними: tg-ws-proxy собран PyInstaller, порт держит
/// дочерний процесс, и без `/T` он переживает родителя.
pub fn kill(pid: u64) -> Result<(), String> {
    output("taskkill.exe", &["/PID", &pid.to_string(), "/T", "/F"]).map(|_| ())
}

/// Останавливает чужой zapret: службы с winws, затем оставшиеся процессы.
/// `disable` — ещё и выключить автозапуск служб, чтобы не вернулись после
/// перезагрузки. Ждёт, пока winws не останется.
pub fn stop_winws(own: Option<u32>, disable: bool) -> Value {
    let found = winws(own);
    if found["error"].is_string() {
        return found;
    }
    let mut errors = Vec::new();
    for svc in found["services"].as_array().into_iter().flatten() {
        let name = svc["name"].as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if disable {
            if let Err(e) = sc(&["config", name, "start=", "disabled"]) {
                errors.push(format!("служба {name}: автозапуск не выключился ({e})"));
            }
        }
        if svc["running"] == true {
            if let Err(e) = sc(&["stop", name]) {
                errors.push(format!("служба {name} не остановилась ({e})"));
            }
        }
    }
    // Служба останавливает winws не сразу; что осталось — снимаем сами.
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let left = winws(own);
        let procs = left["processes"].as_array().cloned().unwrap_or_default();
        if procs.is_empty() {
            break;
        }
        if Instant::now() > deadline {
            errors.push("winws не завершился".into());
            break;
        }
        for p in &procs {
            if let Some(pid) = p["pid"].as_u64() {
                let _ = kill(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    if errors.is_empty() {
        json!({"ok":true})
    } else {
        json!({"error":errors.join("; ")})
    }
}

/* ---------- Telegram-прокси ----------------------------------------------- */

/// Кто слушает TCP-порт на этой машине: pid из `netstat -ano`.
fn listener(text: &str, port: u16) -> Option<u32> {
    let suffix = format!(":{port}");
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let listening = f.len() >= 5 && f[0].eq_ignore_ascii_case("TCP") && f[3].eq_ignore_ascii_case("LISTENING");
        (listening && f[1].ends_with(&suffix)).then(|| f[4].parse().ok()).flatten()
    })
}

/// Процессы из `tasklist /FO CSV /NH`: `(pid, имя)`. Строки без числа во
/// втором поле — сообщения tasklist («ИНФОРМАЦИЯ: …» на русской Windows,
/// «INFO: …» на английской), а не процессы.
fn processes(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split(',').map(|x| x.trim().trim_matches('"'));
            let name = f.next()?.to_owned();
            let pid: u32 = f.next()?.parse().ok()?;
            (!name.is_empty()).then_some((pid, name))
        })
        .collect()
}

fn process_name(pid: u32) -> Option<String> {
    let text = output("tasklist.exe", &["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).ok()?;
    processes(&text).into_iter().find(|(p, _)| *p == pid).map(|(_, name)| name)
}

/// Похоже ли имя процесса на tg-ws-proxy (свой или скачанный отдельно).
pub fn is_tgws(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("tgwsproxy") || n.contains("tg-ws-proxy") || n.starts_with("tgws-")
}

/// Системные процессы не трогаем: их остановка уронит Windows или сеть.
fn is_system(pid: u32, name: &str) -> bool {
    pid <= 4 || ["svchost.exe", "system", "lsass.exe", "services.exe", "wininit.exe", "csrss.exe"].iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// Кто занимает порт: `{pid, name, tgws, system}` или `null`, если свободен.
pub fn port_owner(port: u16) -> Value {
    let Ok(text) = output("netstat.exe", &["-ano", "-p", "TCP"]) else { return Value::Null };
    let Some(pid) = listener(&text, port) else { return Value::Null };
    let name = process_name(pid).unwrap_or_else(|| "неизвестная программа".into());
    json!({"pid":pid,"name":name,"tgws":is_tgws(&name),"system":is_system(pid,&name)})
}

/// Процессы tg-ws-proxy из `tasklist /FO CSV`.
fn tgws_pids(text: &str) -> Vec<(u32, String)> {
    processes(text).into_iter().filter(|(_, name)| is_tgws(name)).collect()
}

/// Другие tg-ws-proxy, кроме нашего: у него защита от второй копии, так что
/// при работающем чужом наш просто не стартует — какой бы ни был порт.
pub fn other_tgws(own: Option<u32>) -> Option<Value> {
    let text = output("tasklist.exe", &["/FO", "CSV", "/NH"]).ok()?;
    let (pid, name) = tgws_pids(&text).into_iter().find(|(pid, _)| Some(*pid) != own)?;
    Some(json!({"pid":pid,"name":name,"tgws":true,"system":false}))
}

/// Порт и секрет из конфига Flowseal tg-ws-proxy: портативного (папка
/// `TgWsProxy_data` рядом с exe) или общего (`%APPDATA%\TgWsProxy`).
fn flowseal_config(exe: Option<&std::path::Path>, appdata: Option<&std::path::Path>) -> Option<(u16, String)> {
    let portable = exe.and_then(|e| e.parent()).map(|d| d.join("TgWsProxy_data").join("config.json"));
    let shared = appdata.map(|a| a.join("TgWsProxy").join("config.json"));
    let cfg: Value = [portable, shared].into_iter().flatten().find_map(|p| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()))?;
    let port = cfg["port"].as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p >= 1024)?;
    let secret = cfg["secret"].as_str().filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))?;
    Some((port, secret.to_owned()))
}

/// Порт и секрет чужого tg-ws-proxy, пока он ещё работает: прокси PLASTO
/// займёт их, и Telegram, настроенный на него, переподключится сам. Только
/// Flowseal: у Rust-версии настройки в аргументах, а не в файле.
pub fn tgws_config(pid: u32) -> Option<(u16, String)> {
    if !is_tgws(&process_name(pid)?) {
        return None;
    }
    let script = format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; (Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").ExecutablePath");
    let exe = output("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &script]).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    let appdata = std::env::var_os("APPDATA");
    flowseal_config(exe.as_deref().map(std::path::Path::new), appdata.as_deref().map(std::path::Path::new))
}

/// Остановить то, что мешает Telegram-прокси. Если это tg-ws-proxy —
/// все его процессы (родитель и дочерние), иначе — процесс с потомками.
pub fn stop_process(pid: u32) -> Value {
    let Some(name) = process_name(pid) else { return json!({"ok":true}) };
    if is_system(pid, &name) {
        return json!({"error":format!("{name} — системный процесс, его останавливать нельзя")});
    }
    let mut targets = vec![pid];
    if is_tgws(&name) {
        if let Ok(text) = output("tasklist.exe", &["/FO", "CSV", "/NH"]) {
            targets = tgws_pids(&text).into_iter().map(|(p, _)| p).collect();
        }
    }
    for p in &targets {
        let _ = kill(*p as u64);
    }
    std::thread::sleep(Duration::from_millis(600));
    match targets.iter().find_map(|p| process_name(*p).map(|n| (*p, n))) {
        None => json!({"ok":true}),
        Some((p, n)) => json!({"error":format!("{n} (процесс {p}) не остановился: возможно, нужны права администратора")}),
    }
}

/// Первый свободный порт после `from`.
pub fn free_port(from: u16) -> Option<u16> {
    (from.saturating_add(1)..from.saturating_add(200)).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_listener_on_exact_port() {
        let text = "  Proto  Local Address  Foreign Address  State  PID\n  TCP    0.0.0.0:14430   0.0.0.0:0  LISTENING  10\n  TCP    127.0.0.1:1443   0.0.0.0:0  LISTENING  4242\n  TCP    127.0.0.1:50000  1.2.3.4:443  ESTABLISHED  7\n";
        assert_eq!(listener(text, 1443), Some(4242));
        assert_eq!(listener(text, 443), None);
        assert_eq!(listener("  TCP    [::]:1443  [::]:0  LISTENING  9", 1443), Some(9));
    }

    #[test]
    fn lists_every_tgws_process() {
        let text = "\"TgWsProxy_windows.exe\",\"26312\",\"Console\"
\"TgWsProxy_windows.exe\",\"22900\",\"Console\"
\"chrome.exe\",\"1\",\"Console\"";
        assert_eq!(tgws_pids(text).iter().map(|p| p.0).collect::<Vec<_>>(), vec![26312, 22900]);
    }

    /// Пустой ответ tasklist — не процесс, на каком бы языке ни была Windows.
    #[test]
    fn tasklist_message_is_not_a_process() {
        assert!(processes("ИНФОРМАЦИЯ: Задачи, отвечающие заданным критериям, отсутствуют.").is_empty());
        assert!(processes("INFO: No tasks are running which match the specified criteria.").is_empty());
        assert!(processes("?????: ????? ???????? ????????, ???????????.").is_empty());
        assert_eq!(processes("\"TgWsProxy_windows (2).exe\",\"14072\",\"Console\",\"1\",\"60 000 КБ\""), vec![(14072, "TgWsProxy_windows (2).exe".to_owned())]);
    }

    #[test]
    fn recognises_tgws_names() {
        assert!(is_tgws("TgWsProxy_windows.exe"));
        assert!(is_tgws("tgws-0123456789abcdef.exe"));
        assert!(!is_tgws("telegram.exe"));
    }

    #[test]
    fn reads_portable_flowseal_config_first() {
        let dir = tempfile::tempdir().unwrap();
        let (exe_dir, appdata) = (dir.path().join("app"), dir.path().join("roaming"));
        std::fs::create_dir_all(exe_dir.join("TgWsProxy_data")).unwrap();
        std::fs::create_dir_all(appdata.join("TgWsProxy")).unwrap();
        let secret = "fa014fe54885f452ca7e4bdf871b4eb6";
        std::fs::write(appdata.join("TgWsProxy/config.json"), json!({"port":1080,"secret":secret}).to_string()).unwrap();
        let exe = exe_dir.join("TgWsProxy_windows.exe");
        assert_eq!(flowseal_config(Some(&exe), Some(&appdata)), Some((1080, secret.to_owned())));
        std::fs::write(exe_dir.join("TgWsProxy_data/config.json"), json!({"port":1500,"secret":secret}).to_string()).unwrap();
        assert_eq!(flowseal_config(Some(&exe), Some(&appdata)), Some((1500, secret.to_owned())));
        std::fs::write(exe_dir.join("TgWsProxy_data/config.json"), json!({"port":1500,"secret":"short"}).to_string()).unwrap();
        assert_eq!(flowseal_config(Some(&exe), None), None);
    }

    #[test]
    fn system_processes_are_protected() {
        assert!(is_system(4, "System"));
        assert!(is_system(900, "svchost.exe"));
        assert!(!is_system(900, "nginx.exe"));
    }

    /// Живая система, только чтение: `cargo test -- --ignored live_lookup --nocapture`.
    #[test]
    #[ignore]
    fn live_lookup() {
        println!("winws: {}", winws(None));
        println!("other tgws: {:?}", other_tgws(None));
        println!("port 1443: {}", port_owner(1443));
        println!("free after 1443: {:?}", free_port(1443));
    }

    #[test]
    fn powershell_single_object_becomes_list() {
        assert_eq!(as_list(json!({"a":1})).len(), 1);
        assert!(as_list(Value::Null).is_empty());
        assert_eq!(as_list(json!([1, 2])).len(), 2);
    }
}
