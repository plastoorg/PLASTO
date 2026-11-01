//! Единая точка входа из интерфейса.
//!
//! Интерфейс зовёт `call(name, payload)` с теми же именами каналов, что были
//! у `ipcMain.handle` в Electron-версии, на рабочем потоке: проверки и загрузки
//! идут секундами и не должны подвешивать окно. Окном (свернуть, спрятать,
//! выйти) интерфейс управляет сам — здесь только бэкенд.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use crate::host::Host;
use crate::engines::{self, BuildOpts, Engine};

/* ---------- конфиг ------------------------------------------------------- */

fn cfg_path(app: &Host) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("config.json"))
}

pub fn read_cfg(app: &Host) -> Map<String, Value> {
    cfg_path(app)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn write_cfg(app: &Host, cfg: &Map<String, Value>) -> std::io::Result<()> {
    let path = cfg_path(app)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "нет папки конфигурации"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".into());
    fs::write(path, text)
}

/// Куда PLASTO скачивает компоненты (zapret, Telegram-прокси): своя папка из
/// настроек или папка профиля в %AppData%.
pub fn components_dir(app: &Host) -> Result<PathBuf, String> {
    match read_cfg(app).get("componentsDir").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        Some(d) => Ok(PathBuf::from(d)),
        None => app.path().app_data_dir().map_err(|e| e.to_string()),
    }
}

/// Подпапки компонентов внутри папки компонентов.
const COMPONENTS: [&str; 3] = ["zapret", "tgws", "tgws-rs"];

/// Перенести папку: переименованием, а на другой диск — копией.
fn move_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_dir(from, to)?;
    fs::remove_dir_all(from)
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() { copy_dir(&entry.path(), &target)? } else { fs::copy(entry.path(), target).map(|_| ())? }
    }
    Ok(())
}

/// Сменить папку компонентов: уже скачанное переезжает туда же.
fn move_components(app: &Host, to: PathBuf) -> Value {
    if crate::runtime::snapshot(app)["running"] == true || crate::tgws::snapshot(app)["running"] == true || crate::tester::running() {
        return json!({"error":"сначала выключите обход и Telegram-прокси"});
    }
    let from = match components_dir(app) { Ok(d) => d, Err(e) => return json!({"error":e}) };
    if from == to { return json!({"dir":to}); }
    if let Some(sub) = COMPONENTS.iter().find(|sub| from.join(sub).exists() && to.join(sub).exists()) {
        return json!({"error":format!("в выбранной папке уже есть «{sub}»")});
    }
    if let Err(e) = fs::create_dir_all(&to) { return json!({"error":format!("папка недоступна: {e}")}); }
    for sub in COMPONENTS.iter().filter(|sub| from.join(sub).exists()) {
        if let Err(e) = move_dir(&from.join(sub), &to.join(sub)) { return json!({"error":format!("не удалось перенести «{sub}»: {e}")}); }
    }
    let mut cfg = read_cfg(app);
    let default = app.path().app_data_dir().ok();
    cfg.insert("componentsDir".into(), if default.as_ref() == Some(&to) { Value::Null } else { json!(to) });
    // Скачанный zapret переехал вместе с папкой.
    let zapret = cfg.get("zapretDir").and_then(Value::as_str).map(PathBuf::from).filter(|d| *d == from.join("zapret"));
    if zapret.is_some() { cfg.insert("zapretDir".into(), json!(to.join("zapret"))); }
    if let Err(e) = write_cfg(app, &cfg) { return json!({"error":format!("компоненты перенесены, но настройки не сохранены: {e}")}); }
    json!({"dir":to,"zapretDir":cfg.get("zapretDir")})
}

/// Системный выбор папки. `None` — окно закрыли без выбора.
fn pick_folder(title: &str) -> Result<Option<String>, String> {
    let script = format!("Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = '{title}'; if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::Write($d.SelectedPath) }}");
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-STA", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    let out = command.output().map_err(|e| e.to_string())?;
    if !out.status.success() { return Err("не удалось открыть выбор папки".into()); }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(Some(dir).filter(|d| !d.is_empty()))
}

/// Поверхностное слияние: ключи патча перекрывают старые, остальное остаётся.
/// Не-объект в патче игнорируется — испорченный вызов не должен стирать конфиг.
pub fn merge(mut cfg: Map<String, Value>, patch: &Value) -> Map<String, Value> {
    if let Some(p) = patch.as_object() {
        for (k, v) in p {
            cfg.insert(k.clone(), v.clone());
        }
    }
    cfg
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    #[link(name = "shell32")]
    extern "system" { fn IsUserAnAdmin() -> i32; }
    unsafe { IsUserAnAdmin() != 0 }
}
#[cfg(not(windows))]
pub fn is_elevated() -> bool { false }

/* ---------- bootstrap ---------------------------------------------------- */

/// Всё, что интерфейсу нужно для первого кадра, одним вызовом.
fn bootstrap(app: &Host) -> Value {
    let mut cfg = read_cfg(app);
    cfg.entry("autoDownload").or_insert(json!(true));
    if let Err(e) = ensure_bundled(app, &mut cfg) {
        eprintln!("[plasto] встроенный zapret: {e}");
    }

    let dir = cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
    let mode = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
    let validation = engines::validate(std::path::Path::new(dir), mode);
    let detected = engines::detect_engine(std::path::Path::new(dir));
    let strategies: Vec<Value> = validation.presets.iter().map(|p| p.to_json()).collect();
    let status = validation.to_json();
    json!({
        "snapshot": crate::runtime::snapshot(app),
        "config": cfg,
        "engineMode": cfg.get("engineMode"),
        "engineDetected": { "engine": detected.engine.map(Engine::as_str), "exe": detected.exe, "reason": detected.reason },
        "testRunning": crate::tester::running(),
        "tgws": crate::tgws::snapshot(app),
        "strategies": strategies,
        "dirStatus": status,
        "logs": crate::runtime::logs(app, false),
        "version": env!("CARGO_PKG_VERSION"),
        "elevated": is_elevated(),
        "installed": null,
        "service": crate::service::status(),
        "portable": false,
        "componentsDir": components_dir(app).ok()
    })
}

/// Встроенная сборка zapret при старте: распаковать, если своей сборки нет
/// или скачанная старее встроенной (см. `bundled`). Свои списки человека
/// переезжают в новую папку. Пока обход работает, папку не меняем.
fn ensure_bundled(app: &Host, cfg: &mut Map<String, Value>) -> Result<(), String> {
    let mode = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
    if mode == Some(Engine::V2) {
        return Ok(());
    }
    let dir = PathBuf::from(cfg.get("zapretDir").and_then(Value::as_str).unwrap_or(""));
    let dir_ok = !dir.as_os_str().is_empty() && engines::validate(&dir, mode).ok;
    let tag = cfg.get("zapretTag").and_then(Value::as_str);
    if !crate::bundled::should_install(dir_ok, tag) || crate::runtime::snapshot(app)["running"] == true {
        return Ok(());
    }
    let target = crate::bundled::install(&components_dir(app)?.join("zapret"))?;
    if dir_ok {
        crate::bundled::carry_user_files(&dir, &target);
    }
    cfg.insert("zapretDir".into(), json!(target));
    cfg.insert("zapretTag".into(), json!(crate::bundled::TAG));
    write_cfg(app, cfg).map_err(|e| e.to_string())
}

/// Канал обновлений PLASTO из настроек; по умолчанию — по своей версии.
fn beta_channel(app: &Host) -> bool {
    match read_cfg(app).get("updateChannel").and_then(Value::as_str) {
        Some("stable") => false,
        Some("beta") => true,
        _ => crate::selfupdate::beta_by_default(),
    }
}

/// Порт Telegram-прокси: свой, если его меняли из-за занятого 1443.
fn tg_port(app: &Host) -> u16 {
    read_cfg(app).get("tgPort").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).filter(|p| *p >= 1024).unwrap_or(crate::tgws::DEFAULT_PORT)
}

/* ---------- главная кнопка ------------------------------------------------ */

/// Что было включено: при запуске PLASTO это возвращается, если стоит
/// «При запуске включать то, что было включено».
fn remember(app: &Host, key: &str, on: bool) {
    let mut cfg = read_cfg(app);
    let mut resume = cfg.get("resume").and_then(Value::as_object).cloned().unwrap_or_default();
    resume.insert(key.into(), json!(on));
    cfg.insert("resume".into(), Value::Object(resume));
    if let Err(e) = write_cfg(app, &cfg) {
        eprintln!("[plasto] состояние не запомнено: {e}");
    }
}

/// Главная кнопка: обход zapret и прокси ИИ-сервисов в hosts вместе.
/// Telegram-прокси и DNS она не трогает. Выключение убирает блок PLASTO из
/// hosts, настройки ИИ остаются; включение пишет его заново по настройкам.
fn power(app: &Host, on: bool) -> Value {
    if crate::tester::running() { return json!({"error":"дождитесь окончания теста или отмените его"}); }
    let cfg = read_cfg(app);
    let mut result = crate::runtime::action(app, &cfg, if on { "start" } else { "stop" }, None);
    if result["error"].is_string() { return result; }
    remember(app, "zapret", on);
    let file = crate::hosts::hosts_path();
    let current = crate::hosts::current(&file);
    let proxy = cfg.get("aiProxyServices").cloned().unwrap_or(json!([]));
    let blocklist = cfg.get("aiBlocklist").cloned().unwrap_or(json!(false));
    let wanted = proxy.as_array().is_some_and(|a| !a.is_empty()) || blocklist == true;
    let payload = if on {
        if !wanted || current.proxies > 0 || current.blocked > 0 { return result; }
        json!({"proxy":proxy,"blocklist":blocklist,"entry":cfg.get("aiEntry").cloned().unwrap_or(json!(""))})
    } else {
        if current.proxies == 0 && current.blocked == 0 { return result; }
        json!({"proxy":[],"blocklist":false})
    };
    let hosts = app.path().app_data_dir().map_err(|e| e.to_string()).and_then(|root| {
        let source = if on {
            match crate::ai::catalog(&root) {
                Some(s) => s,
                None => { crate::ai::refresh(&root)?; crate::ai::catalog(&root).unwrap_or_default() }
            }
        } else { Default::default() };
        crate::ai::apply_hosts(&payload, &root, &file, &source)
    });
    match hosts {
        Ok(_) => { crate::hosts::flush_dns(); }
        Err(e) => result["aiError"] = json!(e),
    }
    result
}

/* ---------- диспетчер ---------------------------------------------------- */

pub fn call(app: &Host, name: &str, payload: Value) -> Value {
    match name {
        "app:bootstrap" => bootstrap(&app),

        "engine:toggle" | "engine:start" | "engine:stop" => {
            if crate::tester::running() { return json!({"error":"дождитесь окончания теста или отмените его"}); }
            let result = crate::runtime::action(app, &read_cfg(app), name.strip_prefix("engine:").unwrap_or(""), payload.as_str());
            if !result["error"].is_string() { remember(app, "zapret", result["running"] == true); }
            result
        }
        "power:on" | "power:off" => power(app, name == "power:on"),
        "engine:check" => crate::tester::health(&app),
        "ping:run" => crate::ping::run(app, &read_cfg(app), &payload),
        "engine:logs" => crate::runtime::logs(&app, false),
        "engine:clearLogs" => crate::runtime::logs(&app, true),
        "diag:run" => {let cfg=read_cfg(&app);let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");crate::diagnostics::run(&app,dir)},
        "test:run" => {
            if crate::speedtest::running() { return json!({"error":"дождитесь окончания замера скорости"}); }
            crate::tester::run(&app,&read_cfg(&app),&payload)
        }
        "speed:run" => {
            if crate::tester::running() { return json!({"error":"сначала завершите тест стратегий"}); }
            crate::speedtest::run(&app)
        }
        "speed:cancel" => json!(crate::speedtest::cancel()),
        "stab:start" => crate::stability::start(&app),
        "stab:stop" => crate::stability::stop(),
        "stab:status" => crate::stability::status(&app),
        "stab:live" => crate::stability::live(),
        "stab:query" => crate::stability::query(&app, &payload),
        "stab:clear" => crate::stability::clear(&app),
        "stab:export" => crate::stability::export(&app, &payload),
        "test:cancel" => json!(crate::tester::cancel()),
        "test:running" => json!(crate::tester::running()),
        "test:last" => crate::tester::last(&app),
        "service:status" => crate::service::status(),
        "service:remove" => crate::service::remove(),
        "service:dropDriver" => {
            if crate::tester::running() {return json!({"error":"сначала завершите тест стратегий"});}
            if crate::runtime::snapshot(app)["running"] == true { return json!({"error":"сначала выключите обход"}); }
            match crate::runtime::other_winws() {Ok(false)=>{},Ok(true)=>return json!({"error":"драйвер держит другая копия winws: закройте её"}),Err(e)=>return json!({"error":e})}
            crate::service::drop_windivert()
        }
        "ai:list" => {
            let cfg=read_cfg(&app);
            let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            let valid=engines::validate(std::path::Path::new(dir),None);
            let hosts=crate::hosts::current(&crate::hosts::hosts_path());
            let stale=if hosts.had_block {crate::hosts::find_stale_catalog(&crate::hosts::hosts_path()).count} else {0};
            let cache=app.path().app_data_dir().ok().and_then(|root|crate::ai::catalog(&root));
            let mut services=crate::ai::services();
            if let (Some(source),Some(items))=(&cache,services.as_array_mut()) { for svc in items { let domains:Vec<String>=svc["domains"].as_array().into_iter().flatten().filter_map(|v|v.as_str().map(str::to_owned)).collect();svc["proxyable"]=json!(crate::hosts::entries_for_domains(&source.unblock,&domains).len()); } }
            json!({"services":services,"bypass":cfg.get("aiBypass").cloned().unwrap_or(json!([])),"proxy":cfg.get("aiProxyServices").cloned().unwrap_or(json!([])),"blocklist":cfg.get("aiBlocklist").cloned().unwrap_or(json!(false)),"extra":cfg.get("aiExtra").cloned().unwrap_or(json!("")),"probeProxy":cfg.get("aiProbeProxy").cloned().unwrap_or(json!("")),"entry":cfg.get("aiEntry").cloned().unwrap_or(json!("")),"dirOk":valid.ok,"applied":0,"hostsApplied":hosts.proxies,"hostsBlocked":hosts.blocked,"hostsStale":stale,"sourceUpdated":cache.and_then(|p|p.updated)})
        }
        "ai:probe" => crate::ai::probe_services(&payload,&read_cfg(&app),&app),
        "ai:setProxy" => {
            let value=payload.as_str().unwrap_or("").trim();
            let cfg=merge(read_cfg(&app),&json!({"aiProbeProxy":value}));
            match write_cfg(&app,&cfg) {Ok(())=>json!(true),Err(e)=>json!({"error":e.to_string()})}
        }
        "ai:openHosts" => {
            let file=crate::hosts::hosts_path();
            let mut command=std::process::Command::new("explorer.exe");command.arg("/select,").arg(&file);
            match command.spawn(){Ok(_)=>json!(file),Err(e)=>json!({"error":e.to_string()})}
        }
        "ai:openList" => {
            let cfg=read_cfg(&app);let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");let file=std::path::Path::new(dir).join("lists/list-general-user.txt");
            if !engines::validate(std::path::Path::new(dir),None).ok{return json!({"error":"компоненты не найдены"})}
            let mut command=std::process::Command::new("explorer.exe");command.arg("/select,").arg(&file);match command.spawn(){Ok(_)=>json!(file),Err(e)=>json!({"error":e.to_string()})}
        }
        "ai:dropStale" => {
            let file=crate::hosts::hosts_path();let root=match app.path().app_data_dir(){Ok(p)=>p,Err(e)=>return json!({"error":e.to_string()})};
            if let Err(e)=fs::create_dir_all(&root){return json!({"error":e.to_string()})}
            let mut backup=match tempfile::Builder::new().prefix("hosts-before-clean-").suffix(".backup").tempfile_in(&root){Ok(f)=>f,Err(e)=>return json!({"error":e.to_string()})};
            let mut input=match fs::File::open(&file){Ok(f)=>f,Err(e)=>return json!({"error":e.to_string()})};
            if let Err(e)=std::io::copy(&mut input,&mut backup){return json!({"error":format!("не удалось сохранить копию hosts: {e}")})}
            let (_file,backup_path)=match backup.keep(){Ok(v)=>v,Err(e)=>return json!({"error":e.to_string()})};
            let services=crate::ai::services();let domains:Vec<String>=services.as_array().into_iter().flatten().flat_map(|s|s["domains"].as_array().into_iter().flatten()).filter_map(|d|d.as_str().map(str::to_owned)).collect();
            match crate::hosts::drop_stale(&file,&domains){Ok(removed)=>{if removed>0{crate::hosts::flush_dns();}json!({"removed":removed,"backup":backup_path})},Err(e)=>json!({"error":e.to_string(),"backup":backup_path})}
        }
        "ai:refreshSource" => {
            match app.path().app_data_dir().map_err(|e|e.to_string()).and_then(|root|crate::ai::refresh(&root)) {Ok(v)=>v,Err(e)=>json!({"error":e})}
        }
        "ai:restoreHosts" => {
            match app.path().app_data_dir().map_err(|e|e.to_string()).and_then(|root|crate::hosts::restore(&root,&crate::hosts::hosts_path())) {
                Ok(path)=>{crate::hosts::flush_dns();json!({"ok":true,"file":path})},Err(e)=>json!({"error":e})
            }
        }
        "ai:apply" => {
            let cfg=read_cfg(&app);let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            let valid=engines::validate(std::path::Path::new(dir),None);
            if !valid.ok {return json!({"error":valid.reason});}
            if crate::tester::running() {return json!({"error":"сначала завершите тест стратегий"});}
            let want_hosts=payload["proxy"].as_array().is_some_and(|a|!a.is_empty())||payload["blocklist"]==true;
            let root=match app.path().app_data_dir(){Ok(p)=>p,Err(e)=>return json!({"error":e.to_string()})};
            let own=payload["entry"].as_str().map(str::trim).unwrap_or("");
            if !own.is_empty() { if let Err(e)=crate::ai::own_entry(own){return json!({"error":e})} }
            // Своему прокси каталог нужен только ради поддоменов: без него обойдёмся доменами сервисов.
            let source=if want_hosts { match crate::ai::catalog(&root) {Some(s)=>Some(s),None=>{if let Err(e)=crate::ai::refresh(&root){if own.is_empty(){return json!({"error":e})}}crate::ai::catalog(&root)}} }else{None};
            let before=crate::runtime::snapshot(&app);
            let was_running=before["running"]==true;
            let previous=before["strategy"].as_str().map(str::to_owned);
            if was_running {crate::runtime::action(&app,&cfg,"stop",None);}
            let mut result=match crate::ai::apply(std::path::Path::new(dir),&payload) {
                Err(e)=>{if was_running {let restored=crate::runtime::action(&app,&cfg,"start",previous.as_deref());if restored["running"]!=true{return json!({"error":format!("{e}; прежнюю стратегию не удалось восстановить")});}}return json!({"error":e})},Ok(v)=>v
            };
            let has_existing=crate::hosts::current(&crate::hosts::hosts_path()).had_block;
            let mut hosts_ok=true;
            if want_hosts||has_existing {
                let source=source.unwrap_or_default();
                match crate::ai::apply_hosts(&payload,&root,&crate::hosts::hosts_path(),&source) {
                    Ok(mut h)=>{
                        crate::hosts::flush_dns();
                        let verified=crate::hosts::verify_hosts(&crate::hosts::hosts_path(),|host|crate::hosts::lookup(host,std::time::Duration::from_secs(3)));
                        h["verify"]=json!({"active":verified.active,"reason":verified.reason});result["hosts"]=h;
                    }
                    Err(e)=>{hosts_ok=false;result["hostsError"]=json!(e);}
                }
            }
            let mut patch=json!({"aiBypass":payload["bypass"],"aiExtra":payload["extra"]});
            if hosts_ok {patch["aiProxyServices"]=payload["proxy"].clone();patch["aiBlocklist"]=payload["blocklist"].clone();patch["aiEntry"]=json!(own);}
            let updated=merge(cfg,&patch);
            let save_error=write_cfg(&app,&updated).err();
            if was_running {
                let restarted=crate::runtime::action(&app,&updated,"start",previous.as_deref());
                result["restarted"]=json!(restarted["running"]==true);
                if restarted["running"]!=true {result["restartError"]=json!(restarted["error"].as_str().unwrap_or("не удалось запустить прежнюю стратегию"));}
            }
            if let Some(e)=save_error {result["settingsError"]=json!(e.to_string());}
            result
        }
        "tgws:openLink" => {
            let link = crate::tgws::snapshot(&app)["link"].as_str().map(str::to_owned);
            if let Some(ref uri) = link {
                let _ = std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", uri]).spawn();
            }
            json!(link)
        }
        "tgws:status" => crate::tgws::snapshot(&app),
        "tgws:check" => crate::tgws::check(app),
        // Встроенный обновляется только с PLASTO: «обновить» его — значит
        // перейти на скачанную Rust-версию, она новее.
        "tgws:update" => {
            let builtin = crate::tgws::flavor(app) == crate::tgws::Flavor::Builtin;
            if builtin {
                let cfg = merge(read_cfg(app), &json!({"tgwsFlavor": crate::tgws::Flavor::Rust.as_str()}));
                if let Err(e) = write_cfg(app, &cfg) { return json!({"error":e.to_string()}); }
            }
            let result = crate::tgws::update(app, tg_port(app));
            // Не скачалось — остаётся встроенный.
            if builtin && result["error"].is_string() {
                let _ = write_cfg(app, &merge(read_cfg(app), &json!({"tgwsFlavor": crate::tgws::Flavor::Builtin.as_str()})));
            }
            result
        }
        // Сменить версию прокси: встроенная, Rust или оригинал. Работающий перезапустится на ней.
        "tgws:flavor" => {
            let flavor = crate::tgws::Flavor::parse(payload["flavor"].as_str());
            let cfg = merge(read_cfg(app), &json!({"tgwsFlavor": flavor.as_str()}));
            if let Err(e) = write_cfg(app, &cfg) { return json!({"error":e.to_string()}); }
            crate::tgws::switch(app, tg_port(app))
        }
        // Остановить то, что мешает Telegram-прокси, и запустить его.
        "tgws:stopOther" => {
            let pid = payload["pid"].as_u64().unwrap_or(0) as u32;
            let port = tg_port(app);
            let known = [crate::conflict::other_tgws(None), Some(crate::conflict::port_owner(port))].into_iter().flatten().any(|o| o["pid"].as_u64() == Some(pid as u64));
            if !known { return json!({"error":"этот процесс уже не мешает: попробуйте включить ещё раз"}); }
            // Порт и секрет остановленного tg-ws-proxy достаются прокси PLASTO:
            // Telegram уже настроен на них и переподключится сам.
            let taken = crate::conflict::tgws_config(pid);
            let stopped = crate::conflict::stop_process(pid);
            if stopped["error"].is_string() { return stopped; }
            let port = match taken {
                Some((taken, secret)) => {
                    if taken != port {
                        let cfg = merge(read_cfg(app), &json!({"tgPort": taken}));
                        if let Err(e) = write_cfg(app, &cfg) { return json!({"error":e.to_string()}); }
                    }
                    if let Err(e) = crate::tgws::adopt(app, taken, &secret) { return json!({"error":e}); }
                    taken
                }
                None => port,
            };
            let result = crate::tgws::toggle(app, port);
            if !result["error"].is_string() { remember(app, "telegram", result["running"] == true); }
            result
        }
        // Запомнить другой порт и запустить на нём.
        "tgws:usePort" => {
            let Some(port) = payload["port"].as_u64().filter(|p| (1024..=65535).contains(p)) else { return json!({"error":"недопустимый порт"}); };
            let cfg = merge(read_cfg(app), &json!({"tgPort": port}));
            if let Err(e) = write_cfg(app, &cfg) { return json!({"error":e.to_string()}); }
            let result = crate::tgws::toggle(app, port as u16);
            if !result["error"].is_string() { remember(app, "telegram", result["running"] == true); }
            result
        }
        "conflict:stopWinws" => {
            if crate::tester::running() { return json!({"error":"дождитесь окончания теста или отмените его"}); }
            let own = crate::runtime::snapshot(app)["pid"].as_u64().map(|p| p as u32);
            crate::conflict::stop_winws(own, payload["disable"] == true)
        }
        "app:check" => crate::selfupdate::check(beta_channel(app)),
        "app:download" => match app.path().app_data_dir() {
            Ok(root) => crate::selfupdate::download(&root.join("update"), app, beta_channel(app)).unwrap_or_else(|e| json!({"error":e})),
            Err(e) => json!({"error":e.to_string()}),
        },
        "engine:restart" => {
            if crate::tester::running() { return json!({"error":"дождитесь окончания теста или отмените его"}); }
            let cfg = read_cfg(app);
            if crate::runtime::snapshot(app)["running"] != true { return crate::runtime::snapshot(app); }
            crate::runtime::action(app, &cfg, "stop", None);
            crate::runtime::action(app, &cfg, "start", None)
        }
        "tgws:install" => crate::tgws::install(&app),
        "tgws:logs" => crate::tgws::logs(&app),
        "tgws:toggle" => {
            let result = crate::tgws::toggle(app, tg_port(app));
            if !result["error"].is_string() { remember(app, "telegram", result["running"] == true); }
            result
        }
        "zapret:check" => {
            match crate::updater::latest() {
                Ok(release) => {
                    let cfg=read_cfg(&app);
                    let installed=cfg.get("zapretTag").and_then(Value::as_str);
                    json!({"installed":installed,"latest":release["tag"],"hasUpdate":installed != release["tag"].as_str(),"release":release})
                }, Err(e) => json!({"error":e})
            }
        }
        "zapret:install" => {
            static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if INSTALLING.swap(true, std::sync::atomic::Ordering::AcqRel) { return json!({"ok":false,"error":"установка уже выполняется"}); }
            let result = match components_dir(&app) {
                Err(e) => Err(e),
                Ok(root) => crate::updater::install(&root.join("zapret"), &app),
            };
            INSTALLING.store(false, std::sync::atomic::Ordering::Release);
            match result {
                Err(e) => json!({"ok":false,"error":e}),
                Ok(result) => {
                    // Свои списки и Game Filter — из прежней сборки в новую.
                    let before=read_cfg(&app);
                    if let (Some(old), Some(new)) = (before.get("zapretDir").and_then(Value::as_str), result["dir"].as_str()) {
                        crate::bundled::carry_user_files(std::path::Path::new(old), std::path::Path::new(new));
                    }
                    let cfg=merge(before, &json!({"zapretDir":result["dir"],"zapretTag":result["tag"]}));
                    if let Err(e)=write_cfg(&app,&cfg) { return json!({"ok":false,"error":format!("компоненты скачаны, но настройки не сохранены: {e}")}); }
                    let _=app.emit("plasto:zapret:installed",result.clone());
                    result
                }
            }
        }
        "components:openDir" => {
            let dir=match components_dir(&app){Ok(d)=>d,Err(e)=>return json!({"error":e})};
            if let Err(e)=fs::create_dir_all(&dir){return json!({"error":format!("папка компонентов недоступна: {e}")})}
            match std::process::Command::new("explorer.exe").arg(&dir).spawn(){Ok(_)=>Value::Null,Err(e)=>json!({"error":e.to_string()})}
        }
        "components:pickDir" => match pick_folder("Папка компонентов PLASTO") {
            Err(e) => json!({"error":e}),
            Ok(None) => Value::Null,
            Ok(Some(dir)) => move_components(&app, PathBuf::from(dir)),
        },
        "shell:open" => {
            let address=payload.as_str().unwrap_or("");
            match url::Url::parse(address) {
                Ok(u) if ["http","https"].contains(&u.scheme()) && u.host_str().is_some() && u.username().is_empty() && u.password().is_none() => {
                    match std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler",u.as_str()]).spawn(){Ok(_)=>Value::Null,Err(e)=>json!({"error":e.to_string()})}
                },_=>json!({"error":"поддерживаются только обычные HTTP(S)-ссылки"})
            }
        }
        "zapret:pickDir" => {
            match pick_folder("Выберите папку с распакованным zapret") {
                Err(e) => json!({"error":e,"status":{"ok":false,"reason":e}}),
                Ok(None) => Value::Null,
                Ok(Some(dir)) => {
                    let cfg = read_cfg(app);
                    let mode = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
                    let valid = engines::validate(std::path::Path::new(&dir), mode);
                    if valid.ok {
                        let updated = merge(cfg, &json!({"zapretDir":dir, "zapretTag":null}));
                        if let Err(e) = write_cfg(&app, &updated) { return json!({"error":e.to_string(),"status":{"ok":false,"reason":e.to_string()}}); }
                    }
                    json!({"dir":dir,"status":valid.to_json(),"strategies":valid.presets.iter().map(|p|p.to_json()).collect::<Vec<_>>()})
                }
            }
        }
        "engine:setMode" => {
            let mode = payload.as_str().and_then(Engine::parse);
            if !payload.is_null() && payload.as_str().is_some_and(|v| !v.is_empty()) && mode.is_none() { return json!({"ok":false,"reason":"неизвестный движок","presets":[]}); }
            let cfg = merge(read_cfg(&app), &json!({"engineMode": mode.map(Engine::as_str)}));
            if let Err(e)=write_cfg(&app,&cfg) { return json!({"ok":false,"reason":e.to_string(),"presets":[]}); }
            let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            engines::validate(std::path::Path::new(dir),mode).to_json()
        }
        "engine:applyStrategy" => {
            let file=payload.as_str().unwrap_or("");
            let cfg=read_cfg(&app);
            let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            let mode=cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
            let valid=engines::validate(std::path::Path::new(dir),mode);
            if !valid.presets.iter().any(|p|p.file==file) { return json!({"error":"стратегия не найдена"}); }
            let cfg=merge(cfg,&json!({"strategy":file}));
            if let Err(e)=write_cfg(&app,&cfg) { return json!({"error":e.to_string()}); }
            if crate::runtime::snapshot(&app)["running"] == true {
                crate::runtime::action(&app,&cfg,"stop",None);
                crate::runtime::action(&app,&cfg,"start",Some(file))
            } else { crate::runtime::snapshot(&app) }
        }
        "engine:gameFilter" => {
            let mode=crate::strategy::GameMode::from_value(&payload);
            let cfg=merge(read_cfg(&app),&json!({"gameFilter":mode.as_str()}));
            if let Err(e)=write_cfg(&app,&cfg) { return json!({"error":e.to_string()}); }
            if let Some(dir)=cfg.get("zapretDir").and_then(Value::as_str) { crate::strategy::write_game_mode(std::path::Path::new(dir),mode); }
            crate::runtime::snapshot(&app)
        }
        "engine:detect" => {
            let cfg = read_cfg(app);
            let dir = cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            let det = engines::detect_engine(std::path::Path::new(dir));
            json!({ "engine": det.engine.map(Engine::as_str), "exe": det.exe, "reason": det.reason })
        }
        "engine:preview" => {
            let cfg = read_cfg(app);
            let dir = cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");
            let file = payload.as_str().or_else(|| cfg.get("strategy").and_then(Value::as_str)).unwrap_or("");
            let mode = cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);
            let valid = engines::validate(std::path::Path::new(dir), mode);
            if !valid.presets.iter().any(|p| p.file == file) {
                json!({ "error": "стратегия не найдена в выбранной папке" })
            } else {
                let mut opts = BuildOpts::default();
                opts.engine = mode;
                opts.parse.game_filter = cfg.get("gameFilter").map(crate::strategy::GameMode::from_value);
                match engines::build_command(std::path::Path::new(dir), file, &opts) {
                    Ok(plan) => json!({ "exe": plan.exe, "args": plan.args, "command": format!("{} {}", plan.exe.display(), plan.args.join(" ")) }),
                    Err(e) => json!({ "error": e }),
                }
            }
        }
        "dns:list" | "dns:apply" | "dns:restore" | "dns:addCustom" | "dns:removeCustom" | "dns:check" => {
            match app.path().app_config_dir() {
                Err(e) => json!({"error": e.to_string()}),
                Ok(dir) => {
                    if let Err(e) = fs::create_dir_all(&dir) { return json!({"error":e.to_string()}); }
                    let runner = crate::dns::SystemRunner;
                    match name {
                        "dns:list" => {
                            let adapters = crate::dns::adapters(&runner);
                            let presets = crate::dns::all_presets(Some(&dir));
                            let active = if adapters.ok { crate::dns::match_preset(&adapters.adapters, &presets) } else { None };
                            json!({"presets":presets,"adapters":adapters.adapters,"active":active,"hasBackup":dir.join("dns.backup.json").exists(),"error":adapters.error})
                        }
                        "dns:apply" => crate::dns::apply(payload.as_str().unwrap_or(""), &crate::dns::ApplyOpts { user_data_dir: Some(&dir), check: None, build: None }, &runner),
                        "dns:restore" => match crate::dns::restore(&dir, &runner) { Ok(adapters) => json!({"adapters":adapters}), Err(e) => json!({"error":e}) },
                        "dns:check" => {
                            // {"ids": [...]} — только эти; без них — все. {"current": id} — ещё и куда переключиться.
                            let all = crate::dns::all_presets(Some(&dir));
                            let ids: Option<Vec<&str>> = payload.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect());
                            let list: Vec<_> = all.iter().filter(|p| ids.as_ref().is_none_or(|ids| ids.contains(&p.id.as_str()))).cloned().collect();
                            let build = crate::dns::win_build(&runner);
                            let (health, online) = crate::dns::check_all(&list, build);
                            let next = payload.get("current").and_then(Value::as_str).and_then(|cur| crate::dns::next_preset(&all, &health, cur, build));
                            let results: Map<String, Value> = health.iter().map(|(id, h)| (id.clone(), json!(h))).collect();
                            json!({"results":results,"online":online,"next":next})
                        }
                        "dns:addCustom" => {
                            let n = payload.get("name").and_then(Value::as_str).unwrap_or("");
                            let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
                            match crate::dns::real_probes_run(|probes| crate::dns::add_custom(n, text, &dir, probes)) { Ok(preset) => json!({"preset":preset}), Err(e) => json!({"error":e}) }
                        }
                        _ => match crate::dns::remove_custom(payload.as_str().unwrap_or(""), &dir) { Ok(id) => json!({"id":id}), Err(e) => json!({"error":e}) },
                    }
                }
            }
        }
        "config:get" => Value::Object(read_cfg(&app)),
        "config:set" => {
            let cfg = merge(read_cfg(&app), &payload);
            if let Err(e) = write_cfg(&app, &cfg) {
                eprintln!("[plasto] конфиг не записан: {e}");
            }
            Value::Object(cfg)
        }


        other => {
            eprintln!("[plasto] неизвестный канал: {other}");
            json!({ "error": format!("неизвестная команда: {other}") })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn merge_перекрывает_и_сохраняет_остальное() {
        let out = merge(obj(json!({ "a": 1, "b": 2 })), &json!({ "b": 3, "c": 4 }));
        assert_eq!(Value::Object(out), json!({ "a": 1, "b": 3, "c": 4 }));
    }

    #[test]
    fn merge_испорченный_патч_конфиг_не_стирает() {
        for bad in [Value::Null, json!("строка"), json!([1, 2]), json!(5)] {
            let out = merge(obj(json!({ "a": 1 })), &bad);
            assert_eq!(Value::Object(out), json!({ "a": 1 }));
        }
    }

    #[test]
    fn merge_вложенное_заменяется_целиком_а_не_сливается() {
        // Поведение сознательно совпадает с Object.assign в main.js: вложенных
        // конфигов в PLASTO нет, а глубокое слияние скрыло бы удаление ключа.
        let out = merge(obj(json!({ "x": { "a": 1, "b": 2 } })), &json!({ "x": { "a": 9 } }));
        assert_eq!(Value::Object(out), json!({ "x": { "a": 9 } }));
    }
}
