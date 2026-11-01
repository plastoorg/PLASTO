//! Разбор .bat-стратегий zapret в готовый argv для winws.exe.
//!
//! Перенос `strategy.js` без изменения поведения. Смысл прежний: не запускать
//! cmd.exe с батником (окно, чужой процесс, не понять, что произошло), а
//! поднимать winws.exe самим — скрыто, с перехватом вывода и точным кодом
//! выхода.
//!
//! Разбор устойчив к продолжениям строк через `^`, кавычкам, вложенным
//! переменным, префиксам `start "title" /min` и комментариям `rem` / `::`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};
use std::sync::LazyLock;

use regex_lite::{Captures, NoExpand, Regex};
use serde_json::Value;

/* ---------------------------------------------------------------------
   Game Filter. Значения взяты из service.bat → :game_switch_status.
   Важно: «выключено» — это порт 12, а не 0 и не пустая строка. Пустая
   строка даёт «--wf-tcp=80,443,» и «--filter-tcp=», и winws падает.
   Режим хранится в utils/game_filter.enabled — тем же файлом, что и в
   оригинальной сборке, чтобы PLASTO и service.bat не расходились.
   --------------------------------------------------------------------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameMode {
    Off,
    All,
    Tcp,
    Udp,
}

const HIGH: &str = "1024-65535";
const STUB: &str = "12";

impl GameMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GameMode::Off => "off",
            GameMode::All => "all",
            GameMode::Tcp => "tcp",
            GameMode::Udp => "udp",
        }
    }

    /// (GameFilter, GameFilterTCP, GameFilterUDP)
    fn ports(self) -> (&'static str, &'static str, &'static str) {
        match self {
            GameMode::Off => (STUB, STUB, STUB),
            GameMode::All => (HIGH, HIGH, HIGH),
            GameMode::Tcp => (HIGH, HIGH, STUB),
            GameMode::Udp => (HIGH, STUB, HIGH),
        }
    }

    /// Строго одно из четырёх имён (регистр уже приведён).
    pub fn parse(s: &str) -> Option<GameMode> {
        match s {
            "off" => Some(GameMode::Off),
            "all" => Some(GameMode::All),
            "tcp" => Some(GameMode::Tcp),
            "udp" => Some(GameMode::Udp),
            _ => None,
        }
    }

    /// Нормализует что угодно (true/false/"all"/"tcp"/null…) в режим.
    /// Как `gameMode()` в JS: true → all, ложное → off, чужое имя → off.
    pub fn from_value(v: &Value) -> GameMode {
        match v {
            Value::Bool(true) => GameMode::All,
            Value::String(s) if !s.is_empty() => {
                GameMode::parse(&s.to_lowercase()).unwrap_or(GameMode::Off)
            }
            _ => GameMode::Off,
        }
    }
}

fn flag_file(zapret_dir: &Path) -> PathBuf {
    zapret_dir.join("utils").join("game_filter.enabled")
}

/// Читает режим из файла-флага сборки, как это делает service.bat.
/// Файла нет → выключено; файл есть, но значение чужое → udp (как в bat).
pub fn read_game_mode(zapret_dir: &Path) -> GameMode {
    match fs::read(flag_file(zapret_dir)) {
        Ok(bytes) => {
            let raw = String::from_utf8_lossy(&bytes);
            let first = raw.lines().next().unwrap_or("").trim().to_lowercase();
            GameMode::parse(&first).unwrap_or(GameMode::Udp)
        }
        Err(_) => GameMode::Off,
    }
}

/// Пишет режим в файл-флаг, чтобы service.bat видел то же самое.
/// Ошибки ФС не всплывают: режим всё равно возвращается — как и в JS.
pub fn write_game_mode(zapret_dir: &Path, mode: GameMode) -> GameMode {
    let file = flag_file(zapret_dir);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if mode == GameMode::Off {
        let _ = fs::remove_file(&file);
    } else {
        let _ = fs::write(&file, format!("{}\r\n", mode.as_str()));
    }
    mode
}

/* ---------------------------------------------------------------------
   Пользовательские списки. Стратегии ссылаются на list-general-user.txt
   и прочие *-user.txt, которых нет в архиве: их создаёт
   service.bat → :load_user_lists при первом запуске. Без них winws
   ругается на недоступный hostlist и завершается.
   --------------------------------------------------------------------- */
const USER_LISTS: [(&str, &str); 3] = [
    ("ipset-exclude-user.txt", "203.0.113.113/32"),
    ("list-general-user.txt", "domain.example.abc"),
    ("list-exclude-user.txt", "domain.example.abc"),
];

/// Создаёт недостающие списки, возвращает имена созданных.
pub fn ensure_user_lists(zapret_dir: &Path) -> Vec<String> {
    let dir = zapret_dir.join("lists");
    let mut created = Vec::new();
    if fs::create_dir_all(&dir).is_err() {
        return created;
    }
    for (name, seed) in USER_LISTS {
        let f = dir.join(name);
        if !f.exists() && fs::write(&f, format!("{seed}\r\n")).is_ok() {
            created.push(name.to_string());
        }
    }
    created
}

/* ---------------------------------------------------------------------
   Разбор
   --------------------------------------------------------------------- */

static RE_CONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\^[ \t]*\r?\n[ \t]*").unwrap());
static RE_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:rem\b|::)").unwrap());
// set [/a] NAME=VALUE   (встречается и set "NAME=VALUE")
static RE_SET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^set\s+(?:/a\s+)?"?([A-Za-z_][A-Za-z0-9_]*)=([^"]*)"?\s*$"#).unwrap()
});
static RE_DP0: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)%~dp0").unwrap());
static RE_VAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([A-Za-z_][A-Za-z0-9_]*)%").unwrap());
static RE_UNRESOLVED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[A-Za-z_~]").unwrap());

#[derive(Default, Clone, Debug)]
pub struct ParseOpts {
    /// None — прочитать из utils/game_filter.enabled.
    pub game_filter: Option<GameMode>,
    /// По умолчанию winws.exe; у zapret2 — winws2.exe.
    pub exe_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Parsed {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub vars: HashMap<String, String>,
    pub game_filter: GameMode,
    pub raw: String,
}

pub fn parse_strategy(bat_file: &Path, opts: &ParseOpts) -> Result<Parsed, String> {
    let dir = bat_file.parent().unwrap_or_else(|| Path::new("."));
    let raw = read_text(bat_file)?;

    // 1. Склеиваем продолжения строк: "… ^\r\n   …" → "… …"
    let joined = RE_CONT.replace_all(&raw, " ");

    // 2. Базовые переменные окружения батника
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut dp0 = dir.to_string_lossy().into_owned();
    if !dp0.ends_with(MAIN_SEPARATOR) {
        dp0.push(MAIN_SEPARATOR);
    }
    vars.insert("~dp0".into(), dp0);

    // Game Filter задаётся не через set в general.bat, а через
    // `call service.bat load_game_filter`. Подставляем сами.
    let mode = opts.game_filter.unwrap_or_else(|| read_game_mode(dir));
    let (gf, gf_tcp, gf_udp) = mode.ports();
    vars.insert("gamefilter".into(), gf.into());
    vars.insert("gamefiltertcp".into(), gf_tcp.into());
    vars.insert("gamefilterudp".into(), gf_udp.into());

    // Имя бинаря зависит от движка: winws.exe у zapret1, winws2.exe у zapret2.
    let exe_name = opts.exe_name.as_deref().unwrap_or("winws.exe");
    let esc = regex_lite::escape(exe_name);
    let exe_re = Regex::new(&format!("(?i){esc}")).map_err(|e| e.to_string())?;

    let mut winws_line: Option<String> = None;

    for line in joined.lines() {
        let t = line.trim();
        if t.is_empty() || RE_COMMENT.is_match(t) {
            continue;
        }

        if let Some(m) = RE_SET.captures(t) {
            let value = expand(&m[2], &vars);
            vars.insert(m[1].to_lowercase(), value);
            continue;
        }

        if winws_line.is_none() && exe_re.is_match(t) {
            winws_line = Some(t.to_string());
        }
    }

    let winws_line = winws_line.ok_or_else(|| {
        let name = bat_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("в {name} не найден вызов {exe_name}")
    })?;

    // 3. Отрезаем всё до имени бинаря включительно:
    //    start "title" /min "%BIN%winws.exe" ...
    let cut_re = Regex::new(&format!(r#"(?i){esc}"?\s*(.*)$"#)).map_err(|e| e.to_string())?;
    let tail_raw = cut_re
        .captures(&winws_line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or("");
    let tail = expand(tail_raw, &vars);

    let args = tokenize(&tail);
    let bin = dir.join("bin");
    let exe = bin.join(exe_name);

    // Пустой аргумент вида «--filter-tcp=» winws не переживает: если такое
    // получилось, значит какая-то переменная не раскрылась — лучше упасть здесь.
    let empty: Vec<&str> = args.iter().filter(|a| a.ends_with('=')).map(|a| a.as_str()).collect();
    if !empty.is_empty() {
        return Err(format!("пустые аргументы после подстановки: {}", empty.join(" ")));
    }

    // Батник делает `cd /d %BIN%` перед запуском: относительные пути внутри
    // winws (и загрузка WinDivert64.sys) отсчитываются от папки bin.
    Ok(Parsed { exe, args, cwd: bin, vars, game_filter: mode, raw: tail })
}

/// Чтение с угадыванием кодировки. U+FFFD означает, что байты не UTF-8, —
/// значит это cp866/cp1251. Аргументы winws всегда ASCII, так что latin1
/// достаточно, чтобы не потерять байты.
fn read_text(file: &Path) -> Result<String, String> {
    let buf = fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let utf8 = String::from_utf8_lossy(&buf);
    if utf8.contains('\u{FFFD}') {
        Ok(buf.iter().map(|&b| b as char).collect())
    } else {
        Ok(utf8.trim_start_matches('\u{FEFF}').to_string())
    }
}

/// Подстановка %VAR% и %~dp0. Имена без учёта регистра; неизвестные —
/// пустая строка, как в cmd; ссылки раскрываются рекурсивно (глубина ≤ 8).
pub fn expand(s: &str, vars: &HashMap<String, String>) -> String {
    expand_at(s, vars, 0)
}

fn expand_at(s: &str, vars: &HashMap<String, String>, depth: usize) -> String {
    if depth > 8 {
        return s.to_string(); // защита от рекурсивных ссылок
    }
    let mut out = s.to_string();

    // %~dp0 — папка батника
    if let Some(dp0) = vars.get("~dp0") {
        out = RE_DP0.replace_all(&out, NoExpand(dp0)).into_owned();
    }

    out = RE_VAR
        .replace_all(&out, |c: &Captures| {
            let name = &c[1];
            if let Some(v) = vars.get(&name.to_lowercase()) {
                return v.clone();
            }
            std::env::var(name).unwrap_or_default()
        })
        .into_owned();

    if RE_UNRESOLVED.is_match(&out) {
        expand_at(&out, vars, depth + 1)
    } else {
        out
    }
}

/// Кавычки окружают пути: --hostlist="C:\zapret\lists\a.txt". Их надо снять,
/// но не разбивать аргумент по пробелу внутри кавычек.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;

    for c in line.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            started = true;
            continue;
        }
        if !in_quotes && c.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut cur));
                started = false;
            }
            continue;
        }
        cur.push(c);
        started = true;
    }
    if started {
        out.push(cur);
    }
    out.into_iter().filter(|a| !a.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /* ---------- песочница ---------- */
    struct Box_ {
        dir: tempfile::TempDir,
    }

    impl Box_ {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("bin")).unwrap();
            fs::create_dir_all(dir.path().join("lists")).unwrap();
            fs::write(dir.path().join("bin").join("winws.exe"), "").unwrap();
            Box_ { dir }
        }
        fn path(&self) -> &Path {
            self.dir.path()
        }
        fn fixture(&self, name: &str, content: &str) -> PathBuf {
            let p = self.path().join(name);
            fs::write(&p, content).unwrap();
            p
        }
    }

    fn norm(s: &str) -> String {
        Regex::new(r"[\\/]+").unwrap().replace_all(s, "/").into_owned()
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn opts(mode: GameMode) -> ParseOpts {
        ParseOpts { game_filter: Some(mode), exe_name: None }
    }

    fn has(r: &Parsed, a: &str) -> bool {
        r.args.iter().any(|x| x == a)
    }

    /* =================== tokenize =================== */

    #[test]
    fn токенизация_простые_аргументы() {
        assert_eq!(tokenize("--a --b=1 --c"), ["--a", "--b=1", "--c"]);
    }

    #[test]
    fn токенизация_кавычки_снимаются_пробел_внутри_сохраняется() {
        assert_eq!(
            tokenize(r#"--hostlist="C:\Program Files\a.txt" --x"#),
            [r"--hostlist=C:\Program Files\a.txt", "--x"]
        );
    }

    #[test]
    fn токенизация_лишние_пробелы_и_табы_игнорируются() {
        assert_eq!(tokenize("  --a \t\t --b  "), ["--a", "--b"]);
    }

    #[test]
    fn токенизация_пустая_строка_даёт_пустой_список() {
        assert!(tokenize("   ").is_empty());
    }

    /* =================== expand =================== */

    #[test]
    fn подстановка_без_учёта_регистра() {
        assert_eq!(expand("%BIN%x", &vars(&[("bin", r"C:\b\")])), r"C:\b\x");
    }

    #[test]
    fn подстановка_вложенные_ссылки_раскрываются() {
        assert_eq!(expand("%A%", &vars(&[("a", "%B%"), ("b", "ok")])), "ok");
    }

    #[test]
    fn подстановка_неизвестная_переменная_пустая_как_в_cmd() {
        assert_eq!(expand("[%NOPE_PLASTO_X%]", &HashMap::new()), "[]");
    }

    #[test]
    fn подстановка_циклическая_ссылка_не_вешает_разбор() {
        let r = expand("%A%", &vars(&[("a", "%A%")]));
        assert_eq!(r, "%A%");
    }

    #[test]
    fn подстановка_знак_доллара_в_пути_не_ломает_замену() {
        // В JS строка замены трактовала бы «$&»; здесь путь вставляется как есть.
        let v = vars(&[("~dp0", r"C:\a$&b\")]);
        assert_eq!(expand("%~dp0bin", &v), r"C:\a$&b\bin");
    }

    /* =================== parse_strategy =================== */

    #[test]
    fn разбор_базовый_формат_с_продолжениями_строк() {
        let b = Box_::new();
        let f = b.fixture(
            "general.bat",
            &[
                "@echo off",
                "chcp 65001 > nul",
                r"set BIN=%~dp0bin\",
                r"set LISTS=%~dp0lists\",
                r#"start "zapret: general" /min "%BIN%winws.exe" ^"#,
                "--wf-tcp=80,443 --wf-udp=443,50000-50100 ^",
                r#"--filter-udp=443 --hostlist="%LISTS%list-general.txt" ^"#,
                "--dpi-desync=fake --dpi-desync-repeats=11",
            ]
            .join("\r\n"),
        );

        let r = parse_strategy(&f, &ParseOpts::default()).unwrap();
        assert!(r.exe.ends_with(Path::new("bin").join("winws.exe")), "путь к winws.exe");
        assert!(has(&r, "--wf-tcp=80,443"), "аргумент из первой строки");
        assert!(has(&r, "--dpi-desync-repeats=11"), "аргумент из последней строки");

        let want = norm(&format!("--hostlist={}/lists/list-general.txt", b.path().display()));
        assert!(
            r.args.iter().any(|a| norm(a) == want),
            "путь к списку раскрыт из %LISTS% и %~dp0: {}",
            r.args.join(" ")
        );
        assert!(!r.args.iter().any(|a| a.contains('%')), "непрораскрытых переменных не осталось");
        assert!(
            !r.args.iter().any(|a| a.eq_ignore_ascii_case("start") || a.eq_ignore_ascii_case("/min")),
            "префикс start отброшен"
        );
    }

    const GF_BAT: [&str; 3] = [
        r#"set "BIN=%~dp0bin\""#,
        r#"start "z" /min "%BIN%winws.exe" --wf-tcp=443,%GameFilterTCP% --wf-udp=443,%GameFilterUDP% ^"#,
        "--filter-tcp=%GameFilterTCP% --dpi-desync=fake",
    ];

    fn gf_fixture(b: &Box_) -> PathBuf {
        b.fixture("general (GF).bat", &GF_BAT.join("\r\n"))
    }

    #[test]
    fn game_filter_off_порт_12_во_всех_переменных() {
        let b = Box_::new();
        let r = parse_strategy(&gf_fixture(&b), &opts(GameMode::Off)).unwrap();
        assert!(has(&r, "--wf-tcp=443,12"), "{}", r.args.join(" "));
        assert!(has(&r, "--wf-udp=443,12"), "{}", r.args.join(" "));
        assert!(has(&r, "--filter-tcp=12"), "{}", r.args.join(" "));
    }

    #[test]
    fn game_filter_all_высокие_порты_для_tcp_и_udp() {
        let b = Box_::new();
        let r = parse_strategy(&gf_fixture(&b), &opts(GameMode::All)).unwrap();
        assert!(has(&r, "--wf-tcp=443,1024-65535"));
        assert!(has(&r, "--wf-udp=443,1024-65535"));
    }

    #[test]
    fn game_filter_tcp_udp_остаётся_заглушкой() {
        let b = Box_::new();
        let r = parse_strategy(&gf_fixture(&b), &opts(GameMode::Tcp)).unwrap();
        assert!(has(&r, "--wf-tcp=443,1024-65535"));
        assert!(has(&r, "--wf-udp=443,12"));
    }

    #[test]
    fn game_filter_udp_tcp_остаётся_заглушкой() {
        let b = Box_::new();
        let r = parse_strategy(&gf_fixture(&b), &opts(GameMode::Udp)).unwrap();
        assert!(has(&r, "--wf-tcp=443,12"));
        assert!(has(&r, "--wf-udp=443,1024-65535"));
    }

    #[test]
    fn game_filter_true_это_all_false_это_off() {
        assert_eq!(GameMode::from_value(&json!(true)), GameMode::All);
        assert_eq!(GameMode::from_value(&json!(false)), GameMode::Off);

        let b = Box_::new();
        let f = gf_fixture(&b);
        let a = parse_strategy(&f, &opts(GameMode::from_value(&json!(true)))).unwrap();
        let o = parse_strategy(&f, &opts(GameMode::from_value(&json!(false)))).unwrap();
        assert_eq!(a.game_filter, GameMode::All);
        assert_eq!(o.game_filter, GameMode::Off);
    }

    #[test]
    fn game_filter_приведение_произвольных_значений() {
        assert_eq!(GameMode::from_value(&json!("TCP")), GameMode::Tcp);
        assert_eq!(GameMode::from_value(&json!("udp")), GameMode::Udp);
        for junk in [json!(null), json!(""), json!("мусор"), json!(0), json!(5), json!([]), json!({})] {
            assert_eq!(GameMode::from_value(&junk), GameMode::Off, "{junk}");
        }
    }

    #[test]
    fn режим_читается_из_файла_флага_если_не_задан_явно() {
        let b = Box_::new();
        fs::create_dir_all(b.path().join("utils")).unwrap();
        fs::write(b.path().join("utils").join("game_filter.enabled"), "tcp\r\n").unwrap();
        let r = parse_strategy(&gf_fixture(&b), &ParseOpts::default()).unwrap();
        assert_eq!(r.game_filter, GameMode::Tcp);
    }

    #[test]
    fn файл_флага_нет_выключено_значение_чужое_udp() {
        let b = Box_::new();
        assert_eq!(read_game_mode(b.path()), GameMode::Off);

        fs::create_dir_all(b.path().join("utils")).unwrap();
        let flag = b.path().join("utils").join("game_filter.enabled");
        fs::write(&flag, "что-то странное\r\n").unwrap();
        assert_eq!(read_game_mode(b.path()), GameMode::Udp);
        fs::write(&flag, "").unwrap();
        assert_eq!(read_game_mode(b.path()), GameMode::Udp);
    }

    #[test]
    fn запись_режима_кладёт_файл_и_off_его_удаляет() {
        let b = Box_::new();
        let flag = b.path().join("utils").join("game_filter.enabled");

        assert_eq!(write_game_mode(b.path(), GameMode::Tcp), GameMode::Tcp);
        assert_eq!(fs::read_to_string(&flag).unwrap(), "tcp\r\n");
        assert_eq!(read_game_mode(b.path()), GameMode::Tcp);

        assert_eq!(write_game_mode(b.path(), GameMode::Off), GameMode::Off);
        assert!(!flag.exists());
        // повторное выключение без файла — не ошибка
        assert_eq!(write_game_mode(b.path(), GameMode::Off), GameMode::Off);
    }

    #[test]
    fn нераскрытая_переменная_даёт_ошибку_а_не_битые_аргументы() {
        let b = Box_::new();
        let f = b.fixture(
            "general (BROKEN).bat",
            &[r#"set "BIN=%~dp0bin\""#, r#""%BIN%winws.exe" --filter-tcp=%TotallyUnknownVar%"#].join("\r\n"),
        );
        let e = parse_strategy(&f, &ParseOpts::default()).unwrap_err();
        assert!(e.contains("пустые аргументы"), "{e}");
    }

    #[test]
    fn cwd_это_папка_bin_как_после_cd_d_bin() {
        let b = Box_::new();
        let r = parse_strategy(&gf_fixture(&b), &opts(GameMode::Off)).unwrap();
        assert_eq!(r.cwd, b.path().join("bin"));
    }

    #[test]
    fn переменная_заданная_внутри_bat_важнее_нашей_подстановки() {
        let b = Box_::new();
        let f = b.fixture(
            "general (SET).bat",
            &[
                r"set BIN=%~dp0bin\",
                "set GameFilterUDP=100-200",
                r#"start "z" /min "%BIN%winws.exe" --wf-udp=443,%GameFilterUDP%"#,
            ]
            .join("\r\n"),
        );
        let r = parse_strategy(&f, &opts(GameMode::All)).unwrap();
        assert!(has(&r, "--wf-udp=443,100-200"), "{}", r.args.join(" "));
    }

    #[test]
    fn quoted_set_разбирается() {
        let b = Box_::new();
        let f = b.fixture(
            "general (Q).bat",
            &[
                r#"set "BIN=%~dp0bin\""#,
                r#"set "LISTS=%~dp0lists\""#,
                r#""%BIN%winws.exe" --hostlist="%LISTS%list-general.txt""#,
            ]
            .join("\r\n"),
        );
        let r = parse_strategy(&f, &ParseOpts::default()).unwrap();
        assert!(r.args.iter().any(|a| norm(a).ends_with("/lists/list-general.txt")), "{}", r.args.join(" "));
    }

    #[test]
    fn строки_rem_и_двоеточия_не_мешают() {
        let b = Box_::new();
        let f = b.fixture(
            "general (REM).bat",
            &[
                r"rem set BIN=C:\wrong\",
                r":: set BIN=C:\also-wrong\",
                r"set BIN=%~dp0bin\",
                r#""%BIN%winws.exe" --wf-tcp=443"#,
            ]
            .join("\r\n"),
        );
        let r = parse_strategy(&f, &ParseOpts::default()).unwrap();
        assert!(has(&r, "--wf-tcp=443"));
        assert!(!r.exe.to_string_lossy().contains("wrong"));
    }

    #[test]
    fn new_как_разделитель_профилей_сохраняется() {
        let b = Box_::new();
        let f = b.fixture(
            "general (NEW).bat",
            &[
                r"set BIN=%~dp0bin\",
                r#""%BIN%winws.exe" --filter-udp=443 --dpi-desync=fake ^"#,
                "--new ^",
                "--filter-tcp=443 --dpi-desync=split2",
            ]
            .join("\r\n"),
        );
        let r = parse_strategy(&f, &ParseOpts::default()).unwrap();
        assert_eq!(r.args.iter().filter(|a| *a == "--new").count(), 1);
        assert!(has(&r, "--dpi-desync=split2"));
    }

    #[test]
    fn файл_без_winws_даёт_понятную_ошибку() {
        let b = Box_::new();
        let f = b.fixture("general (BAD).bat", "@echo off\r\necho nothing here");
        let e = parse_strategy(&f, &ParseOpts::default()).unwrap_err();
        assert!(e.contains("не найден вызов winws.exe"), "{e}");
    }

    #[test]
    fn cp866_не_роняет_разбор() {
        let b = Box_::new();
        let p = b.path().join("general (CP866).bat");
        let mut bytes = b"rem ".to_vec();
        bytes.extend_from_slice(&[0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2]); // «Привет» в cp866
        bytes.extend_from_slice(b"\r\nset BIN=%~dp0bin\\\r\n\"%BIN%winws.exe\" --wf-tcp=443\r\n");
        fs::write(&p, bytes).unwrap();
        let r = parse_strategy(&p, &ParseOpts::default()).unwrap();
        assert!(has(&r, "--wf-tcp=443"));
    }

    #[test]
    fn zapret2_ищет_winws2_и_не_путает_с_winws() {
        let b = Box_::new();
        let f = b.fixture(
            "general (V2).bat",
            &[r"set BIN=%~dp0bin\", r#""%BIN%winws2.exe" --lua-init=@x.lua"#].join("\r\n"),
        );
        let o = ParseOpts { game_filter: Some(GameMode::Off), exe_name: Some("winws2.exe".into()) };
        let r = parse_strategy(&f, &o).unwrap();
        assert!(r.exe.ends_with(Path::new("bin").join("winws2.exe")));
        assert!(has(&r, "--lua-init=@x.lua"));
        // а как zapret1 тот же файл — не стратегия
        assert!(parse_strategy(&f, &ParseOpts::default()).is_err());
    }

    /* =================== пользовательские списки =================== */

    #[test]
    fn пользовательские_списки_создаются_один_раз() {
        let b = Box_::new();
        let first = ensure_user_lists(b.path());
        assert_eq!(first.len(), 3);
        let text = fs::read_to_string(b.path().join("lists").join("ipset-exclude-user.txt")).unwrap();
        assert_eq!(text, "203.0.113.113/32\r\n");

        // существующие не перезаписываем — там могут быть правки пользователя
        fs::write(b.path().join("lists").join("list-general-user.txt"), "my.example\r\n").unwrap();
        assert!(ensure_user_lists(b.path()).is_empty());
        assert_eq!(
            fs::read_to_string(b.path().join("lists").join("list-general-user.txt")).unwrap(),
            "my.example\r\n"
        );
    }
}

/// Сравнение с JS-эталоном на сгенерированном корпусе.
///
///   node scripts/gen-corpus.js %TEMP%\plasto-corpus
///   set PLASTO_CORPUS=%TEMP%\plasto-corpus
///   cargo test -- --ignored сравнение_с_js
///
/// Не запускается по умолчанию: ему нужен корпус, который делает Node.
/// Живёт рядом с портом, пока strategy.js не удалён: потом сравнивать будет
/// не с чем, и тест вместе с генератором можно убрать.
#[cfg(test)]
mod differential {
    use super::*;
    use serde_json::Value;

    fn norm(s: &str) -> String {
        Regex::new(r"[\\/]+").unwrap().replace_all(s, "/").into_owned()
    }

    #[test]
    #[ignore = "нужен корпус: PLASTO_CORPUS (scripts/gen-corpus.js)"]
    fn сравнение_с_js_на_случайных_батниках() {
        let dir = PathBuf::from(std::env::var("PLASTO_CORPUS").expect("задайте PLASTO_CORPUS"));
        let text = fs::read_to_string(dir.join("expected.json")).expect("expected.json");
        let expected: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert!(!expected.is_empty(), "корпус пуст");

        let mut bad: Vec<String> = Vec::new();
        let (mut ok_n, mut err_n) = (0, 0);

        for e in &expected {
            let name = e["file"].as_str().unwrap();
            let mode = GameMode::parse(e["mode"].as_str().unwrap()).unwrap();
            let o = ParseOpts { game_filter: Some(mode), exe_name: None };
            let got = parse_strategy(&dir.join(name), &o);

            match (e["ok"].as_bool().unwrap(), got) {
                (true, Ok(r)) => {
                    ok_n += 1;
                    let want: Vec<String> =
                        e["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
                    if r.args != want {
                        bad.push(format!("{name}: args\n   js:   {want:?}\n   rust: {:?}", r.args));
                    } else if r.raw != e["raw"].as_str().unwrap() {
                        bad.push(format!("{name}: raw\n   js:   {:?}\n   rust: {:?}", e["raw"], r.raw));
                    } else if norm(&r.exe.to_string_lossy()) != norm(e["exe"].as_str().unwrap())
                        || norm(&r.cwd.to_string_lossy()) != norm(e["cwd"].as_str().unwrap())
                    {
                        bad.push(format!("{name}: exe/cwd"));
                    } else if r.game_filter.as_str() != e["gameFilter"].as_str().unwrap() {
                        bad.push(format!("{name}: gameFilter"));
                    }
                }
                (false, Err(msg)) => {
                    err_n += 1;
                    let want = e["error"].as_str().unwrap();
                    if msg != want {
                        bad.push(format!("{name}: текст ошибки\n   js:   {want}\n   rust: {msg}"));
                    }
                }
                (js_ok, got) => bad.push(format!(
                    "{name}: исход разный — js {} / rust {}",
                    if js_ok { "разобрал" } else { "ошибка" },
                    if got.is_ok() { "разобрал" } else { "ошибка" }
                )),
            }
        }

        println!("корпус: {} файлов (разобрано {ok_n}, ошибок {err_n})", expected.len());
        assert!(
            bad.is_empty(),
            "{} расхождений из {}:\n{}",
            bad.len(),
            expected.len(),
            bad.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}
