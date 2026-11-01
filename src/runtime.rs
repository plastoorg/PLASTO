use std::{io::{BufRead, BufReader}, process::{Child, Command, Stdio}, sync::{Arc, Mutex}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use serde_json::{json, Map, Value};
use crate::host::Host;
use crate::{engines::{self, BuildOpts, Engine}, strategy};

#[derive(Default)]
pub struct Runtime { child: Option<Child>, strategy: Option<String>, started: Option<Instant>, state: String, logs: Vec<Value>, generation: u64, failure_reason: Option<String>, crashes: Vec<Instant>, reason: String,
    /// Запущен ли winws без портов тестера: тогда подбор его не трогает.
    isolated: bool }
pub type EngineState = Arc<Mutex<Runtime>>;
impl Drop for Runtime { fn drop(&mut self) { if let Some(c) = self.child.as_mut() { let _ = c.kill(); let _ = c.wait(); } } }
impl Runtime {
    pub fn snapshot(&self) -> Value {
        json!({"state": if self.state.is_empty() { "off" } else { &self.state }, "strategy": self.strategy, "running": self.child.is_some(), "pid": self.child.as_ref().map(Child::id), "uptime": self.started.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0), "isolated": self.child.is_some() && self.isolated, "lastCheck": null})
    }
    fn log(&mut self, level: &str, message: &str) -> Value {
        let item = json!({"ts":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,"level":level,"message":message});
        self.logs.push(item.clone()); if self.logs.len() > 500 { self.logs.remove(0); } item
    }
}
/// Сколько раз подряд поднимать упавший winws, прежде чем сдаться.
const MAX_RESTARTS: usize = 3;
/// Окно, в котором считаются падения: реже — уже не «подряд».
const CRASH_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Упал раньше — значит, не запускается стратегия, а не сбой: не перезапускаем.
const MIN_UPTIME: Duration = Duration::from_secs(10);
const RESTART_DELAY: Duration = Duration::from_millis(1500);
fn emit_state(app: &Host, rt: &Runtime, reason: &str) {
    let snap = rt.snapshot(); let _ = app.emit("plasto:engine:state", json!({"state":snap["state"],"snapshot":snap,"reason":reason}));
}
/// Итог пинга: процесс жив, но какие-то сервисы (`down`) не открываются —
/// «timeout» с их перечнем; открылись все — снова «on».
pub fn set_health(app: &Host, down: &[&str]) {
    let state = app.state::<EngineState>();
    let Ok(mut rt) = state.lock() else { return };
    if rt.child.is_none() { return; }
    let want = if down.is_empty() { "on" } else { "timeout" };
    let reason = if down.is_empty() { "сайты снова открываются".to_owned() } else { format!("не открывается {}", down.join(" и ")) };
    if rt.state != want || (want == "timeout" && rt.reason != reason) {
        rt.state = want.into();
        rt.reason = reason.clone();
        emit_state(app, &rt, &reason);
    }
}
pub fn snapshot(app: &Host) -> Value { app.state::<EngineState>().lock().map(|s| s.snapshot()).unwrap_or(json!({"state":"error","running":false})) }
pub fn logs(app: &Host, clear: bool) -> Value { match app.state::<EngineState>().lock() { Ok(mut s) => { if clear { s.logs.clear(); json!(true) } else { json!(s.logs) } }, Err(_) => json!([]) } }
pub fn shutdown(app: &Host) {
    let state=app.state::<EngineState>();
    if let Ok(mut rt)=state.lock() { rt.generation+=1; if let Some(mut child)=rt.child.take() { let _=child.kill(); let _=child.wait(); } rt.started=None; rt.state="off".into(); };
}

#[cfg_attr(not(windows), allow(dead_code))]
fn contains_winws_process_except(text: &str, ignore_pid: Option<u32>) -> bool {
    text.lines().any(|line| { let mut fields=line.split(','); let first=fields.next().unwrap_or("").trim().trim_matches('"'); let pid=fields.next().and_then(|x|x.trim().trim_matches('"').parse::<u32>().ok()); (first.eq_ignore_ascii_case("winws.exe") || first.eq_ignore_ascii_case("winws2.exe")) && (ignore_pid.is_none() || pid != ignore_pid) })
}

pub fn other_winws_except(ignore_pid: Option<u32>) -> Result<bool,String> {
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        let out=Command::new("tasklist.exe").args(["/FO","CSV","/NH"]).creation_flags(0x08000000).output().map_err(|e|e.to_string())?;
        if !out.status.success() { return Err("не удалось проверить запущенные процессы".into()); }
        let text=String::from_utf8_lossy(&out.stdout);
        Ok(contains_winws_process_except(&text,ignore_pid))
    }
    #[cfg(not(windows))] { let _=ignore_pid; Ok(false) }
}

pub fn other_winws() -> Result<bool,String> { other_winws_except(None) }

pub fn action(app: &Host, cfg: &Map<String, Value>, action: &str, selected: Option<&str>) -> Value {
    let state = app.state::<EngineState>();
    let Ok(mut rt) = state.lock() else { return json!({"error":"ошибка состояния движка"}) };
    let stop = action == "stop" || (action == "toggle" && rt.child.is_some());
    if stop {
        rt.generation += 1;
        if let Some(mut child) = rt.child.take() { let _ = child.kill(); let _ = child.wait(); }
        rt.state = "off".into(); rt.started = None; emit_state(app, &rt, "остановлено"); return rt.snapshot();
    }
    if rt.child.is_some() { return rt.snapshot(); }
    // Тестовый winws подбора — наш, не чужой.
    let testing = crate::tester::child_pid();
    match other_winws_except(testing) {
        // Чужой zapret — не наша ошибка: состояние не трогаем, а отдаём, что
        // нашли, чтобы интерфейс предложил это остановить.
        Ok(true) => { drop(rt); return json!({"error":"уже работает другой zapret","conflict":"winws","found":crate::conflict::winws(testing)}); }
        Err(e) => { rt.state="error".into(); emit_state(app,&rt,&e); return json!({"error":e}); }
        _ => {}
    }
    let dir = std::path::Path::new(cfg.get("zapretDir").and_then(Value::as_str).unwrap_or(""));
    let mode = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
    let valid = engines::validate(dir, mode);
    let chosen = selected.or_else(|| cfg.get("strategy").and_then(Value::as_str)).or_else(|| valid.presets.first().map(|p| p.file.as_str())).unwrap_or("");
    if !valid.ok || !valid.presets.iter().any(|p| p.file == chosen) { let err = valid.reason.unwrap_or_else(|| "стратегия не найдена".into()); rt.state="error".into(); emit_state(app,&rt,&err); return json!({"error":err}); }
    let mut plan = match engines::build_command(dir, chosen, &build_opts(cfg)) { Ok(p) => p, Err(e) => { rt.state="error".into(); emit_state(app,&rt,&e); return json!({"error":e}); } };
    if let Err(e) = prepare(dir) { rt.state="error".into(); emit_state(app,&rt,&e); return json!({"error":e}); }
    #[cfg(windows)] if !tcp_timestamps() { let item=rt.log("warn","Не удалось включить TCP timestamps: некоторые стратегии могут не работать"); let _=app.emit("plasto:engine:log",item); }
    // Порты тестера основному winws не достаются: подбор идёт рядом, не выключая обход.
    rt.isolated = match crate::isolate::apply(&mut plan, crate::isolate::Side::Main) {
        Ok(()) => true,
        Err(e) => { let item=rt.log("warn",&format!("Подбор стратегии будет выключать обход на время проверки: {e}")); let _=app.emit("plasto:engine:log",item); false }
    };
    let mut cmd = Command::new(&plan.exe); cmd.args(&plan.args).current_dir(&plan.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x08000000); }
    let mut child = match cmd.spawn() { Ok(c) => c, Err(e) => { let e=if e.raw_os_error()==Some(740) { "Требуются права администратора: запустите PLASTO через «Запуск от имени администратора»".to_string() } else { format!("не удалось запустить winws: {e}") }; rt.state="critical".into(); emit_state(app,&rt,&e); return json!({"error":e}); } };
    rt.generation += 1; rt.failure_reason=None; let gen = rt.generation;
    for (level, pipe) in [("out", child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)), ("err", child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>))] {
        if let Some(pipe) = pipe { let app = app.clone(); let shared = Arc::clone(&state); std::thread::spawn(move || { for line in BufReader::new(pipe).lines().map_while(Result::ok) { if let Ok(mut rt) = shared.lock() { if rt.generation != gen { break; } if line.to_ascii_lowercase().contains("a copy of winws is already running") {rt.failure_reason=Some("Другой winws уже использует этот фильтр. Проверьте запущенные программы и службы.".into());} let item=rt.log(level,&line); let _=app.emit("plasto:engine:log",item); } } }); }
    }
    rt.child = Some(child); rt.strategy = Some(chosen.to_owned()); rt.started = Some(Instant::now()); rt.state = "on".into(); emit_state(app,&rt,"процесс запущен; проверка соединения ещё не выполнена");
    let result=rt.snapshot(); drop(rt);
    let app = app.clone(); let shared = Arc::clone(&state);
    std::thread::spawn(move || watch(app, shared, gen));
    result
}

/// Как собирать команду winws по настройкам: движок и Game Filter.
pub fn build_opts(cfg: &Map<String, Value>) -> BuildOpts {
    let mut opts = BuildOpts::default(); opts.engine = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
    opts.parse.game_filter = cfg.get("gameFilter").map(strategy::GameMode::from_value);
    opts
}

/// Готовит папку zapret к запуску winws: и основного, и тестового.
pub fn prepare(dir: &std::path::Path) -> Result<(), String> {
    strategy::ensure_user_lists(dir);
    for filename in ["ipset-exclude-user.txt","list-general-user.txt","list-exclude-user.txt"] {
        if !dir.join("lists").join(filename).is_file() { return Err(format!("не удалось подготовить пользовательский список {filename}")); }
    }
    // Снимаем отметку только у файлов, используемых движком, не меняя содержимое.
    if let Ok(entries)=std::fs::read_dir(dir.join("bin")) { for entry in entries.flatten() {
        let path=entry.path();if path.is_file() && path.extension().and_then(|e|e.to_str()).is_some_and(|e|["exe","dll","sys","bin"].iter().any(|x|e.eq_ignore_ascii_case(x))) {let _=std::fs::remove_file(format!("{}:Zone.Identifier",path.display()));}
    } }
    Ok(())
}

/// Части стратегий нужны TCP timestamps; false — включить не удалось.
#[cfg(windows)]
pub fn tcp_timestamps() -> bool {
    use std::os::windows::process::CommandExt;
    let out=Command::new("netsh.exe").args(["interface","tcp","set","global","timestamps=enabled"]).creation_flags(0x08000000).output();
    out.as_ref().map_or(false, |o|o.status.success())
}

/// Присмотр за запущенным winws: упал сам после нормальной работы — поднять
/// той же стратегией, не больше `MAX_RESTARTS` раз за `CRASH_WINDOW`.
fn watch(app: Host, shared: EngineState, gen: u64) {
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let Ok(mut rt)=shared.lock() else { return };
        if rt.generation != gen { return; }
        let Some(status)=rt.child.as_mut().and_then(|c| c.try_wait().ok()).flatten() else { continue };
        let lived=rt.started.map_or(Duration::ZERO, |t| t.elapsed());
        rt.child=None; rt.started=None; rt.state="error".into();
        let known=rt.failure_reason.take();
        let now=Instant::now(); rt.crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        // Известная причина (чужой winws) — не сбой, а повод остановиться.
        let retry=known.is_none() && lived >= MIN_UPTIME && rt.crashes.len() < MAX_RESTARTS;
        let reason=known.unwrap_or_else(||format!("winws завершился: {status}"));
        let item=rt.log("error",&reason); let _=app.emit("plasto:engine:log",item);
        if !retry { emit_state(&app,&rt,&reason); return; }
        rt.crashes.push(now);
        let note=format!("Перезапускаю winws ({} из {MAX_RESTARTS})", rt.crashes.len()); let item=rt.log("warn",&note); let _=app.emit("plasto:engine:log",item);
        let strategy=rt.strategy.clone();
        drop(rt);
        std::thread::sleep(RESTART_DELAY);
        // Пока ждали, обход могли выключить или включить заново — тогда не мешаем.
        if shared.lock().map_or(true, |rt| rt.generation != gen) { return; }
        let snap=action(&app,&crate::api::read_cfg(&app),"start",strategy.as_deref());
        if snap["running"] != true {
            if let Ok(rt)=shared.lock() { if rt.child.is_none() { emit_state(&app,&rt,snap["error"].as_str().unwrap_or(&reason)); } }
        }
        return;
    }
}

#[cfg(test)] mod conflict_tests {
    use super::contains_winws_process_except;
    #[test] fn detects_only_process_name() {
        assert!(contains_winws_process_except("\"WINWS.exe\",\"123\",\"Console\"",None));
        assert!(contains_winws_process_except("\"winws2.exe\",\"44\"",None));
        assert!(!contains_winws_process_except("\"not-winws.exe\",\"12\"",None));
        assert!(!contains_winws_process_except("INFO: no tasks",None));
        assert!(!contains_winws_process_except("\"winws.exe\",\"123\"",Some(123)));
    }
}
