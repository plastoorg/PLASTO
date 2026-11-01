//! Обновление самого PLASTO из релизов github.com/plastoorg/PLASTO.
//!
//! Только по согласию: проверка ничего не скачивает; загрузка кладёт новый
//! exe в папку данных и сверяет SHA-256 с GitHub; замена — переименовать
//! работающий exe в `.old`, положить новый на его место и запустить его.
//! Старый файл удаляется при следующем запуске.
//!
//! Два канала: «стабильный» видит только выпущенные версии (тег без суффикса),
//! «бета» — ещё и предварительные (`-beta-3`), в том числе автосборки CI со
//! свежим zapret, которые никто не проверял вручную.
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, time::Duration};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::host::Host;

const API: &str = "https://api.github.com/repos/plastoorg/PLASTO/releases";
const DOWNLOAD: &str = "https://github.com/plastoorg/PLASTO/releases/download/";
const MAX_EXE: u64 = 64 * 1024 * 1024;

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| format!("TLS: {e}"))?;
    Ok(ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).build())
}

/// Числа версии и суффикс предварительной сборки: «v0.1.2-beta-3» →
/// ([0, 1, 2], Some("beta-3")). Метка сборки после `+` не учитывается.
fn parse(version: &str) -> (Vec<u64>, Option<&str>) {
    let v = version.trim().trim_start_matches(['v', 'V']);
    let v = v.split_once('+').map_or(v, |(v, _)| v);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
}

/// Суффиксы по частям, как в semver: числа — по значению, число младше
/// слова, короткий суффикс младше продолженного («beta» < «beta-1»).
fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| s.split(['.', '-']).map(str::to_owned).collect::<Vec<_>>();
    let (a, b) = (split(a), split(b));
    for (x, y) in a.iter().zip(&b) {
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if order.is_ne() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// Новее ли `tag`, чем `current`. Одинаковые числа: релиз новее
/// предварительной сборки, из предварительных — с большим номером.
pub fn newer(tag: &str, current: &str) -> bool {
    let ((mut a, a_pre), (mut b, b_pre)) = (parse(tag), parse(current));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (a_pre, b_pre) {
            (None, Some(_)) => true,
            (Some(a), Some(b)) => cmp_pre(a, b).is_gt(),
            _ => false,
        },
    }
}

/// Канал по умолчанию: кто поставил предварительную сборку, тот на бете.
pub fn beta_by_default() -> bool {
    env!("CARGO_PKG_VERSION").contains('-')
}

/// Последний релиз канала: версия, адрес exe, размер и сумма. Стабильный —
/// `releases/latest` (GitHub отдаёт там только выпущенные), бета — самый новый
/// из последних релизов, предварительные тоже.
pub fn latest(beta: bool) -> Result<Value, String> {
    let url = if beta { format!("{API}?per_page=30") } else { format!("{API}/latest") };
    let response = match agent()?.get(&url).set("User-Agent", "PLASTO-desktop").set("Accept", "application/vnd.github+json").timeout(Duration::from_secs(30)).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Err(if beta { "релизов PLASTO пока нет или репозиторий закрыт" } else { "выпущенных версий PLASTO пока нет — только бета" }.into()),
        Err(e) => return Err(format!("GitHub: {e}")),
    };
    let mut text = String::new();
    response.into_reader().take(4_000_000).read_to_string(&mut text).map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| format!("ответ GitHub: {e}"))?;
    if !beta {
        return release_info(&answer);
    }
    let mut best: Option<Value> = None;
    for release in answer.as_array().into_iter().flatten().filter(|r| r["draft"] != true) {
        if let Ok(info) = release_info(release) {
            if best.as_ref().is_none_or(|b| newer(info["tag"].as_str().unwrap_or(""), b["tag"].as_str().unwrap_or(""))) {
                best = Some(info);
            }
        }
    }
    best.ok_or_else(|| "релизов PLASTO пока нет".into())
}

/// Версия, exe и его сумма из одного релиза GitHub.
fn release_info(release: &Value) -> Result<Value, String> {
    let tag = release["tag_name"].as_str().filter(|s| !s.is_empty()).ok_or("нет версии релиза")?;
    let asset = release["assets"]
        .as_array()
        .and_then(|items| items.iter().find(|a| a["name"].as_str().is_some_and(|n| n.to_lowercase().starts_with("plasto") && n.to_lowercase().ends_with(".exe"))))
        .ok_or("в релизе нет plasto.exe")?;
    let url = asset["browser_download_url"].as_str().filter(|u| u.starts_with(DOWNLOAD)).ok_or("неверный адрес файла")?;
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit())).ok_or("у GitHub нет контрольной суммы файла")?;
    let size = asset["size"].as_u64().filter(|s| *s > 0 && *s <= MAX_EXE).ok_or("недопустимый размер файла")?;
    Ok(json!({"tag":tag,"notesUrl":release["html_url"],"url":url,"sha256":digest,"size":size,"prerelease":release["prerelease"] == true,"name":release["name"]}))
}

pub fn check(beta: bool) -> Value {
    let current = env!("CARGO_PKG_VERSION");
    match latest(beta) {
        Ok(rel) => {
            let tag = rel["tag"].as_str().unwrap_or("");
            json!({"installed":current,"latest":tag,"hasUpdate":newer(tag,current),"release":rel})
        }
        Err(e) => json!({"installed":current,"error":e}),
    }
}

/// Скачивает новый exe в `root` и сверяет его с GitHub. Возвращает путь.
pub fn download(root: &Path, app: &Host, beta: bool) -> Result<Value, String> {
    let rel = latest(beta)?;
    let tag = rel["tag"].as_str().unwrap_or("");
    if !newer(tag, env!("CARGO_PKG_VERSION")) {
        return Err("установлена последняя версия".into());
    }
    let (url, digest, size) = (rel["url"].as_str().unwrap_or(""), rel["sha256"].as_str().unwrap_or(""), rel["size"].as_u64().unwrap_or(0));
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let target = root.join(format!("plasto-{}.exe", &digest[..16]));
    if target.is_file() && sha256(&target).is_ok_and(|d| d == digest.to_lowercase()) {
        return Ok(json!({"tag":tag,"file":target}));
    }
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    let mut stream = agent()?.get(url).set("User-Agent", "PLASTO-desktop").timeout(Duration::from_secs(180)).call().map_err(|e| format!("загрузка: {e}"))?.into_reader().take(MAX_EXE + 1);
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut buf = [0u8; 32768];
    loop {
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > size {
            return Err("файл больше, чем обещал GitHub".into());
        }
        hash.update(&buf[..n]);
        tmp.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        let _ = app.emit("plasto:app:progress", json!({"received":received,"size":size}));
    }
    if received != size || format!("{:x}", hash.finalize()) != digest.to_lowercase() {
        return Err("размер или SHA-256 не совпадает с GitHub".into());
    }
    tmp.persist(&target).map_err(|e| e.to_string())?;
    Ok(json!({"tag":tag,"file":target}))
}

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut input, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Подменяет работающий exe скачанным и запускает новый. Новый процесс ждёт,
/// пока этот выйдет (`--after <pid>`): одна копия PLASTO за раз. Вызывающий
/// после успеха должен сам выйти.
pub fn install(file: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = fs::remove_file(&old);
    // Работающий exe нельзя перезаписать, но можно переименовать.
    fs::rename(&exe, &old).map_err(|e| format!("не удалось убрать старую версию: {e}"))?;
    if let Err(e) = fs::copy(file, &exe) {
        let _ = fs::rename(&old, &exe);
        return Err(format!("не удалось положить новую версию: {e}"));
    }
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--after").arg(std::process::id().to_string());
    if let Err(e) = cmd.spawn() {
        let _ = fs::remove_file(&exe);
        let _ = fs::rename(&old, &exe);
        return Err(format!("новая версия не запустилась: {e}"));
    }
    let _ = fs::remove_file(file);
    Ok(())
}

/// При запуске: убрать exe прошлой версии, если остался.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(old_path(&exe));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.0.3", "0.0.2-b"));
        assert!(newer("0.1.0", "0.0.9"));
        assert!(newer("0.0.2", "0.0.2-b"));
        assert!(!newer("0.0.2-b", "0.0.2-b"));
        assert!(!newer("v0.0.2", "0.0.2"));
        assert!(!newer("0.0.1", "0.0.2-b"));
        assert!(newer("1.0", "0.9.9"));
    }

    /// Предварительные сборки различаются номером: beta-1 → beta-2 — обновление.
    #[test]
    fn prerelease_numbers_count() {
        assert!(newer("1.0.0-beta-2", "1.0.0-beta-1"));
        assert!(newer("v1.0.0-beta-10", "1.0.0-beta-9"));
        assert!(!newer("1.0.0-beta-1", "1.0.0-beta-2"));
        assert!(!newer("1.0.0-beta-2", "1.0.0-beta-2"));
        assert!(newer("1.0.0", "1.0.0-beta-7"));
        assert!(!newer("1.0.0-beta-7", "1.0.0"));
        assert!(newer("1.0.1-beta-1", "1.0.0"));
        assert!(newer("1.0.0-beta", "1.0.0-alpha"));
        assert!(newer("1.0.0-beta-1", "1.0.0-beta"));
        assert!(!newer("1.0.0-beta-2+zapret.1.10.5", "1.0.0-beta-2"));
    }

    #[test]
    fn old_file_sits_next_to_exe() {
        assert_eq!(old_path(Path::new("C:/a/plasto.exe")), PathBuf::from("C:/a/plasto.old.exe"));
    }
}
