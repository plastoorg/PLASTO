//! Загрузка опубликованных компонентов во временную папку с проверкой архива.
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use crate::host::Host;

const API: &str = "https://api.github.com/repos/Flowseal/zapret-discord-youtube/releases/latest";
const MAX_ARCHIVE: u64 = 100 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 500 * 1024 * 1024;

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| format!("TLS: {e}"))?;
    Ok(ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).build())
}

pub fn latest() -> Result<Value, String> {
    let response = agent()?.get(API).set("User-Agent", "PLASTO-desktop").set("Accept", "application/vnd.github+json").timeout(std::time::Duration::from_secs(30)).call().map_err(|e| format!("GitHub: {e}"))?;
    let mut data = String::new(); response.into_reader().take(2_000_000).read_to_string(&mut data).map_err(|e| e.to_string())?;
    let release: Value = serde_json::from_str(&data).map_err(|e| format!("ответ GitHub: {e}"))?;
    let tag = release["tag_name"].as_str().filter(|s| !s.is_empty()).ok_or("нет версии релиза")?;
    let asset = release["assets"].as_array().and_then(|items| items.iter().find(|a| a["name"].as_str().is_some_and(|s| s.to_lowercase().starts_with("zapret-discord-youtube") && s.to_lowercase().ends_with(".zip")))).ok_or("в релизе нет архива компонентов")?;
    let url = asset["browser_download_url"].as_str().filter(|s| s.starts_with("https://github.com/Flowseal/zapret-discord-youtube/releases/download/")).ok_or("неверный адрес архива")?;
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit())).ok_or("у GitHub нет контрольной суммы архива")?;
    let size = asset["size"].as_u64().filter(|s| *s > 0 && *s <= MAX_ARCHIVE).ok_or("недопустимый размер архива")?;
    Ok(json!({"tag":tag,"name":release["name"],"published":release["published_at"],"notesUrl":release["html_url"],"asset":{"name":asset["name"],"url":url,"size":size,"sha256":digest}}))
}

pub fn install(root: &Path, app: &Host) -> Result<Value, String> {
    let rel = latest()?;
    let tag = rel["tag"].as_str().ok_or("нет версии")?;
    let url = rel["asset"]["url"].as_str().ok_or("нет адреса")?;
    let digest = rel["asset"]["sha256"].as_str().ok_or("нет суммы")?;
    let size = rel["asset"]["size"].as_u64().ok_or("нет размера")?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new().prefix("download-").tempdir_in(root).map_err(|e| e.to_string())?;
    let archive = stage.path().join("release.zip");
    let mut file = fs::File::create(&archive).map_err(|e| e.to_string())?;
    let response = agent()?.get(url).set("User-Agent", "PLASTO-desktop").timeout(std::time::Duration::from_secs(90)).call().map_err(|e| format!("загрузка: {e}"))?;
    let mut reader = response.into_reader().take(MAX_ARCHIVE + 1);
    let mut hash = Sha256::new(); let mut received = 0u64; let mut buf = [0u8; 32768];
    loop { let n=reader.read(&mut buf).map_err(|e| e.to_string())?; if n==0 { break; } received+=n as u64; if received>MAX_ARCHIVE { return Err("архив слишком велик".into()); } hash.update(&buf[..n]); file.write_all(&buf[..n]).map_err(|e| e.to_string())?; let _=app.emit("plasto:zapret:progress",json!({"stage":"download","detail":{"name":rel["asset"]["name"],"size":size,"received":received}})); }
    if received != size || format!("{:x}",hash.finalize()) != digest.to_lowercase() { return Err("размер или SHA-256 архива не совпадает с GitHub".into()); }
    drop(file);
    let extracted=stage.path().join("files"); fs::create_dir(&extracted).map_err(|e| e.to_string())?;
    let _=app.emit("plasto:zapret:progress",json!({"stage":"extract","detail":null}));
    let mut zip=zip::ZipArchive::new(fs::File::open(&archive).map_err(|e|e.to_string())?).map_err(|e|format!("архив: {e}"))?;
    let mut total=0u64;
    for i in 0..zip.len() { let mut item=zip.by_index(i).map_err(|e|e.to_string())?; let path=item.enclosed_name().ok_or("небезопасный путь в архиве")?.to_path_buf(); total=total.checked_add(item.size()).ok_or("архив слишком велик")?; if total>MAX_EXTRACTED { return Err("распакованный архив слишком велик".into()); } if item.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) { return Err("ссылка в архиве недопустима".into()); } let dest=extracted.join(path); if item.is_dir() { fs::create_dir_all(&dest).map_err(|e|e.to_string())?; } else { if let Some(p)=dest.parent() { fs::create_dir_all(p).map_err(|e|e.to_string())?; } let mut out=fs::File::create(dest).map_err(|e|e.to_string())?; std::io::copy(&mut item,&mut out).map_err(|e|e.to_string())?; } }
    let inner = if extracted.join("bin/winws.exe").is_file() { extracted.clone() } else { let entries=fs::read_dir(&extracted).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?; if entries.len()!=1 || !entries[0].path().is_dir() { return Err("в архиве не найдена папка компонентов".into()); } entries[0].path() };
    let valid=crate::engines::validate(&inner, Some(crate::engines::Engine::V1)); if !valid.ok { return Err(valid.reason.unwrap_or("компоненты повреждены".into())); }
    // Имя основано на проверенной сумме, а не на произвольном имени версии из сети.
    let target: PathBuf = root.join(format!("release-{}", &digest[..16]));
    if target.exists() { let existing=crate::engines::validate(&target, Some(crate::engines::Engine::V1)); if !existing.ok { return Err("папка этой версии уже существует, но повреждена".into()); } } else { fs::rename(&inner,&target).map_err(|e|e.to_string())?; }
    let valid=crate::engines::validate(&target,Some(crate::engines::Engine::V1));
    Ok(json!({"ok":true,"tag":tag,"dir":target,"dirStatus":valid.to_json(),"strategies":valid.presets.iter().map(|p|p.to_json()).collect::<Vec<_>>() }))
}
