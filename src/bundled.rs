//! Встроенная сборка zapret: Flowseal zapret-discord-youtube в самом exe.
//!
//! Обход работает и без доступа к GitHub: при первом запуске сборка
//! распаковывается в папку компонентов. Скачанная с GitHub новее — работает
//! она; новый PLASTO с более свежей встроенной сборкой переключается на неё
//! сам. Папку, указанную человеком вручную, PLASTO не трогает.
//!
//! Сборку кладёт в `assets/zapret` скрипт `ci/refresh-zapret.py` (без того,
//! что PLASTO не запускает), в архив её пакует build.rs.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::engines::{self, Engine};

/// Версия встроенной сборки — тег релиза Flowseal.
pub const TAG: &str = env!("ZAPRET_BUNDLED_TAG");
static ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/zapret.zip"));

fn version(tag: &str) -> Vec<u64> {
    tag.trim_start_matches(['v', 'V']).split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|n| n.parse().unwrap_or(0)).collect()
}

/// Новее ли версия `a`, чем `b`: «1.10.4» > «1.9.12».
pub fn newer(a: &str, b: &str) -> bool {
    version(a) > version(b)
}

/// Брать ли встроенную при старте: текущей сборки нет или она испорчена,
/// либо это скачанная PLASTO (с тегом) и она старее встроенной. Папка
/// человека (без тега) остаётся, если она цела.
pub fn should_install(dir_ok: bool, tag: Option<&str>) -> bool {
    !dir_ok || tag.is_some_and(|t| newer(TAG, t))
}

/// Распаковать в `root/bundled-<версия>`; если там уже цела — взять её.
pub fn install(root: &Path) -> Result<PathBuf, String> {
    let name: String = TAG.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let target = root.join(format!("bundled-{name}"));
    if engines::validate(&target, Some(Engine::V1)).ok {
        return Ok(target);
    }
    fs::create_dir_all(root).map_err(|e| format!("папка компонентов: {e}"))?;
    let stage = tempfile::Builder::new().prefix("bundled-").tempdir_in(root).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(ZIP)).map_err(|e| format!("встроенная сборка: {e}"))?;
    for i in 0..zip.len() {
        let mut item = zip.by_index(i).map_err(|e| e.to_string())?;
        let rel = item.enclosed_name().ok_or("небезопасный путь во встроенной сборке")?;
        let dest = stage.path().join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        std::io::copy(&mut item, &mut out).map_err(|e| e.to_string())?;
    }
    let valid = engines::validate(stage.path(), Some(Engine::V1));
    if !valid.ok {
        return Err(valid.reason.unwrap_or_else(|| "встроенная сборка повреждена".into()));
    }
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| format!("испорченная встроенная сборка не удалилась: {e}"))?;
    }
    fs::rename(stage.path(), &target).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Что человек настроил в прежней сборке — в новую: свои списки (`*-user.txt`,
/// туда же пишутся домены ИИ-сервисов) и режим Game Filter.
pub fn carry_user_files(from: &Path, to: &Path) {
    if from == to {
        return;
    }
    if let Ok(entries) = fs::read_dir(from.join("lists")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().to_lowercase().ends_with("-user.txt") {
                let _ = fs::create_dir_all(to.join("lists"));
                let _ = fs::copy(entry.path(), to.join("lists").join(&name));
            }
        }
    }
    let flag = Path::new("utils").join("game_filter.enabled");
    if from.join(&flag).is_file() {
        let _ = fs::create_dir_all(to.join("utils"));
        let _ = fs::copy(from.join(&flag), to.join(&flag));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn версии_сравниваются_по_числам() {
        assert!(newer("1.10.4", "1.9.12"));
        assert!(!newer("1.10.4", "1.10.4"));
        assert!(!newer("1.10.3", "1.10.4"));
    }

    #[test]
    fn когда_брать_встроенную() {
        assert!(should_install(false, None), "сборки нет");
        assert!(should_install(false, Some("9.9.9")), "скачанная испорчена");
        assert!(!should_install(true, None), "папка человека не трогается");
        assert!(should_install(true, Some("0.0.1")), "скачанная старее");
        assert!(!should_install(true, Some(TAG)), "та же версия");
        assert!(!should_install(true, Some("999.0.0")), "скачанная новее");
    }

    /// Встроенная сборка распаковывается целой, все её стратегии собираются
    /// в команду запуска: CI ловит смену формата у Flowseal до релиза.
    #[test]
    fn встроенная_сборка_цела_и_все_стратегии_собираются() {
        let root = tempfile::tempdir().unwrap();
        let dir = install(root.path()).unwrap();
        let valid = engines::validate(&dir, Some(Engine::V1));
        assert!(valid.ok, "{:?}", valid.reason);
        assert!(valid.presets.len() >= 10, "стратегий: {}", valid.presets.len());
        for name in ["bin/winws.exe", "bin/cygwin1.dll", "bin/WinDivert.dll", "bin/WinDivert64.sys"] {
            assert!(dir.join(name).is_file(), "{name}");
        }
        assert!(!dir.join("service.bat").exists() && !dir.join("utils").exists());
        crate::strategy::ensure_user_lists(&dir);
        for preset in &valid.presets {
            let plan = engines::build_command(&dir, &preset.file, &Default::default());
            assert!(plan.is_ok(), "{}: {:?}", preset.file, plan.err());
        }
        // Повторно — та же папка, без распаковки.
        assert_eq!(install(root.path()).unwrap(), dir);
    }

    #[test]
    fn свои_списки_переезжают() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::create_dir_all(a.path().join("lists")).unwrap();
        fs::write(a.path().join("lists/list-general-user.txt"), "chatgpt.com\n").unwrap();
        fs::write(a.path().join("lists/list-general.txt"), "из сборки\n").unwrap();
        crate::strategy::write_game_mode(a.path(), crate::strategy::GameMode::Udp);
        carry_user_files(a.path(), b.path());
        assert_eq!(fs::read_to_string(b.path().join("lists/list-general-user.txt")).unwrap(), "chatgpt.com\n");
        assert!(!b.path().join("lists/list-general.txt").exists(), "списки сборки не трогаются");
        assert_eq!(crate::strategy::read_game_mode(b.path()), crate::strategy::GameMode::Udp);
    }
}
