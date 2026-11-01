//! Абстракция движка обхода: zapret1 и zapret2 — разные продукты.
//!
//! Перенос `engines.js` без изменения поведения.
//!
//! zapret1 (bol-van/zapret + Flowseal/zapret-discord-youtube)
//!   · бинарь bin/winws.exe, стратегии — general*.bat;
//!   · аргументы собираются из переменных, разбирает `strategy`.
//!
//! zapret2 (bol-van/zapret2)
//!   · бинарь bin/winws2.exe, стратегии — на Lua или .bat с winws2;
//!   · в движок встроен zapret-auto (автоперебор и детект сбоев).
//!
//! Остальное приложение не должно знать, какой движок под ним: оно спрашивает
//! «дай список пресетов» и «собери команду запуска».

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex_lite::Regex;
use serde_json::{json, Value};

use crate::strategy::{self, GameMode, ParseOpts};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    V1,
    V2,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::V1 => "zapret1",
            Engine::V2 => "zapret2",
        }
    }

    pub fn parse(s: &str) -> Option<Engine> {
        match s {
            "zapret1" => Some(Engine::V1),
            "zapret2" => Some(Engine::V2),
            _ => None,
        }
    }

    pub fn exe_name(self) -> &'static str {
        match self {
            Engine::V1 => "winws.exe",
            Engine::V2 => "winws2.exe",
        }
    }

    fn exe_path(self, dir: &Path) -> PathBuf {
        dir.join("bin").join(self.exe_name())
    }
}

/* ---------------------------------------------------------------------
   Определение движка по содержимому папки
   --------------------------------------------------------------------- */

#[derive(Clone, Debug)]
pub struct Detected {
    pub engine: Option<Engine>,
    pub exe: Option<PathBuf>,
    pub reason: String,
}

pub fn detect_engine(dir: &Path) -> Detected {
    if dir.as_os_str().is_empty() {
        return Detected { engine: None, exe: None, reason: "папка не выбрана".into() };
    }
    // Сначала zapret2: если лежат оба, новый движок приоритетнее.
    for (engine, why) in [(Engine::V2, "найден bin/winws2.exe"), (Engine::V1, "найден bin/winws.exe")] {
        let exe = engine.exe_path(dir);
        if exe.exists() {
            return Detected { engine: Some(engine), exe: Some(exe), reason: why.into() };
        }
    }
    Detected { engine: None, exe: None, reason: "не найден ни winws.exe, ни winws2.exe".into() }
}

/* ---------------------------------------------------------------------
   Пресеты
   --------------------------------------------------------------------- */

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub file: String,
    pub label: String,
    pub group: &'static str,
    pub engine: Engine,
    pub kind: &'static str,
}

impl Preset {
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.file, "label": self.label, "group": self.group,
            "engine": self.engine.as_str(), "kind": self.kind
        })
    }
}

#[derive(Clone, Debug)]
pub struct Validation {
    pub ok: bool,
    pub reason: Option<String>,
    pub engine: Option<Engine>,
    pub exe: Option<PathBuf>,
    pub presets: Vec<Preset>,
}

impl Validation {
    fn fail(reason: impl Into<String>, engine: Option<Engine>) -> Self {
        Validation { ok: false, reason: Some(reason.into()), engine, exe: None, presets: Vec::new() }
    }

    /// Та же форма, что отдавал JS: `strategies` — прежнее имя поля `presets`.
    pub fn to_json(&self) -> Value {
        if !self.ok {
            let mut o = json!({ "ok": false, "reason": self.reason });
            if let Some(e) = self.engine {
                o["engine"] = json!(e.as_str());
            }
            return o;
        }
        let presets: Vec<Value> = self.presets.iter().map(Preset::to_json).collect();
        json!({
            "ok": true,
            "engine": self.engine.map(Engine::as_str),
            "exe": self.exe.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "presets": presets.clone(),
            "strategies": presets
        })
    }
}

/// Полная проверка папки под конкретный (или автоопределённый) движок.
pub fn validate(dir: &Path, prefer: Option<Engine>) -> Validation {
    if dir.as_os_str().is_empty() {
        return Validation::fail("папка не выбрана", None);
    }
    match fs::metadata(dir) {
        Err(_) => return Validation::fail("папка не найдена", None),
        Ok(m) if !m.is_dir() => return Validation::fail("это не папка", None),
        Ok(_) => {}
    }

    let det = detect_engine(dir);

    // Пользователь явно выбрал движок, которого в папке нет.
    if let (Some(want), Some(found)) = (prefer, det.engine) {
        if want != found && !want.exe_path(dir).exists() {
            return Validation::fail(
                format!("выбран {}, но в папке нет bin/{}", want.as_str(), want.exe_name()),
                det.engine,
            );
        }
    }

    let engine = match prefer {
        Some(p) if p.exe_path(dir).exists() => Some(p),
        _ => det.engine,
    };
    let Some(engine) = engine else {
        return Validation::fail(det.reason, None);
    };

    let presets = list_presets(dir, engine);
    if presets.is_empty() {
        let why = match engine {
            Engine::V2 => "не найдено ни одного пресета zapret2 (.bat с winws2 или .lua)",
            Engine::V1 => "не найдено ни одного general*.bat",
        };
        return Validation::fail(why, Some(engine));
    }

    Validation {
        ok: true,
        reason: None,
        engine: Some(engine),
        exe: Some(engine.exe_path(dir)),
        presets,
    }
}

pub fn list_presets(dir: &Path, engine: Engine) -> Vec<Preset> {
    match engine {
        Engine::V2 => list_v2(dir),
        Engine::V1 => list_v1(dir),
    }
}

static RE_GENERAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^general.*\.bat$").unwrap());
static RE_PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([^)]+)\)").unwrap());
static RE_BAT_EXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(bat|cmd)$").unwrap());
static RE_LUA_EXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.lua$").unwrap());
static RE_SKIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(service|blockcheck|install|uninstall)").unwrap());
/// Упоминает ли батник `winws2.exe` (без учёта регистра). Файл читается как
/// байты: кодировка у .bat бывает любая, а имя exe всегда в ASCII.
fn mentions_winws2(bytes: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"winws2.exe";
    bytes.windows(NEEDLE.len()).any(|w| w.eq_ignore_ascii_case(NEEDLE))
}

static RE_DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9]+").unwrap());

fn file_names(dir: &Path) -> Vec<String> {
    match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(_) => Vec::new(),
    }
}

/// zapret1: general*.bat в корне сборки.
fn list_v1(dir: &Path) -> Vec<Preset> {
    let mut names: Vec<String> = file_names(dir).into_iter().filter(|f| RE_GENERAL.is_match(f)).collect();
    names.sort_by(|a, b| by_preset_order(a, b));

    names
        .into_iter()
        .map(|file| {
            let label = RE_PAREN
                .captures(&file)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "DEFAULT".into());
            let up = file.to_uppercase();
            let group = if up.contains("FAKE") {
                "FAKE"
            } else if up.contains("ALT") {
                "ALT"
            } else {
                "BASE"
            };
            Preset { file, label, group, engine: Engine::V1, kind: "bat" }
        })
        .collect()
}

/// zapret2: набор пресетов ещё не устоялся — у Flowseal сборки под zapret2
/// нет. Поддерживаем два варианта, которые встречаются:
///   · .bat, внутри которого вызывается winws2.exe;
///   · .lua в папке strategies/ presets/ lua/ (запускается через --lua).
fn list_v2(dir: &Path) -> Vec<Preset> {
    let mut out: Vec<Preset> = Vec::new();

    for file in file_names(dir) {
        if !RE_BAT_EXT.is_match(&file) || RE_SKIP.is_match(&file) {
            continue;
        }
        let Ok(bytes) = fs::read(dir.join(&file)) else { continue };
        if !mentions_winws2(&bytes) {
            continue;
        }
        let label = RE_PAREN
            .captures(&file)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| RE_BAT_EXT.replace(&file, "").into_owned());
        out.push(Preset { file, label, group: "BAT", engine: Engine::V2, kind: "bat" });
    }

    for sub in ["strategies", "presets", "lua"] {
        let d = dir.join(sub);
        if !d.is_dir() {
            continue;
        }
        for file in file_names(&d) {
            if !RE_LUA_EXT.is_match(&file) {
                continue;
            }
            out.push(Preset {
                file: Path::new(sub).join(&file).to_string_lossy().into_owned(),
                label: RE_LUA_EXT.replace(&file, "").into_owned(),
                group: "LUA",
                engine: Engine::V2,
                kind: "lua",
            });
        }
    }

    out.sort_by(|a, b| by_preset_order(&a.file, &b.file));
    out
}

/* ---------------------------------------------------------------------
   Порядок пресетов
   --------------------------------------------------------------------- */

/// Порядок как у `localeCompare(…, 'en')` (ICU/CLDR root) для имён файлов:
/// пробел и пунктуация < цифры < буквы; регистр различается только когда
/// всё остальное совпало, и строчная идёт раньше заглавной; префикс короче.
/// Таблица пунктуации сверена с Node (ICU 77) по всем печатным ASCII.
/// Не-ASCII символы упорядочены по коду после букв — для имён батников этого
/// достаточно, полноценный ICU-коллатор ради этого тянуть незачем.
const PUNCT_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary(c: char) -> (u8, u32) {
    if let Some(i) = PUNCT_ORDER.find(c) {
        return (0, i as u32);
    }
    if c.is_ascii_digit() {
        return (1, c as u32);
    }
    if c.is_ascii_alphabetic() {
        return (2, c.to_ascii_lowercase() as u32);
    }
    (3, c as u32)
}

pub fn locale_cmp(a: &str, b: &str) -> Ordering {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());

    for (x, y) in ac.iter().zip(bc.iter()) {
        let o = primary(*x).cmp(&primary(*y));
        if o != Ordering::Equal {
            return o;
        }
    }
    let o = ac.len().cmp(&bc.len());
    if o != Ordering::Equal {
        return o;
    }
    // Тертичный уровень: первое отличие только в регистре — строчная раньше.
    for (x, y) in ac.iter().zip(bc.iter()) {
        if x != y {
            return if x.is_ascii_lowercase() { Ordering::Less } else { Ordering::Greater };
        }
    }
    Ordering::Equal
}

/// DEFAULT первым, потом ALT по числам, потом FAKE. Иначе ALT10 встаёт перед ALT2.
pub fn by_preset_order(a: &str, b: &str) -> Ordering {
    fn rank(f: &str) -> u8 {
        if f.contains('(') {
            if f.to_uppercase().contains("FAKE") {
                2
            } else {
                1
            }
        } else {
            0
        }
    }
    fn num(f: &str) -> f64 {
        RE_DIGITS.find(f).and_then(|m| m.as_str().parse::<f64>().ok()).unwrap_or(0.0)
    }
    fn stripped(f: &str) -> String {
        RE_DIGITS.replace(f, "").into_owned() // как JS: убирается только первая группа цифр
    }

    rank(a)
        .cmp(&rank(b))
        .then_with(|| locale_cmp(&stripped(a), &stripped(b)))
        .then_with(|| num(a).partial_cmp(&num(b)).unwrap_or(Ordering::Equal))
}

/* ---------------------------------------------------------------------
   Сборка команды запуска
   --------------------------------------------------------------------- */

#[derive(Clone, Debug, Default)]
pub struct BuildOpts {
    pub parse: ParseOpts,
    /// Явный движок; иначе определяется по папке.
    pub engine: Option<Engine>,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub engine: Engine,
    pub game_filter: Option<GameMode>,
    pub raw: Option<String>,
}

pub fn build_command(dir: &Path, preset: &str, opts: &BuildOpts) -> Result<Command, String> {
    let engine = opts
        .engine
        .or_else(|| detect_engine(dir).engine)
        .ok_or_else(|| "движок не определён".to_string())?;

    if engine == Engine::V1 {
        let p = strategy::parse_strategy(&dir.join(preset), &opts.parse)?;
        return Ok(Command {
            exe: p.exe,
            args: p.args,
            cwd: p.cwd,
            engine,
            game_filter: Some(p.game_filter),
            raw: Some(p.raw),
        });
    }

    // zapret2
    let exe = Engine::V2.exe_path(dir);
    let cwd = dir.join("bin");

    if RE_LUA_EXT.is_match(preset) {
        // Lua-пресет передаётся движку напрямую: он сам исполняет execution plan.
        return Ok(Command {
            exe,
            args: vec!["--lua".into(), dir.join(preset).to_string_lossy().into_owned()],
            cwd,
            engine,
            game_filter: None,
            raw: None,
        });
    }

    // .bat под winws2 разбираем тем же парсером: формат переменных совпадает,
    // отличается только имя бинаря.
    let parse = ParseOpts { exe_name: Some("winws2.exe".into()), ..opts.parse.clone() };
    let p = strategy::parse_strategy(&dir.join(preset), &parse)?;
    Ok(Command { exe, args: p.args, cwd, engine, game_filter: Some(p.game_filter), raw: Some(p.raw) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct Dir(tempfile::TempDir);

    impl Dir {
        fn new() -> Self {
            Dir(tempfile::tempdir().unwrap())
        }
        fn p(&self) -> &Path {
            self.0.path()
        }
        fn file(&self, rel: &str, text: &str) -> &Self {
            let f = self.p().join(rel);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, text).unwrap();
            self
        }
        fn v1(&self) -> &Self {
            self.file("bin/winws.exe", "")
        }
        fn v2(&self) -> &Self {
            self.file("bin/winws2.exe", "")
        }
    }

    fn files(v: &[Preset]) -> Vec<&str> {
        v.iter().map(|p| p.file.as_str()).collect()
    }

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| by_preset_order(a, b));
        v
    }

    /* ---------------- определение ---------------- */

    #[test]
    fn определение_пустой_путь_и_пустая_папка() {
        assert_eq!(detect_engine(Path::new("")).reason, "папка не выбрана");
        let d = Dir::new();
        let r = detect_engine(d.p());
        assert!(r.engine.is_none() && r.exe.is_none());
        assert_eq!(r.reason, "не найден ни winws.exe, ни winws2.exe");
    }

    #[test]
    fn определение_v1_v2_и_приоритет_нового_движка() {
        let d = Dir::new();
        d.v1();
        assert_eq!(detect_engine(d.p()).engine, Some(Engine::V1));
        d.v2();
        let r = detect_engine(d.p());
        assert_eq!(r.engine, Some(Engine::V2));
        assert!(r.exe.unwrap().ends_with(Path::new("bin").join("winws2.exe")));
    }

    /* ---------------- validate ---------------- */

    #[test]
    fn проверка_папка_не_выбрана_не_найдена_и_это_файл() {
        assert_eq!(validate(Path::new(""), None).reason.unwrap(), "папка не выбрана");
        let d = Dir::new();
        assert_eq!(validate(&d.p().join("нет"), None).reason.unwrap(), "папка не найдена");
        d.file("файл.txt", "x");
        assert_eq!(validate(&d.p().join("файл.txt"), None).reason.unwrap(), "это не папка");
    }

    #[test]
    fn проверка_без_движка_и_без_пресетов() {
        let d = Dir::new();
        assert_eq!(validate(d.p(), None).reason.unwrap(), "не найден ни winws.exe, ни winws2.exe");

        d.v1();
        let r = validate(d.p(), None);
        assert!(!r.ok);
        assert_eq!(r.engine, Some(Engine::V1));
        assert_eq!(r.reason.unwrap(), "не найдено ни одного general*.bat");

        let d2 = Dir::new();
        d2.v2();
        let r2 = validate(d2.p(), None);
        assert_eq!(r2.engine, Some(Engine::V2));
        assert!(r2.reason.unwrap().contains("zapret2"));
    }

    #[test]
    fn проверка_явно_выбранного_движка_которого_нет() {
        let d = Dir::new();
        d.v1().file("general.bat", "");
        let r = validate(d.p(), Some(Engine::V2));
        assert!(!r.ok);
        assert_eq!(r.engine, Some(Engine::V1));
        assert_eq!(r.reason.unwrap(), "выбран zapret2, но в папке нет bin/winws2.exe");
    }

    #[test]
    fn проверка_явный_выбор_старого_движка_при_двух_бинарях() {
        let d = Dir::new();
        d.v1().v2().file("general.bat", "").file("z2.bat", "winws2.exe --x");
        // авто → новый
        assert_eq!(validate(d.p(), None).engine, Some(Engine::V2));
        // явно v1 → v1, и пресеты v1
        let r = validate(d.p(), Some(Engine::V1));
        assert!(r.ok);
        assert_eq!(r.engine, Some(Engine::V1));
        assert_eq!(files(&r.presets), ["general.bat"]);
    }

    #[test]
    fn проверка_успех_отдаёт_и_presets_и_прежнее_имя_strategies() {
        let d = Dir::new();
        d.v1().file("general.bat", "").file("general (ALT).bat", "");
        let j = validate(d.p(), None).to_json();
        assert_eq!(j["ok"], json!(true));
        assert_eq!(j["engine"], json!("zapret1"));
        assert_eq!(j["presets"], j["strategies"]);
        assert_eq!(j["presets"].as_array().unwrap().len(), 2);
    }

    /* ---------------- список v1 ---------------- */

    #[test]
    fn список_v1_метки_и_группы() {
        let d = Dir::new();
        d.v1()
            .file("general.bat", "")
            .file("general (ALT).bat", "")
            .file("general (FAKE TLS AUTO).bat", "")
            .file("general (SIMPLE FAKE ALT).bat", "")
            .file("service.bat", "")
            .file("README.txt", "");
        let v = list_presets(d.p(), Engine::V1);
        let by: std::collections::HashMap<_, _> = v.iter().map(|p| (p.file.as_str(), p)).collect();

        assert_eq!(by.len(), 4, "service.bat и README не пресеты");
        assert_eq!((by["general.bat"].label.as_str(), by["general.bat"].group), ("DEFAULT", "BASE"));
        assert_eq!((by["general (ALT).bat"].label.as_str(), by["general (ALT).bat"].group), ("ALT", "ALT"));
        assert_eq!(by["general (FAKE TLS AUTO).bat"].group, "FAKE");
        // FAKE важнее ALT, если в имени есть оба
        assert_eq!(by["general (SIMPLE FAKE ALT).bat"].group, "FAKE");
        assert!(v.iter().all(|p| p.kind == "bat" && p.engine == Engine::V1));
    }

    #[test]
    fn список_v1_регистр_расширения_не_важен() {
        let d = Dir::new();
        d.v1().file("GENERAL (ALT).BAT", "").file("General.Bat", "");
        assert_eq!(list_presets(d.p(), Engine::V1).len(), 2);
    }

    #[test]
    fn порядок_default_первым_alt_по_числам_fake_последним() {
        let got = sorted(&[
            "general (ALT10).bat",
            "general (FAKE TLS).bat",
            "general (ALT2).bat",
            "general.bat",
            "general (ALT).bat",
            "general (ALT3).bat",
        ]);
        assert_eq!(
            got,
            [
                "general.bat",
                "general (ALT).bat",
                "general (ALT2).bat",
                "general (ALT3).bat",
                "general (ALT10).bat",
                "general (FAKE TLS).bat"
            ]
        );
    }

    #[test]
    fn порядок_alt10_не_обгоняет_alt2_как_при_простой_сортировке_строк() {
        let got = sorted(&["general (ALT10).bat", "general (ALT2).bat"]);
        assert_eq!(got, ["general (ALT2).bat", "general (ALT10).bat"]);
    }

    /* ---------------- ICU-порядок ---------------- */

    #[test]
    fn сравнение_как_у_icu_регистр_пунктуация_префикс() {
        use Ordering::*;
        assert_eq!(locale_cmp("a", "A"), Less, "строчная раньше заглавной");
        assert_eq!(locale_cmp("A", "a"), Greater);
        assert_eq!(locale_cmp("a", "AB"), Less, "регистр слабее длины");
        assert_eq!(locale_cmp("B", "a"), Greater, "буква важнее регистра");
        assert_eq!(locale_cmp("ab", "abc"), Less);
        assert_eq!(locale_cmp("a b", "ab"), Less, "пробел раньше буквы");
        assert_eq!(locale_cmp("a_b", "ab"), Less);
        assert_eq!(locale_cmp("a1", "aa"), Less, "цифра раньше буквы");
        assert_eq!(locale_cmp("(", ")"), Less);
        assert_eq!(locale_cmp(".", "("), Less, "точка раньше скобки, как в CLDR, а не как в ASCII");
        assert_eq!(locale_cmp("same", "same"), Equal);
    }

    #[test]
    fn таблица_пунктуации_без_дублей() {
        let set: HashSet<char> = PUNCT_ORDER.chars().collect();
        assert_eq!(set.len(), PUNCT_ORDER.chars().count());
    }

    /* ---------------- список v2 ---------------- */

    #[test]
    fn список_v2_bat_только_с_winws2_и_без_служебных() {
        let d = Dir::new();
        d.v2()
            .file("plan (A).bat", r#""%BIN%winws2.exe" --x"#)
            .file("plan (B).cmd", "START WINWS2.EXE --y")
            .file("old.bat", r#""%BIN%winws.exe" --x"#)
            .file("service.bat", "winws2.exe")
            .file("blockcheck.bat", "winws2.exe")
            .file("install-me.bat", "winws2.exe")
            .file("Uninstall.cmd", "winws2.exe")
            .file("plain.bat", "winws2.exe");
        let v = list_presets(d.p(), Engine::V2);
        let mut names: Vec<&str> = files(&v);
        names.sort();
        assert_eq!(names, ["plain.bat", "plan (A).bat", "plan (B).cmd"]);
        let by: std::collections::HashMap<_, _> = v.iter().map(|p| (p.file.as_str(), p)).collect();
        assert_eq!(by["plan (A).bat"].label, "A");
        assert_eq!(by["plain.bat"].label, "plain", "без скобок — имя без расширения");
        assert!(v.iter().all(|p| p.group == "BAT" && p.kind == "bat"));
    }

    #[test]
    fn список_v2_lua_из_трёх_папок() {
        let d = Dir::new();
        d.v2()
            .file("strategies/a.lua", "")
            .file("presets/b.LUA", "")
            .file("lua/c.lua", "")
            .file("other/d.lua", "")
            .file("strategies/readme.txt", "");
        let v = list_presets(d.p(), Engine::V2);
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(|p| p.group == "LUA" && p.kind == "lua"));
        let labels: HashSet<&str> = v.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, HashSet::from(["a", "b", "c"]));
        let sep = std::path::MAIN_SEPARATOR;
        assert!(v.iter().any(|p| p.file == format!("strategies{sep}a.lua")));
    }

    /* ---------------- build_command ---------------- */

    fn opts_off() -> BuildOpts {
        BuildOpts { parse: ParseOpts { game_filter: Some(GameMode::Off), exe_name: None }, engine: None }
    }

    #[test]
    fn команда_v1_разбирается_через_strategy() {
        let d = Dir::new();
        d.v1().file(
            "general.bat",
            "set BIN=%~dp0bin\\\r\n\"%BIN%winws.exe\" --wf-tcp=443,%GameFilterTCP%\r\n",
        );
        let c = build_command(d.p(), "general.bat", &opts_off()).unwrap();
        assert_eq!(c.engine, Engine::V1);
        assert_eq!(c.args, ["--wf-tcp=443,12"]);
        assert_eq!(c.cwd, d.p().join("bin"));
        assert_eq!(c.game_filter, Some(GameMode::Off));
    }

    #[test]
    fn команда_v2_lua_передаётся_движку_напрямую() {
        let d = Dir::new();
        d.v2().file("strategies/a.lua", "");
        let c = build_command(d.p(), "strategies/a.lua", &opts_off()).unwrap();
        assert_eq!(c.engine, Engine::V2);
        assert!(c.exe.ends_with(Path::new("bin").join("winws2.exe")));
        assert_eq!(c.args[0], "--lua");
        assert!(c.args[1].replace('\\', "/").ends_with("strategies/a.lua"));
        assert_eq!(c.game_filter, None);
    }

    #[test]
    fn команда_v2_bat_ищет_winws2_и_подставляет_game_filter() {
        let d = Dir::new();
        d.v2().file(
            "plan.bat",
            "set BIN=%~dp0bin\\\r\n\"%BIN%winws2.exe\" --lua-init=@x.lua --wf-udp=%GameFilterUDP%\r\n",
        );
        let c = build_command(d.p(), "plan.bat", &opts_off()).unwrap();
        assert_eq!(c.engine, Engine::V2);
        assert!(c.exe.ends_with(Path::new("bin").join("winws2.exe")));
        assert_eq!(c.args, ["--lua-init=@x.lua", "--wf-udp=12"]);
    }

    #[test]
    fn команда_без_движка_падает_внятно_а_явный_движок_важнее_папки() {
        let d = Dir::new();
        d.file("general.bat", "\"%BIN%winws.exe\" --x");
        let e = build_command(d.p(), "general.bat", &opts_off()).unwrap_err();
        assert_eq!(e, "движок не определён");

        // папка пустая, но движок задан явно — разбор всё равно идёт
        let o = BuildOpts { engine: Some(Engine::V1), ..opts_off() };
        let c = build_command(d.p(), "general.bat", &o).unwrap();
        assert_eq!(c.args, ["--x"]);
    }
}

/// Сравнение с JS-эталоном: порядок пресетов и validate() на случайных папках.
///
///   node scripts/gen-engines-corpus.js %TEMP%\plasto-engines
///   set PLASTO_CORPUS_ENGINES=%TEMP%\plasto-engines
///   cargo test -- --ignored сравнение_с_js_движки
///
/// Живёт рядом с портом, пока engines.js не удалён.
#[cfg(test)]
mod differential {
    use super::*;

    /// Пути в JSON сравниваем без различий слэшей.
    fn norm(v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(s.replace('\\', "/")),
            Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), norm(x))).collect()),
            other => other.clone(),
        }
    }

    #[test]
    #[ignore = "нужен корпус: PLASTO_CORPUS_ENGINES (scripts/gen-engines-corpus.js)"]
    fn сравнение_с_js_движки() {
        let dir = PathBuf::from(std::env::var("PLASTO_CORPUS_ENGINES").expect("задайте PLASTO_CORPUS_ENGINES"));
        let text = fs::read_to_string(dir.join("engines-expected.json")).expect("engines-expected.json");
        let exp: Value = serde_json::from_str(&text).unwrap();
        let mut bad: Vec<String> = Vec::new();

        // 1. порядок
        let sort = exp["sort"].as_array().unwrap();
        for (i, c) in sort.iter().enumerate() {
            let names: Vec<String> = c["names"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().into()).collect();
            let want: Vec<String> = c["sorted"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().into()).collect();
            let mut got = names.clone();
            got.sort_by(|a, b| by_preset_order(a, b));
            if got != want {
                bad.push(format!("сортировка #{i}\n   js:   {want:?}\n   rust: {got:?}"));
            }
        }

        // 2. validate на папках
        let dirs = exp["dirs"].as_array().unwrap();
        let mut checks = 0;
        for d in dirs {
            let path = PathBuf::from(d["path"].as_str().unwrap());
            for (key, prefer) in [("auto", None), ("zapret1", Some(Engine::V1)), ("zapret2", Some(Engine::V2))] {
                checks += 1;
                let got = norm(&validate(&path, prefer).to_json());
                let want = norm(&d["results"][key]);
                if got != want {
                    bad.push(format!("{} / {key}\n   js:   {want}\n   rust: {got}", path.display()));
                }
            }
        }

        println!("сортировка: {} списков; validate: {checks} проверок в {} папках", sort.len(), dirs.len());
        assert!(
            bad.is_empty(),
            "{} расхождений:\n{}",
            bad.len(),
            bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}
