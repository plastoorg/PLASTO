//! Изоляция подбора стратегий по локальному порту.
//!
//! WinDivert не различает процессы, но различает порты. Тестер открывает
//! соединения только с портов `PORTS`; тестовый winws видит лишь их, а
//! основной — всё, кроме них. Так стратегии проверяются, пока обход работает
//! как работал. Проверено вживую: два winws с такими фильтрами работают
//! одновременно и не обрабатывают пакеты друг друга.
//!
//! Фильтр строит сам winws из своих `--wf-*` (спрашиваем через `--wf-save`):
//! повторять его логику — значит разойтись с ней на первой же новой версии.
//! Мы только дописываем к нему условие на порт и отдаём через `--wf-raw`.
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::{Command as Proc, Stdio};
use std::time::{Duration, Instant};

use crate::engines::Command;

/// Ниже динамического диапазона Windows (49152–65535): чужие программы сюда
/// почти не попадают, а попав, лишь останутся без обхода.
pub const PORTS: RangeInclusive<u16> = 41000..=41999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Основной обход: всё, кроме портов тестера.
    Main,
    /// Проверяемая стратегия: только порты тестера.
    Test,
}

impl Side {
    /// Условие на порт. Входящая часть обязательна: autottl и conntrack
    /// смотрят на SYN-ACK сервера. WinDivert не умеет отрицать скобки
    /// (`not (…)` и `!(…)` — ошибка разбора), поэтому «вне диапазона»
    /// записано положительно.
    fn ports(self) -> String {
        let (lo, hi) = (PORTS.start(), PORTS.end());
        match self {
            Side::Test => format!("((outbound and tcp.SrcPort >= {lo} and tcp.SrcPort <= {hi}) or (inbound and tcp.DstPort >= {lo} and tcp.DstPort <= {hi}))"),
            Side::Main => format!("((outbound and (tcp.SrcPort < {lo} or tcp.SrcPort > {hi})) or (inbound and (tcp.DstPort < {lo} or tcp.DstPort > {hi})))"),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Side::Main => "main",
            Side::Test => "test",
        }
    }
}

/// Аргументы, задающие фильтр WinDivert: их целиком заменяет `--wf-raw`.
fn is_filter_arg(arg: &str) -> bool {
    arg.starts_with("--wf-") && !arg.starts_with("--wf-save")
}

/// Готовый фильтр стороны `side` из фильтра, который построил winws.
pub fn combine(generated: &str, side: Side) -> String {
    format!("({generated}) and {}", side.ports())
}

/// Переводит команду winws на фильтр стороны `side`. Ошибка — команду не
/// изолировать (zapret2 без `--wf-save`, Lua-пресет, фильтр не собрался);
/// тогда команда не меняется.
pub fn apply(cmd: &mut Command, side: Side) -> Result<(), String> {
    let (wf, rest): (Vec<String>, Vec<String>) = cmd.args.iter().cloned().partition(|a| is_filter_arg(a));
    if wf.is_empty() {
        return Err("в стратегии нет --wf-*".into());
    }
    let dir = std::env::temp_dir().join("plasto");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let generated = generated(cmd, &wf, &dir.join(format!("wf-{}-winws.txt", side.name())))?;
    let filter = combine(&generated, side);
    compiles(&cmd.cwd.join("WinDivert.dll"), &filter)?;
    // Файлом, а не строкой в аргументах: так фильтр проверен вживую, и
    // кириллица или длина командной строки ему не помеха.
    let file = dir.join(format!("wf-{}.txt", side.name()));
    std::fs::write(&file, &filter).map_err(|e| format!("{}: {e}", file.display()))?;
    cmd.args = std::iter::once(format!("--wf-raw=@{}", file.display())).chain(rest).collect();
    Ok(())
}

/// Фильтр, который winws собирает из своих `--wf-*`, одной строкой.
fn generated(cmd: &Command, wf: &[String], file: &Path) -> Result<String, String> {
    let _ = std::fs::remove_file(file);
    let mut c = Proc::new(&cmd.exe);
    c.args(wf).arg(format!("--wf-save={}", file.display())).current_dir(&cmd.cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    let mut child = c.spawn().map_err(|e| format!("winws --wf-save: {e}"))?;
    let began = Instant::now();
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if began.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("winws --wf-save не завершился".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(file).map_err(|_| "winws не сохранил фильтр (--wf-save)".to_string())?;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Err("winws сохранил пустой фильтр".into());
    }
    Ok(flat)
}

/// Принимает ли WinDivert фильтр. Компилятор фильтров работает без драйвера
/// и прав, так что ошибку видно до запуска winws, а не по его падению.
#[cfg(windows)]
fn compiles(dll: &Path, filter: &str) -> Result<(), String> {
    use std::ffi::{CStr, CString};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // BOOL WinDivertHelperCompileFilter(const char *filter, WINDIVERT_LAYER layer,
    //     char *object, UINT objLen, const char **errorStr, UINT *errorPos)
    type Compile = unsafe extern "system" fn(*const u8, i32, *mut u8, u32, *mut *const u8, *mut u32) -> i32;
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let text = CString::new(filter).map_err(|_| "нулевой байт в фильтре".to_string())?;
    unsafe {
        let module = LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        if module.is_null() {
            return Err(format!("не загрузился {}", dll.display()));
        }
        let result = match GetProcAddress(module, c"WinDivertHelperCompileFilter".as_ptr().cast()) {
            None => Err("в WinDivert.dll нет WinDivertHelperCompileFilter".into()),
            Some(f) => {
                let compile: Compile = std::mem::transmute(f);
                let (mut error, mut pos) = (std::ptr::null(), 0u32);
                // Слой 0 — WINDIVERT_LAYER_NETWORK, как у winws.
                if compile(text.as_ptr().cast(), 0, std::ptr::null_mut(), 0, &mut error, &mut pos) != 0 {
                    Ok(())
                } else {
                    let why = if error.is_null() { "ошибка разбора".into() } else { CStr::from_ptr(error.cast()).to_string_lossy().into_owned() };
                    Err(format!("WinDivert не принял фильтр: {why} (позиция {pos})"))
                }
            }
        };
        FreeLibrary(module);
        result
    }
}

#[cfg(not(windows))]
fn compiles(_dll: &Path, _filter: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn стороны_делят_порты_без_пересечения() {
        let test = combine("tcp", Side::Test);
        let main = combine("tcp", Side::Main);
        assert!(test.starts_with("(tcp) and "));
        assert!(test.contains("tcp.SrcPort >= 41000 and tcp.SrcPort <= 41999"));
        assert!(main.contains("tcp.SrcPort < 41000 or tcp.SrcPort > 41999"));
        assert!(main.contains("tcp.DstPort < 41000 or tcp.DstPort > 41999"));
        // WinDivert не принимает отрицание скобок.
        assert!(!main.contains("not") && !main.contains('!'));
    }

    #[test]
    fn фильтр_заменяет_все_wf_кроме_save() {
        assert!(is_filter_arg("--wf-tcp=80,443"));
        assert!(is_filter_arg("--wf-raw=@x.txt"));
        assert!(is_filter_arg("--wf-l3=ipv4"));
        assert!(!is_filter_arg("--wf-save=x"));
        assert!(!is_filter_arg("--filter-tcp=443"));
    }

    /// Настоящий WinDivert.dll из встроенной сборки: компилятор фильтров
    /// работает без драйвера и прав.
    #[cfg(windows)]
    #[test]
    fn windivert_принимает_обе_стороны_и_отвергает_not() {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/zapret/bin/WinDivert.dll");
        let generated = "!impostor and !loopback and ((outbound and tcp.DstPort == 443) or (inbound and tcp and tcp.SrcPort == 443))";
        compiles(&dll, &combine(generated, Side::Test)).unwrap();
        compiles(&dll, &combine(generated, Side::Main)).unwrap();
        let err = compiles(&dll, &format!("({generated}) and not (tcp.SrcPort == 1)")).unwrap_err();
        assert!(err.contains("WinDivert не принял фильтр"), "{err}");
    }

    #[test]
    fn без_wf_аргументов_не_изолируется() {
        let mut cmd = Command { exe: "winws.exe".into(), args: vec!["--lua".into(), "x.lua".into()], cwd: ".".into(), engine: crate::engines::Engine::V2, game_filter: None, raw: None };
        assert!(apply(&mut cmd, Side::Test).is_err());
        assert_eq!(cmd.args, ["--lua", "x.lua"]);
    }
}
