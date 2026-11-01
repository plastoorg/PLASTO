//! Служба Windows из старых версий PLASTO и драйвер WinDivert.
//!
//! Новая версия службу не ставит: обходом управляет приложение. Отсюда её
//! можно только найти и удалить, а заодно снять зарегистрированный драйвер
//! WinDivert, если он остался после другой программы.
use serde_json::{json, Value};
use std::process::{Command, Output};

const NAME: &str = "PlastoZapret";

fn sc(args: &[&str]) -> Result<Output, String> {
    let mut c = Command::new("sc.exe");
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    c.output().map_err(|e| e.to_string())
}

fn running(text: &str) -> bool {
    text.lines().any(|l| l.contains("STATE") && l.split_whitespace().any(|w| w == "4"))
}

fn command(args: &[&str]) -> Result<(), String> {
    let output = sc(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("sc {}: код {}", args.first().unwrap_or(&""), output.status.code().unwrap_or(-1)))
    }
}

/// Есть ли служба из старой версии и зарегистрирован ли WinDivert.
pub fn status() -> Value {
    let own = match sc(&["query", NAME]) {
        Ok(o) => o,
        Err(e) => return json!({"error":format!("не удалось проверить службу: {e}")}),
    };
    // 1060 — службы нет.
    if !own.status.success() && own.status.code() != Some(1060) {
        return json!({"error":format!("проверка службы завершилась с кодом {}",own.status.code().unwrap_or(-1))});
    }
    let installed = own.status.success();
    let active = installed && running(&String::from_utf8_lossy(&own.stdout));
    let mut windivert = Value::Null;
    for name in ["WinDivert", "WinDivert14"] {
        if let Ok(o) = sc(&["query", name]) {
            if o.status.success() {
                windivert = json!({"name":name,"running":running(&String::from_utf8_lossy(&o.stdout))});
                break;
            }
        }
    }
    json!({"name":NAME,"installed":installed,"running":active,"windivert":windivert})
}

/// Удаляет службу из старой версии: остановить, если работает, и удалить.
pub fn remove() -> Value {
    let existing = status();
    if existing["error"].is_string() {
        return existing;
    }
    if existing["installed"] != true {
        return json!({"error":"служба PLASTO не установлена"});
    }
    if existing["running"] == true {
        if let Err(e) = command(&["stop", NAME]) {
            return json!({"error":e});
        }
    }
    if let Err(e) = command(&["delete", NAME]) {
        return json!({"error":e});
    }
    status()
}

/// Снимает зарегистрированный драйвер WinDivert. Звать только при выключенном
/// обходе: драйвер выгрузится, когда его не держит ни один процесс.
pub fn drop_windivert() -> Value {
    let existing = status();
    if existing["error"].is_string() {
        return existing;
    }
    let Some(name) = existing["windivert"]["name"].as_str() else {
        return json!({"error":"драйвер WinDivert не зарегистрирован"});
    };
    if existing["windivert"]["running"] == true {
        // Остановка может не пройти, пока драйвер кем-то занят; удаление всё
        // равно отметит службу, и она уйдёт с последним дескриптором.
        let _ = command(&["stop", name]);
    }
    if let Err(e) = command(&["delete", name]) {
        return json!({"error":e});
    }
    status()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_is_running_only_for_numeric_four() {
        assert!(running(" STATE : 4 RUNNING"));
        assert!(!running(" STATE : 1 STOPPED"));
    }
}
