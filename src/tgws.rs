use std::{fs, io::{Read, Write, BufRead, BufReader}, path::Path, process::{Child, Command, Stdio}, sync::{Arc,Mutex}, time::{Duration,Instant,SystemTime,UNIX_EPOCH}};
use serde_json::{json,Value};
use sha2::{Sha256,Digest};
use crate::host::Host;
use regex_lite::Regex;
/// Какой tg-ws-proxy запускать: встроенный (tg-ws-proxy-rs как библиотека,
/// внутри PLASTO), или скачанный отдельной программой — оригинал Flowseal
/// (Python в сборке PyInstaller, со своим значком в трее) либо порт на Rust от
/// valnesfjord. Скачанные лежат в разных папках; секрет у всех общий.
#[derive(Clone,Copy,PartialEq,Eq,Debug,Default)]
pub enum Flavor { #[default] Builtin, Original, Rust }
impl Flavor {
 pub fn parse(s:Option<&str>)->Self { match s {Some("original")=>Flavor::Original,Some("rust")=>Flavor::Rust,_=>Flavor::Builtin} }
 pub fn as_str(self)->&'static str { match self {Flavor::Builtin=>"builtin",Flavor::Original=>"original",Flavor::Rust=>"rust"} }
 /// Встроенный сверяется с релизами Rust-версии: это она и есть.
 fn repo(self)->&'static str { match self {Flavor::Original=>"Flowseal/tg-ws-proxy",Flavor::Rust|Flavor::Builtin=>"valnesfjord/tg-ws-proxy-rs"} }
 fn asset(self)->&'static str { match self {Flavor::Original=>"TgWsProxy_windows.exe",Flavor::Rust|Flavor::Builtin=>"tg-ws-proxy-x86_64-pc-windows-gnu.zip"} }
 fn dir(self)->&'static str { match self {Flavor::Original=>"tgws",Flavor::Rust|Flavor::Builtin=>"tgws-rs"} }
}
/// Версия встроенного tg-ws-proxy-rs (из Cargo.lock, см. build.rs).
pub const BUILTIN:&str=env!("TGWS_BUILTIN_VERSION");
fn builtin_meta()->Value { json!({"tag":format!("v{BUILTIN}"),"builtin":true}) }
/// Версия из настроек PLASTO (`tgwsFlavor`).
pub fn flavor(app:&Host)->Flavor { Flavor::parse(crate::api::read_cfg(app).get("tgwsFlavor").and_then(Value::as_str)) }
/// Работающий прокси: отдельный процесс или встроенный, в своём потоке.
enum Run { Child(Child), Inside(Inside) }
struct Inside { stop:tokio::sync::watch::Sender<bool>, thread:std::thread::JoinHandle<()> }
impl Run {
 fn pid(&self)->Option<u32> { match self {Run::Child(c)=>Some(c.id()),Run::Inside(_)=>None} }
 fn exited(&mut self)->bool { match self {Run::Child(c)=>c.try_wait().ok().flatten().is_some(),Run::Inside(i)=>i.thread.is_finished()} }
}
#[derive(Default)]
pub struct Proxy { run:Option<Run>, secret:Option<String>, port:u16, installed:Option<Value>, flavor:Flavor, generation:u64 }
pub type State=Arc<Mutex<Proxy>>;
static DOWNLOADING:std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
static EXITING:std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
/// Журнал прокси отдельно от состояния: встроенный пишет в него из своего
/// потока, и остановка, которая ждёт этот поток под замком состояния, не
/// встанет в взаимную блокировку.
static LOGS:Mutex<Vec<Value>>=Mutex::new(Vec::new());
fn log_line(line:&str) {
 static SECRET:std::sync::OnceLock<Regex>=std::sync::OnceLock::new();
 let re=SECRET.get_or_init(||Regex::new(r"(?i)secret[\s:=]*([0-9a-f]{32,})").unwrap());
 let clean=re.replace_all(line,"secret [скрыт]").into_owned();
 if let Ok(mut logs)=LOGS.lock(){logs.push(json!({"level":"out","message":clean}));if logs.len()>200{logs.remove(0);}}
}
struct DownloadGuard;
impl Drop for DownloadGuard {fn drop(&mut self){DOWNLOADING.store(false,std::sync::atomic::Ordering::Release)}}
impl Drop for Proxy { fn drop(&mut self) { if let Some(r)=self.run.take() { stop(r); } } }
fn status(p:&Proxy)->Value { let port=if p.port==0{DEFAULT_PORT}else{p.port}; let link=p.secret.as_ref().filter(|_|p.run.is_some()).map(|s|format!("tg://proxy?server=127.0.0.1&port={port}&secret=dd{s}")); json!({"running":p.run.is_some(),"pid":p.run.as_ref().and_then(Run::pid),"host":"127.0.0.1","port":port,"secret":p.secret,"link":link,"installed":p.installed,"flavor":p.flavor.as_str()}) }
/// Свой прокси: процесс — вместе с дочерним процессом PyInstaller, который
/// держит порт; встроенный — сигнал остановки и ожидание потока (рантайм
/// сворачивается не дольше пары секунд).
fn stop(run:Run) { match run {
 Run::Child(mut c)=>{let _=crate::conflict::kill(c.id() as u64);let _=c.kill();let _=c.wait();}
 Run::Inside(i)=>{let _=i.stop.send(true);let _=i.thread.join();}
} }
pub fn shutdown(app:&Host) { EXITING.store(true,std::sync::atomic::Ordering::Release); let state=app.state::<State>(); if let Ok(mut p)=state.lock(){p.generation+=1;if let Some(r)=p.run.take(){stop(r);}}; }
pub fn logs(_app:&Host)->Value {LOGS.lock().map(|l|json!(*l)).unwrap_or(json!([]))}
pub fn snapshot(app:&Host)->Value { app.state::<State>().lock().map(|p|status(&p)).unwrap_or(Value::Null) }
/// Журнал встроенного прокси — в общий журнал PLASTO, только его записи.
fn install_logging() {
 static ONCE:std::sync::Once=std::sync::Once::new();
 ONCE.call_once(||{
  use tracing_subscriber::{layer::SubscriberExt,Layer};
  let layer=tracing_subscriber::fmt::layer().with_ansi(false).with_target(false).without_time().with_writer(||LineWriter(Vec::new()))
   .with_filter(tracing_subscriber::filter::Targets::new().with_target("tg_ws_proxy_rs",tracing::Level::INFO));
  let _=tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer));
 });
}
/// Строки события tracing → журнал прокси.
struct LineWriter(Vec<u8>);
impl Write for LineWriter { fn write(&mut self,b:&[u8])->std::io::Result<usize>{self.0.extend_from_slice(b);Ok(b.len())} fn flush(&mut self)->std::io::Result<()>{Ok(())} }
impl Drop for LineWriter { fn drop(&mut self) { for line in String::from_utf8_lossy(&self.0).lines().map(str::trim_end).filter(|l|!l.is_empty()) { log_line(line); } } }
/// Запустить встроенный прокси в своём потоке и дождаться, пока он займёт
/// порт или не сможет.
fn start_inside(port:u16,secret:&str)->Result<Inside,String> {
 // `--default-domains`: запасной путь через Cloudflare, как у Flowseal; прямые
 // WebSocket-адреса Telegram в России заблокированы.
 let config=tg_ws_proxy_rs::config::Config::try_from_cli_line(&format!("--host 127.0.0.1 --link-ip 127.0.0.1 --port {port} --secret {secret} --default-domains"))?;
 install_logging();
 let (stop,mut rx)=tokio::sync::watch::channel(false);
 let (ready_tx,ready)=std::sync::mpsc::channel::<Result<(),String>>();
 let thread=std::thread::Builder::new().name("tg-ws-proxy".into()).spawn(move||{
  // Двух рабочих потоков хватает: так же делает Android-обёртка автора.
  let rt=match tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("tg-ws-worker").enable_all().build(){Ok(rt)=>rt,Err(e)=>{let _=ready_tx.send(Err(e.to_string()));return}};
  let listening=ready_tx.clone();
  let result=rt.block_on(tg_ws_proxy_rs::server::run_with_listen(config,async move{let _=rx.wait_for(|s|*s).await;},move|_|{let _=listening.send(Ok(()));}));
  rt.shutdown_timeout(Duration::from_secs(2));
  if let Err(e)=result {log_line(&format!("прокси остановился с ошибкой: {e}"));let _=ready_tx.send(Err(e.to_string()));}
 }).map_err(|e|e.to_string())?;
 match ready.recv_timeout(Duration::from_secs(5)) {
  Ok(Err(e))=>{let _=thread.join();Err(format!("встроенный прокси не запустился: {e}"))}
  _=>Ok(Inside{stop,thread}),
 }
}
fn cached(root:&Path)->Option<Value> {
 let value:Value=serde_json::from_str(&fs::read_to_string(root.join("installed.json")).ok()?).ok()?;
 let digest=value["sha256"].as_str()?;if digest.len()!=64||!digest.chars().all(|c|c.is_ascii_hexdigit()){return None}
 let exe=root.join(format!("tgws-{}.exe",&digest[..16]));let mut input=fs::File::open(&exe).ok()?;let mut hash=Sha256::new();std::io::copy(&mut input,&mut hash).ok()?;
 if format!("{:x}",hash.finalize())!=digest.to_lowercase(){return None}
 Some(json!({"exe":exe,"tag":value["tag"]}))
}
fn agent()->Result<ureq::Agent,String>{let tls=native_tls::TlsConnector::new().map_err(|e|e.to_string())?;Ok(ureq::AgentBuilder::new().tls_connector(Arc::new(tls)).build())}
fn latest(agent:&ureq::Agent,flavor:Flavor)->Result<Value,String>{
 let mut text=String::new(); agent.get(&format!("https://api.github.com/repos/{}/releases/latest",flavor.repo())).set("User-Agent","PLASTO-desktop").timeout(Duration::from_secs(30)).call().map_err(|e|e.to_string())?.into_reader().take(2_000_000).read_to_string(&mut text).map_err(|e|e.to_string())?;
 serde_json::from_str(&text).map_err(|e|e.to_string())
}
/// `fresh` — сверить с последним релизом, даже если прокси уже скачан.
fn download(root:&Path,flavor:Flavor,app:&Host,fresh:bool)->Result<Value,String> {
 let have=cached(root);
 if let (Some(meta),false)=(&have,fresh){return Ok(meta.clone())}
 let agent=agent()?;
 let rel=latest(&agent,flavor)?;
 if let Some(meta)=have.filter(|m|m["tag"]==rel["tag_name"]){return Ok(meta)}
 let asset=rel["assets"].as_array().and_then(|a|a.iter().find(|x|x["name"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(flavor.asset())))).ok_or("нет Windows-версии Telegram-прокси")?;
 let url=asset["browser_download_url"].as_str().filter(|s|s.starts_with(&format!("https://github.com/{}/releases/download/",flavor.repo()))).ok_or("неверный адрес прокси")?;
 let digest=asset["digest"].as_str().and_then(|x|x.strip_prefix("sha256:")).filter(|x|x.len()==64&&x.chars().all(|c|c.is_ascii_hexdigit())).ok_or("нет контрольной суммы прокси")?;
 let size=asset["size"].as_u64().filter(|x|*x>0&&*x<100*1024*1024).ok_or("недопустимый размер прокси")?;
 fs::create_dir_all(root).map_err(|e|e.to_string())?; let mut tmp=tempfile::NamedTempFile::new_in(root).map_err(|e|e.to_string())?;
 let mut stream=agent.get(url).set("User-Agent","PLASTO-desktop").timeout(Duration::from_secs(120)).call().map_err(|e|format!("загрузка прокси: {e}"))?.into_reader().take(100*1024*1024+1);
 let mut hasher=Sha256::new(); let mut count=0u64; let mut buf=[0;32768]; loop { let n=stream.read(&mut buf).map_err(|e|e.to_string())?; if n==0{break} count+=n as u64; if count>size{return Err("размер прокси превышен".into())} hasher.update(&buf[..n]); tmp.write_all(&buf[..n]).map_err(|e|e.to_string())?; let _=app.emit("plasto:tgws:progress",json!({"stage":"download","detail":{"received":count,"size":size}})); }
 if count!=size||format!("{:x}",hasher.finalize())!=digest.to_lowercase(){return Err("контрольная сумма прокси не совпадает".into())}
 // Rust-версия приходит в zip: дальше хранится сам exe под своей суммой.
 let (tmp,digest)=if flavor==Flavor::Rust {unpack(root,&tmp)?} else {(tmp,digest.to_lowercase())};
 let digest=digest.as_str();
 let exe=root.join(format!("tgws-{}.exe",&digest[..16])); if exe.exists() { let mut existing=fs::File::open(&exe).map_err(|e|e.to_string())?;let mut h=Sha256::new();std::io::copy(&mut existing,&mut h).map_err(|e|e.to_string())?;if format!("{:x}",h.finalize())!=digest.to_lowercase(){return Err("имеющийся файл прокси повреждён; повторная установка отменена".into())} } else { tmp.persist(&exe).map_err(|e|e.to_string())?; }
 let meta=json!({"tag":rel["tag_name"],"sha256":digest});
 let mut marker=tempfile::NamedTempFile::new_in(root).map_err(|e|e.to_string())?;marker.write_all(meta.to_string().as_bytes()).map_err(|e|e.to_string())?;marker.persist(root.join("installed.json")).map_err(|e|e.to_string())?;
 Ok(json!({"exe":exe,"tag":rel["tag_name"]}))
}
/// Достать `tg-ws-proxy.exe` из проверенного zip. Возвращает файл и его SHA-256.
fn unpack(root:&Path,archive:&tempfile::NamedTempFile)->Result<(tempfile::NamedTempFile,String),String> {
 let mut zip=zip::ZipArchive::new(archive.reopen().map_err(|e|e.to_string())?).map_err(|e|format!("архив прокси: {e}"))?;
 let mut item=zip.by_name("tg-ws-proxy.exe").map_err(|_|"в архиве нет tg-ws-proxy.exe".to_string())?;
 if item.size()>100*1024*1024 {return Err("недопустимый размер прокси".into())}
 let mut out=tempfile::NamedTempFile::new_in(root).map_err(|e|e.to_string())?;
 let mut hasher=Sha256::new(); let mut buf=[0;32768]; let mut count=0u64;
 loop { let n=item.read(&mut buf).map_err(|e|format!("архив прокси: {e}"))?; if n==0{break} count+=n as u64; if count>100*1024*1024{return Err("размер прокси превышен".into())} hasher.update(&buf[..n]); out.write_all(&buf[..n]).map_err(|e|e.to_string())?; }
 Ok((out,format!("{:x}",hasher.finalize())))
}
fn root(app:&Host,flavor:Flavor)->Result<std::path::PathBuf,String> { crate::api::components_dir(app).map(|p|p.join(flavor.dir())) }
fn fetch(app:&Host,flavor:Flavor,fresh:bool)->Value {
 if flavor==Flavor::Builtin {return builtin_meta()}
 if DOWNLOADING.swap(true,std::sync::atomic::Ordering::AcqRel){return json!({"error":"загрузка Telegram-прокси уже выполняется"})}let _guard=DownloadGuard;
 let root=match root(app,flavor){Ok(p)=>p,Err(e)=>return json!({"error":e})};
 match download(&root,flavor,app,fresh){Ok(meta)=>{if let Ok(mut p)=app.state::<State>().lock(){if p.run.is_none()||p.flavor==flavor{p.installed=Some(meta.clone());p.flavor=flavor}}meta},Err(e)=>json!({"error":e})}
}
pub fn install(app:&Host)->Value { fetch(app,flavor(app),false) }
/// Номер версии из тега вида `v2.5.8`: для сравнения «новее ли».
fn version(tag:&str)->Vec<u64> { tag.trim_start_matches('v').split(|c:char|!c.is_ascii_digit()).take(3).map(|n|n.parse().unwrap_or(0)).collect() }
/// Есть ли версия новее скачанной. Не скачанный прокси обновлять нечего:
/// он скачается последним при первом включении. Встроенный обновляется
/// только с PLASTO; если вышла Rust-версия новее, её можно скачать отдельно
/// (`hasUpdate`). Скачанной Rust-версии, которую встроенный догнал,
/// предлагается вернуться на встроенный (`builtinCaughtUp`).
pub fn check(app:&Host)->Value {
 let flavor=flavor(app);
 let installed=if flavor==Flavor::Builtin {Some(format!("v{BUILTIN}"))} else {root(app,flavor).ok().and_then(|r|cached(&r)).and_then(|m|m["tag"].as_str().map(str::to_owned))};
 let caught_up=flavor==Flavor::Rust&&installed.as_deref().is_some_and(|i|version(i)<=version(BUILTIN));
 match agent().and_then(|a|latest(&a,flavor)) {
  Ok(rel)=>{
   let tag=rel["tag_name"].as_str().unwrap_or("");
   let newer=match flavor {Flavor::Builtin=>version(tag)>version(BUILTIN),_=>installed.as_deref().is_some_and(|i|i!=tag)};
   json!({"installed":installed,"latest":tag,"hasUpdate":newer,"builtin":flavor==Flavor::Builtin,"builtinCaughtUp":caught_up})
  }
  Err(e)=>json!({"installed":installed,"error":e,"builtinCaughtUp":caught_up})
 }
}
/// Скачать последнюю версию; если прокси работал — перезапустить на ней.
/// Встроенный так не обновить: вместо него ставится скачанная Rust-версия
/// (настройку `tgwsFlavor` переключает вызывающий).
pub fn update(app:&Host,port:u16)->Value {
 let mut meta=fetch(app,flavor(app),true);if meta["error"].is_string(){return meta}
 if let Err(e)=restart(app,port){return e}
 meta["flavor"]=json!(flavor(app).as_str());
 meta
}
/// Перезапустить, если работает: так подхватываются новая версия и смена версии.
fn restart(app:&Host,port:u16)->Result<(),Value> {
 if snapshot(app)["running"]==true {let off=toggle(app,port);if off["error"].is_string(){return Err(off)}let on=toggle(app,port);if on["error"].is_string(){return Err(on)}}
 Ok(())
}
/// Сменить версию прокси (она уже записана в настройки). Работающий прокси
/// перезапускается на новой версии, при надобности она скачивается.
pub fn switch(app:&Host,port:u16)->Value {
 if let Err(e)=restart(app,port){return e}
 // Выключенный показывает версию, которая запустится: скачанную или никакую.
 let flavor=flavor(app);let installed=if flavor==Flavor::Builtin {Some(builtin_meta())} else {root(app,flavor).ok().and_then(|r|cached(&r))};
 let state=app.state::<State>();let Ok(mut p)=state.lock() else{return json!({"error":"ошибка состояния прокси"})};
 if p.run.is_none(){p.flavor=flavor;p.installed=installed;}
 let v=status(&p);let _=app.emit("plasto:tgws:state",&v);v
}
pub const DEFAULT_PORT:u16=1443;
/// Секрет прокси: 16 случайных байт в hex, как его делает сам tg-ws-proxy.
/// Ключи `RandomState` берутся из системного генератора случайных чисел.
fn new_secret()->String { use std::hash::{BuildHasher,Hasher}; (0..2).map(|i|{let mut h=std::collections::hash_map::RandomState::new().build_hasher();h.write_u64(i);h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());format!("{:016x}",h.finish())}).collect() }
fn valid_secret(s:&str)->bool { s.len()==32&&s.chars().all(|c|c.is_ascii_hexdigit()) }
/// Свой конфиг PLASTO для tg-ws-proxy (запуск с `--portable`, рядом с exe):
/// порт из настроек PLASTO, постоянный секрет, без своих обновлений,
/// автозапуска и окна первого запуска. Секрет при первом создании берётся из
/// общего конфига tg-ws-proxy, чтобы прежняя ссылка в Telegram продолжила
/// работать; `adopt` — секрет остановленного чужого прокси, он важнее.
/// Секрет общий для всех версий, встроенной тоже. Возвращает секрет.
fn prepare(root:&Path,port:u16,adopt:Option<&str>)->Result<String,String> {
 let dir=root.join("TgWsProxy_data"); fs::create_dir_all(&dir).map_err(|e|e.to_string())?;
 let file=dir.join("config.json");
 let read=|p:&Path|fs::read_to_string(p).ok().and_then(|t|serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object);
 let shared=std::env::var_os("APPDATA").map(|a|Path::new(&a).join("TgWsProxy").join("config.json"));
 let mut cfg=read(&file).or_else(||shared.as_deref().and_then(read)).unwrap_or(json!({}));
 let secret=adopt.filter(|s|valid_secret(s)).or(cfg["secret"].as_str().filter(|s|valid_secret(s))).map(str::to_owned).unwrap_or_else(new_secret);
 cfg["secret"]=json!(secret);cfg["host"]=json!("127.0.0.1");cfg["port"]=json!(port);cfg["check_updates"]=json!(false);cfg["autostart"]=json!(false);
 fs::write(&file,serde_json::to_string_pretty(&cfg).unwrap_or_default()).map_err(|e|format!("конфиг прокси не записан: {e}"))?;
 for marker in [".first_run_done_mtproto",".ipv6_warned"] { let _=fs::OpenOptions::new().create(true).append(true).open(dir.join(marker)); }
 Ok(secret)
}
/// Взять секрет остановленного чужого прокси: ссылка, уже добавленная в
/// Telegram, продолжит работать с прокси PLASTO.
pub fn adopt(app:&Host,port:u16,secret:&str)->Result<(),String> { prepare(&root(app,Flavor::Original)?,port,Some(secret)).map(|_|()) }
/// Что мешает запуску: другой Flowseal tg-ws-proxy (две его копии не
/// уживаются, порт тут не поможет) или чужая программа на порту — тогда можно
/// взять другой порт. Встроенному и Rust-версии важен только порт.
fn obstacle(port:u16,flavor:Flavor)->Option<Value> {
 if flavor==Flavor::Original { if let Some(other)=crate::conflict::other_tgws(None) { return Some(json!({"error":"Telegram-прокси уже запущен отдельно от PLASTO","conflict":"tgws","owner":other,"port":port})); } }
 let busy=std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127,0,0,1],port)),Duration::from_millis(300)).is_ok()||std::net::TcpListener::bind(("127.0.0.1",port)).is_err();
 if !busy { return None; }
 let owner=crate::conflict::port_owner(port);
 Some(json!({"error":format!("порт {port} уже занят"),"conflict":"port","owner":owner,"port":port,"free":crate::conflict::free_port(port)}))
}
pub fn toggle(app:&Host,port:u16)->Value {
 let state=app.state::<State>(); let Ok(mut p)=state.lock() else{return json!({"error":"ошибка состояния прокси"})};
 if let Some(r)=p.run.take() {p.generation+=1; stop(r);p.secret=None;let v=status(&p);let _=app.emit("plasto:tgws:state",&v);return v;}
 drop(p);
 let flavor=flavor(app);
 if let Some(stop)=obstacle(port,flavor) { return stop; }
 let (root,shared)=match (root(app,flavor),root(app,Flavor::Original)){(Ok(r),Ok(s))=>(r,s),(Err(e),_)|(_,Err(e))=>return json!({"error":e})};
 let meta=fetch(app,flavor,false);if meta["error"].is_string(){return meta}
 // Секрет один на все версии: смена версии не ломает ссылку в Telegram.
 let secret=match prepare(&shared,port,None){Ok(s)=>s,Err(e)=>return json!({"error":e})};
 if EXITING.load(std::sync::atomic::Ordering::Acquire){return json!({"error":"приложение закрывается"})}
 let Ok(mut p)=state.lock() else {return json!({"error":"ошибка состояния прокси"})};
 if EXITING.load(std::sync::atomic::Ordering::Acquire){return json!({"error":"приложение закрывается"})}
 if p.run.is_some(){return status(&p)}
 p.generation+=1;let generation=p.generation;
 let run=if flavor==Flavor::Builtin {
  match start_inside(port,&secret){Ok(i)=>Run::Inside(i),Err(e)=>return json!({"error":e})}
 } else {
  let exe=meta["exe"].as_str().unwrap_or("");
  let mut cmd=Command::new(exe);cmd.current_dir(&root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
  match flavor {
   // Секрет через окружение, а не аргументом: так его не видно в списке процессов.
   Flavor::Rust=>{cmd.args(["--host","127.0.0.1","--link-ip","127.0.0.1","--port",&port.to_string(),"--default-domains"]).env("TG_SECRET",&secret);}
   _=>{cmd.arg("--portable");}
  }
  #[cfg(windows)] {use std::os::windows::process::CommandExt;cmd.creation_flags(0x08000000);}
  let mut child=match cmd.spawn(){Ok(c)=>c,Err(e)=>return json!({"error":e.to_string()})};
  // Не переживёт PLASTO, даже если тот упадёт: снимет Windows.
  crate::platform::bind_to_app(&child);
  for pipe in [child.stdout.take().map(|p|Box::new(p) as Box<dyn Read+Send>),child.stderr.take().map(|p|Box::new(p) as Box<dyn Read+Send>)].into_iter().flatten(){let shared=Arc::clone(&state);std::thread::spawn(move || {for line in BufReader::new(pipe).lines().map_while(Result::ok){if shared.lock().map_or(true,|p|p.generation!=generation){break}log_line(&line);}});}
  Run::Child(child)
 };
 p.run=Some(run);p.secret=Some(secret);p.port=port;p.installed=Some(meta);p.flavor=flavor;let v=status(&p);let _=app.emit("plasto:tgws:state",&v);drop(p);
 let shared=Arc::clone(&state);let app=app.clone();std::thread::spawn(move|| loop {
  std::thread::sleep(Duration::from_millis(500));let Ok(mut p)=shared.lock()else{break};if p.generation!=generation{break}
  if p.run.as_mut().is_some_and(Run::exited){match p.run.take(){Some(Run::Inside(i))=>{let _=i.thread.join();}Some(Run::Child(mut c))=>{let _=c.wait();}None=>{}}p.secret=None;let _=app.emit("plasto:tgws:state",status(&p));break}
 });v
}
/// Проверка через сам прокси, как его видит Telegram: obfuscated2-рукопожатие
/// с секретом и `req_pq_multi` к DC2. Ответ `resPQ` — значит, прокси довёл
/// запрос до Telegram любым своим путём (WebSocket, Cloudflare). Прямые адреса
/// Telegram для этого не годятся: у провайдера они как раз заблокированы.
pub fn probe(port:u16,secret:&str,timeout:Duration)->Result<u64,String> {
 use aes::cipher::{KeyIvInit,StreamCipher};
 type Ctr=ctr::Ctr128BE<aes::Aes256>;
 let secret:Vec<u8>=(0..secret.len()/2).filter_map(|i|u8::from_str_radix(secret.get(i*2..i*2+2)?,16).ok()).collect();
 if secret.len()!=16 {return Err("нет секрета прокси".into())}
 let began=Instant::now();
 // Начало пакета не должно походить на HTTP, TLS и другие транспорты.
 let mut init=[0u8;64];
 loop {
  fill_random(&mut init);
  let head=&init[..4];
  if init[0]!=0xef&&![b"HEAD",b"POST",b"GET ",b"OPTI",&[0xdd;4],&[0xee;4],&[0x16,0x03,0x01,0x02]].iter().any(|b|head==*b)&&init[4..8]!=[0;4] {break}
 }
 init[56..60].copy_from_slice(&[0xdd;4]);
 init[60..62].copy_from_slice(&2i16.to_le_bytes());
 let key=|k:&[u8]|->[u8;32]{let mut h=Sha256::new();h.update(k);h.update(&secret);h.finalize().into()};
 let rev:Vec<u8>=init[8..56].iter().rev().copied().collect();
 let mut enc=Ctr::new(&key(&init[8..40]).into(),init[40..56].try_into().unwrap());
 let mut dec=Ctr::new(&key(&rev[..32]).into(),rev[32..48].try_into().unwrap());
 let mut sealed=init;enc.apply_keystream(&mut sealed);
 init[56..].copy_from_slice(&sealed[56..]);
 // req_pq_multi без шифрования MTProto: auth_key_id 0, msg_id, длина, тело.
 let now=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
 let msg_id=(now.as_secs()<<32)|(u64::from(now.subsec_nanos())&!3);
 let mut nonce=[0u8;16];fill_random(&mut nonce);
 let mut msg=Vec::with_capacity(40);
 msg.extend_from_slice(&0u64.to_le_bytes());msg.extend_from_slice(&msg_id.to_le_bytes());msg.extend_from_slice(&20u32.to_le_bytes());
 msg.extend_from_slice(&0xbe7e8ef1u32.to_le_bytes());msg.extend_from_slice(&nonce);
 let mut packet=(msg.len() as u32).to_le_bytes().to_vec();packet.extend_from_slice(&msg);
 enc.apply_keystream(&mut packet);
 let addr=std::net::SocketAddr::from(([127,0,0,1],port));
 let mut sock=std::net::TcpStream::connect_timeout(&addr,timeout).map_err(|_|"прокси не принимает соединения".to_string())?;
 sock.set_read_timeout(Some(timeout)).ok();sock.set_write_timeout(Some(timeout)).ok();
 sock.write_all(&init).and_then(|_|sock.write_all(&packet)).map_err(|_|"прокси оборвал соединение".to_string())?;
 let mut len=[0u8;4];
 sock.read_exact(&mut len).map_err(|e|if matches!(e.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut){"таймаут".to_string()}else{"Telegram не ответил".to_string()})?;
 dec.apply_keystream(&mut len);
 let len=u32::from_le_bytes(len) as usize;
 if !(24..=4096).contains(&len) {return Err("непонятный ответ прокси".into())}
 let mut body=vec![0u8;len];
 sock.read_exact(&mut body).map_err(|_|"Telegram не ответил".to_string())?;
 dec.apply_keystream(&mut body);
 // resPQ с нашим nonce: ответ пришёл от Telegram, а не от кого-то по пути.
 if body[20..24]==0x05162463u32.to_le_bytes()&&body.len()>=40&&body[24..40]==nonce {Ok(began.elapsed().as_millis() as u64)} else {Err("непонятный ответ прокси".into())}
}
fn fill_random(buf:&mut [u8]) {
 use std::hash::{BuildHasher,Hasher};
 for (i,chunk) in buf.chunks_mut(8).enumerate(){let mut h=std::collections::hash_map::RandomState::new().build_hasher();h.write_usize(i);h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());let b=h.finish().to_le_bytes();chunk.copy_from_slice(&b[..chunk.len()]);}
}
#[cfg(test)] mod cache_tests {
 use super::*;
 #[test] fn cache_requires_matching_checksum_and_never_trusts_path_from_metadata() {
  let dir=tempfile::tempdir().unwrap();let data=b"test executable bytes";let digest=format!("{:x}",Sha256::digest(data));let exe=dir.path().join(format!("tgws-{}.exe",&digest[..16]));
  fs::write(&exe,data).unwrap();fs::write(dir.path().join("installed.json"),json!({"tag":"v1","sha256":digest,"exe":"C:/ignored.exe"}).to_string()).unwrap();
  assert_eq!(cached(dir.path()).unwrap()["exe"],json!(exe));fs::write(&exe,b"changed").unwrap();assert!(cached(dir.path()).is_none());
 }
}

#[cfg(test)] mod flavor_tests {
 use super::*;
 #[test] fn unknown_flavor_is_builtin() { assert_eq!(Flavor::parse(Some("rust")),Flavor::Rust); assert_eq!(Flavor::parse(Some("original")),Flavor::Original); assert_eq!(Flavor::parse(Some("x")),Flavor::Builtin); assert_eq!(Flavor::parse(None),Flavor::Builtin); }
 #[test] fn versions_compare_by_number() { assert!(version("v2.5.10")>version("v2.5.9")); assert!(version("v2.5.8")<=version("2.5.8")); assert!(version("v3.0.0-beta")>version(BUILTIN)||BUILTIN.starts_with('3')); assert_ne!(BUILTIN,"0.0.0"); }
 /// Встроенный занимает порт, освобождает его по остановке и сообщает,
 /// если порт уже занят.
 #[test] fn builtin_binds_and_releases_port() {
  let port=std::net::TcpListener::bind(("127.0.0.1",0)).unwrap().local_addr().unwrap().port();
  let secret=new_secret();
  let inside=start_inside(port,&secret).unwrap();
  assert!(std::net::TcpStream::connect(("127.0.0.1",port)).is_ok());
  // До порта прокси сначала берёт список доменов Cloudflare с GitHub; если это
  // дольше ожидания запуска (медленная сеть CI), ошибка порта придёт позже —
  // поток второго прокси всё равно завершится.
  match start_inside(port,&secret) {
   Err(e)=>assert!(e.contains("не запустился"),"{e}"),
   Ok(second)=>{let deadline=Instant::now()+Duration::from_secs(60);while !second.thread.is_finished(){assert!(Instant::now()<deadline,"второй прокси на занятом порту не завершился");std::thread::sleep(Duration::from_millis(100));}}
  }
  stop(Run::Inside(inside));
  assert!(std::net::TcpListener::bind(("127.0.0.1",port)).is_ok());
 }
 /// Живая система: скачанный Flowseal из папки PLASTO запускается так же, как
 /// из `toggle`, и остановка снимает все его процессы (PyInstaller держит два).
 /// `cargo test --lib -- --ignored flowseal_stops_completely --nocapture`.
 #[test] #[ignore] fn flowseal_stops_completely() {
  let root=Path::new(&std::env::var("APPDATA").unwrap()).join("app.plasto.desktop").join("tgws");
  let exe=cached(&root).expect("Flowseal не скачан")["exe"].as_str().unwrap().to_owned();
  let name=Path::new(&exe).file_name().unwrap().to_string_lossy().into_owned();
  let count=||{let out=Command::new("tasklist").args(["/FI",&format!("IMAGENAME eq {name}"),"/FO","CSV","/NH"]).output().unwrap();String::from_utf8_lossy(&out.stdout).lines().filter(|l|l.contains(&name)).count()};
  assert_eq!(count(),0,"Flowseal уже запущен");
  let mut cmd=Command::new(&exe);cmd.current_dir(&root).arg("--portable").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
  #[cfg(windows)] {use std::os::windows::process::CommandExt;cmd.creation_flags(0x08000000);}
  let child=cmd.spawn().unwrap();
  crate::platform::bind_to_app(&child);
  std::thread::sleep(Duration::from_secs(6));
  let running=count();
  println!("процессов Flowseal после запуска: {running}");
  stop(Run::Child(child));
  std::thread::sleep(Duration::from_millis(500));
  println!("после остановки: {}",count());
  assert!(running>=1);
  assert_eq!(count(),0);
 }
 /// Живая сеть: `cargo test --lib -- --ignored builtin_reaches_telegram --nocapture`.
 #[test] #[ignore] fn builtin_reaches_telegram() {
  let port=std::net::TcpListener::bind(("127.0.0.1",0)).unwrap().local_addr().unwrap().port();
  let secret=new_secret();
  let inside=start_inside(port,&secret).unwrap();
  let result=probe(port,&secret,Duration::from_secs(15));
  println!("probe: {result:?}");
  for l in LOGS.lock().unwrap().iter().take(20) { println!("{}",l["message"].as_str().unwrap_or("")); }
  stop(Run::Inside(inside));
  assert!(result.is_ok());
 }
 #[test] fn unpack_takes_exe_and_hashes_it() {
  let dir=tempfile::tempdir().unwrap();let archive=tempfile::NamedTempFile::new_in(dir.path()).unwrap();
  let mut w=zip::ZipWriter::new(archive.reopen().unwrap());w.start_file("tg-ws-proxy.exe",zip::write::SimpleFileOptions::default()).unwrap();w.write_all(b"exe bytes").unwrap();w.finish().unwrap();
  let (exe,digest)=unpack(dir.path(),&archive).unwrap();
  assert_eq!(fs::read(exe.path()).unwrap(),b"exe bytes");assert_eq!(digest,format!("{:x}",Sha256::digest(b"exe bytes")));
 }
 #[test] fn unpack_rejects_archive_without_exe() {
  let dir=tempfile::tempdir().unwrap();let archive=tempfile::NamedTempFile::new_in(dir.path()).unwrap();
  let mut w=zip::ZipWriter::new(archive.reopen().unwrap());w.start_file("readme.txt",zip::write::SimpleFileOptions::default()).unwrap();w.finish().unwrap();
  assert!(unpack(dir.path(),&archive).is_err());
 }
}

#[cfg(test)] mod config_tests {
 use super::*;
 #[test] fn log_hides_secret() {
  let secret="b41283f0f673428bb0ac69c5f3c26925";
  for line in [format!(" INFO   Secret:        {secret}"),format!("tg://proxy?server=127.0.0.1&port=1443&secret=dd{secret}")] {log_line(&line);}
  let logs=LOGS.lock().unwrap();
  assert!(logs.iter().all(|l|!l["message"].as_str().unwrap_or("").contains(secret)));
 }
 #[test] fn secret_is_32_hex_and_differs() { let (a,b)=(new_secret(),new_secret()); assert!(valid_secret(&a)); assert_ne!(a,b); }
 #[test] fn prepare_keeps_secret_and_sets_port() {
  let dir=tempfile::tempdir().unwrap(); let first=prepare(dir.path(),1443,None).unwrap(); let again=prepare(dir.path(),1500,None).unwrap();
  assert_eq!(first,again);
  let cfg:Value=serde_json::from_str(&fs::read_to_string(dir.path().join("TgWsProxy_data/config.json")).unwrap()).unwrap();
  assert_eq!(cfg["port"],1500); assert_eq!(cfg["check_updates"],false);
 }
}
