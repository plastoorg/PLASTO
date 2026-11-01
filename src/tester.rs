//! Проверка стратегий: отдельные TLS-соединения для каждого измерения.
//!
//! Соединения идут с портов `isolate::PORTS`. Проверяемый winws видит только
//! их, основной их не трогает — обход на время подбора не выключается. Если
//! основной или проверяемую стратегию изолировать не удалось, основной обход
//! выключается до конца подбора, как раньше.
use std::{io::{BufRead,BufReader,Read,Write},net::{Ipv4Addr,Ipv6Addr,SocketAddr,TcpStream,ToSocketAddrs},process::{Child,Command,Stdio},sync::{atomic::{AtomicBool,AtomicU16,AtomicU32,Ordering},Arc,Mutex},time::{Duration,Instant,SystemTime,UNIX_EPOCH}};
use serde_json::{json,Map,Value};
use crate::host::Host;
use crate::{engines::{self,Engine},isolate::{self,Side},runtime,service};
static RUNNING:AtomicBool=AtomicBool::new(false);
static TEST_PID:AtomicU32=AtomicU32::new(0);
static NEXT_PORT:AtomicU16=AtomicU16::new(0);
/// Попыток на цель; решает большинство: блокировка у провайдера срабатывает
/// не каждый раз, и одна удача без обхода — ещё не доступность.
const TRIES:usize=3;
static CANCEL:AtomicBool=AtomicBool::new(false);
const TARGETS:[(&str,f64,&str,&str);8]=[("Discord",1.3,"discord.com","/api/v9/gateway"),("Discord",1.3,"cdn.discordapp.com","/"),("YouTube",1.3,"www.youtube.com","/generate_204"),("YouTube",1.3,"redirector.googlevideo.com","/generate_204"),("Google",1.0,"www.google.com","/generate_204"),("Google",1.0,"i.ytimg.com","/"),("Cloudflare",0.8,"cloudflare.com","/cdn-cgi/trace"),("Cloudflare",0.8,"speed.cloudflare.com","/__down?bytes=1")];
struct Guard; impl Drop for Guard {fn drop(&mut self){RUNNING.store(false,Ordering::Release);}}
pub fn running()->bool{RUNNING.load(Ordering::Acquire)}
/// PID проверяемого winws: он наш, а не чужой zapret.
pub fn child_pid()->Option<u32>{match TEST_PID.load(Ordering::Acquire){0=>None,p=>Some(p)}}
pub fn cancel()->bool{if running(){CANCEL.store(true,Ordering::Release);true}else{false}}
fn event(app:&Host,name:&str,value:Value){let _=app.emit(&format!("plasto:test:{name}"),value);}
fn median(values:&mut [u64])->Option<u64>{if values.is_empty(){return None}values.sort_unstable();let n=values.len();Some(if n%2==1{values[n/2]}else{(values[n/2-1]+values[n/2]+1)/2})}
/// Сокет на свободном порту тестера: подряд по кругу, занятые пропускаем.
fn socket(addr:&SocketAddr)->Result<socket2::Socket,String>{
 let (lo,hi)=(*isolate::PORTS.start(),*isolate::PORTS.end());let mut last=String::new();
 for _ in 0..32{let port=lo+NEXT_PORT.fetch_add(1,Ordering::Relaxed)%(hi-lo+1);let s=socket2::Socket::new(socket2::Domain::for_address(*addr),socket2::Type::STREAM,Some(socket2::Protocol::TCP)).map_err(|e|e.to_string())?;
 let local:SocketAddr=if addr.is_ipv4(){(Ipv4Addr::UNSPECIFIED,port).into()}else{(Ipv6Addr::UNSPECIFIED,port).into()};match s.bind(&local.into()){Ok(())=>return Ok(s),Err(e)=>last=e.to_string()}}
 Err(format!("нет свободного порта {lo}–{hi}: {last}"))
}
/// Один HEAD по HTTPS с порта тестера. Любой HTTP-ответ, даже 404, — сайт
/// доступен; провал — только таймаут и обрыв.
fn head(host:&str,path:&str)->Result<(u16,u64),String>{
 let start=Instant::now();let limit=Duration::from_secs(5);let left=||limit.saturating_sub(start.elapsed()).max(Duration::from_millis(100));
 let addrs:Vec<SocketAddr>=(host,443).to_socket_addrs().map_err(|e|format!("DNS: {e}"))?.collect();
 let addr=*addrs.iter().find(|a|a.is_ipv4()).or(addrs.first()).ok_or("DNS: нет адресов")?;
 // Порт после прошлой проверки того же сервера ещё в TIME_WAIT: bind проходит, а connect — нет. Берём следующий.
 let mut busy=0;let sock=loop{let sock=socket(&addr)?;match sock.connect_timeout(&addr.into(),limit){Ok(())=>break sock,Err(e) if e.kind()==std::io::ErrorKind::AddrInUse&&busy<8=>busy+=1,Err(e)=>return Err(format!("соединение: {e}"))}};
 let stream:TcpStream=sock.into();let _=stream.set_read_timeout(Some(left()));let _=stream.set_write_timeout(Some(left()));
 let tls=native_tls::TlsConnector::new().map_err(|e|e.to_string())?;let mut tls=tls.connect(host,stream).map_err(|e|format!("TLS: {e}"))?;
 write!(tls,"HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mozilla/5.0\r\nConnection: close\r\n\r\n").map_err(|e|format!("запрос: {e}"))?;
 let _=tls.get_ref().set_read_timeout(Some(left()));let mut got=Vec::new();let mut buf=[0u8;256];
 while !got.windows(2).any(|w|w==b"\r\n")&&got.len()<1024{let n=tls.read(&mut buf).map_err(|e|format!("ответ: {e}"))?;if n==0{break}got.extend_from_slice(&buf[..n]);}
 let line=String::from_utf8_lossy(&got);let code=line.strip_prefix("HTTP/").and_then(|l|l.split_whitespace().nth(1)).and_then(|c|c.parse::<u16>().ok()).ok_or("ответ не HTTP")?;
 Ok((code,start.elapsed().as_millis() as u64))
}
/// Итог одного сайта за все его попытки.
#[derive(Default)]
struct Outcome{hits:usize,tries:usize,first:Option<(u16,u64)>,error:String}
/// Попытки одного сайта до большинства из TRIES: два одинаковых исхода подряд решают сразу.
fn attempts(app:&Host,phase:&str,label:&str,index:usize,host:&str,path:&str)->Outcome{
 let mut o=Outcome::default();
 while o.tries<TRIES&&o.hits*2<=TRIES&&(o.tries-o.hits)*2<=TRIES{if CANCEL.load(Ordering::Acquire){break}o.tries+=1;event(app,"progress",json!({"phase":phase,"preset":label,"index":index,"total":TARGETS.len(),"host":host,"try":o.tries}));match head(host,path){Ok(r)=>{o.hits+=1;o.first.get_or_insert(r);},Err(e)=>o.error=e}}
 o
}
fn probe(app:&Host,phase:&str,label:&str)->Value{
 // Сайты независимы и проверяются разом: стратегия занимает столько, сколько
 // самый медленный из них (обычно два таймаута недоступного), а не их сумму.
 let outcomes:Vec<Outcome>=std::thread::scope(|scope|{
  let handles:Vec<_>=TARGETS.iter().enumerate().map(|(index,(_,_,host,path))|{let app=app.clone();scope.spawn(move||{let o=attempts(&app,phase,label,index,host,path);event(&app,"progress",json!({"phase":phase,"preset":label,"index":index,"total":TARGETS.len(),"host":host,"ok":o.hits*2>o.tries}));o})}).collect();
  handles.into_iter().map(|h|h.join().unwrap_or_else(|_|Outcome{error:"проверка сайта упала".into(),..Default::default()})).collect()
 });
 let mut rows=Vec::new();let mut group=Map::new();let mut times=Vec::new();let mut good=0.0;let mut total=0.0;
 for ((g,weight,host,_),o) in TARGETS.iter().zip(outcomes){
 let ok=o.hits*2>o.tries;total+=weight;if ok{good+=weight};let row=json!({"group":g,"host":host,"weight":weight,"ok":ok,"hits":o.hits,"tries":o.tries,"status":o.first.map(|s|s.0),"ms":o.first.map(|s|s.1),"error":if ok{None}else{Some(o.error)}});if let (true,Some((_,ms)))=(ok,o.first){times.push(ms)};
 let entry=group.entry(*g).or_insert(json!({"ok":0,"total":0}));entry["total"]=json!(entry["total"].as_u64().unwrap_or(0)+1);if ok{entry["ok"]=json!(entry["ok"].as_u64().unwrap_or(0)+1)}rows.push(row);}
 json!({"score":if total>0.0{((good/total*100.0) as f64).round() as u64}else{0},"passed":rows.iter().filter(|x|x["ok"]==true).count(),"total":rows.len(),"medianMs":median(&mut times),"groups":group,"targets":rows})
}
/// Проверяемый winws. Пока жив — его PID знают проверки на чужой zapret;
/// убивается при выходе из области видимости, даже по отмене.
struct Trial{child:Child,tail:Arc<Mutex<Vec<String>>>}
impl Trial{
 fn spawn(cmd:&engines::Command)->Result<Trial,String>{
  let mut c=Command::new(&cmd.exe);c.args(&cmd.args).current_dir(&cmd.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
  #[cfg(windows)]{use std::os::windows::process::CommandExt;c.creation_flags(0x08000000);}
  let mut child=c.spawn().map_err(|e|format!("не удалось запустить winws: {e}"))?;TEST_PID.store(child.id(),Ordering::Release);
  // Вывод читаем до конца, иначе winws встанет на полном канале; помним хвост — причину падения.
  let tail=Arc::new(Mutex::new(Vec::new()));
  for pipe in [child.stdout.take().map(|p|Box::new(p) as Box<dyn Read+Send>),child.stderr.take().map(|p|Box::new(p) as Box<dyn Read+Send>)].into_iter().flatten(){let tail=Arc::clone(&tail);std::thread::spawn(move||{for line in BufReader::new(pipe).lines().map_while(Result::ok){if let Ok(mut t)=tail.lock(){t.push(line);if t.len()>3{t.remove(0);}}}});}
  Ok(Trial{child,tail})
 }
 fn alive(&mut self)->bool{matches!(self.child.try_wait(),Ok(None))}
 fn why(&self)->String{let tail=self.tail.lock().map(|t|t.join("; ")).unwrap_or_default();if tail.is_empty(){"процесс завершился".into()}else{format!("процесс завершился: {tail}")}}
}
impl Drop for Trial{fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();TEST_PID.store(0,Ordering::Release);}}
pub fn health(app:&Host)->Value {
 if runtime::snapshot(app)["running"]!=true {return Value::Null}
 let mut results=Vec::new();
 for (name,url) in [("YouTube","https://www.youtube.com/generate_204"),("Discord","https://discord.com/api/v9/gateway")] {
  let began=Instant::now();let tls=match native_tls::TlsConnector::new(){Ok(t)=>t,Err(e)=>{results.push(json!({"name":name,"ok":false,"error":e.to_string()}));continue}};
  let agent=ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).build();
  let response=agent.get(url).set("Connection","close").timeout(Duration::from_secs(6)).call();
  match response {Ok(r)|Err(ureq::Error::Status(_,r))=>results.push(json!({"name":name,"ok":true,"status":r.status(),"ms":began.elapsed().as_millis() as u64})),Err(e)=>results.push(json!({"name":name,"ok":false,"error":e.to_string(),"ms":began.elapsed().as_millis() as u64}))}
 }
 let out=json!({"ts":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,"okCount":results.iter().filter(|r|r["ok"]==true).count(),"total":results.len(),"results":results});
 let _=app.emit("plasto:engine:health",out.clone());out
}
/// Последний отчёт подбора: показать его и после перезапуска, с датой.
fn last_path(app:&Host)->Option<std::path::PathBuf>{app.path().app_config_dir().ok().map(|d|d.join("last-test.json"))}
fn save_last(app:&Host,report:&Value){
 if report["results"].as_array().is_none_or(|r|r.is_empty()){return}
 if let Some(path)=last_path(app){if let Some(dir)=path.parent(){let _=std::fs::create_dir_all(dir);}let _=std::fs::write(path,report.to_string());}
}
pub fn last(app:&Host)->Value{last_path(app).and_then(|p|std::fs::read_to_string(p).ok()).and_then(|s|serde_json::from_str(&s).ok()).unwrap_or(Value::Null)}
pub fn run(app:&Host,cfg:&Map<String,Value>,opts:&Value)->Value{
 if RUNNING.swap(true,Ordering::AcqRel){return json!({"error":"тест уже выполняется"})} let _guard=Guard;CANCEL.store(false,Ordering::Release);
 let svc=service::status();if svc["error"].is_string(){return json!({"error":svc["error"]})}if svc["installed"]==true{return json!({"error":"сначала остановите и удалите службу PLASTO"})}
 let dir=cfg.get("zapretDir").and_then(Value::as_str).unwrap_or("");let mode=cfg.get("engineMode").and_then(Value::as_str).and_then(Engine::parse);let v=engines::validate(std::path::Path::new(dir),mode);if !v.ok{return json!({"error":v.reason})}
 let selected=opts["only"].as_array();let presets:Vec<_>=v.presets.into_iter().filter(|p|selected.map_or(true, |a|a.is_empty()||a.iter().any(|x|x.as_str()==Some(p.file.as_str())))).collect();if presets.is_empty(){return json!({"error":"нет подходящих стратегий"})}
 let prev=runtime::snapshot(app);let was_running=prev["running"]==true;let old=prev["strategy"].as_str().map(str::to_owned);let start=Instant::now();
 let mut stopped=false;let owned_pid=prev["pid"].as_u64().and_then(|p|u32::try_from(p).ok());
 match runtime::other_winws_except(owned_pid) { Ok(false)=>{}, Ok(true)=>return json!({"error":"уже работает другой zapret","conflict":"winws","found":crate::conflict::winws(owned_pid)}), Err(e)=>return json!({"error":e}) }
 let dir=std::path::Path::new(dir);if let Err(e)=runtime::prepare(dir){return json!({"error":e})}
 #[cfg(windows)]let _=runtime::tcp_timestamps();
 // Основной обход без исключения портов тестера ловит и его соединения — тогда только выключить.
 let mut paused=false;let mut pause=|app:&Host|if !paused&&runtime::snapshot(app)["running"]==true{runtime::action(app,cfg,"stop",None);paused=true;};
 if was_running&&prev["isolated"]!=true{pause(app);}
 event(app,"stage",json!({"stage":"baseline"}));let base=probe(app,"baseline","");event(app,"baseline",base.clone());let mut results=Vec::new();let build=runtime::build_opts(cfg);
 for (i,p) in presets.iter().enumerate(){if CANCEL.load(Ordering::Acquire){break}event(app,"progress",json!({"phase":"queue","index":i,"total":presets.len()}));event(app,"stage",json!({"stage":"preset","preset":p.to_json()}));let begun=Instant::now();
 let fail=|error:String|json!({"preset":p.file,"label":p.label,"ok":false,"error":error,"score":0,"passed":0,"total":TARGETS.len(),"groups":{},"ms":begun.elapsed().as_millis() as u64});
 let mut cmd=match engines::build_command(dir,&p.file,&build){Ok(c)=>c,Err(e)=>{let r=fail(e);event(app,"result",r.clone());results.push(r);continue}};
 // Не изолировалась — значит, ловит весь трафик: основной обход на это время выключаем.
 if isolate::apply(&mut cmd,Side::Test).is_err(){pause(app);}
 let mut trial=match Trial::spawn(&cmd){Ok(t)=>t,Err(e)=>{let r=fail(e);event(app,"result",r.clone());results.push(r);continue}};
 let ready=Instant::now();while ready.elapsed()<Duration::from_millis(2600)&&trial.alive()&&!CANCEL.load(Ordering::Acquire){std::thread::sleep(Duration::from_millis(100));}
 let mut r=if trial.alive()&&!CANCEL.load(Ordering::Acquire){probe(app,"preset",&p.label)}else{json!({"score":0,"passed":0,"total":0,"medianMs":null,"groups":{},"error":trial.why()})};drop(trial);std::thread::sleep(Duration::from_millis(300));r["preset"]=json!(p.file);r["label"]=json!(p.label);r["group"]=json!(p.group);r["ok"]=json!(r["score"].as_u64().unwrap_or(0)>0);r["ms"]=json!(begun.elapsed().as_millis() as u64);event(app,"result",r.clone());let perfect=r["score"]==100;results.push(r);if perfect&&opts["stopOnPerfect"]==true&&i+1<presets.len(){stopped=true;break}}
 results.sort_by(|a,b|b["ok"].as_bool().cmp(&a["ok"].as_bool()).then_with(||b["score"].as_u64().cmp(&a["score"].as_u64())).then_with(||a["medianMs"].as_u64().unwrap_or(u64::MAX).cmp(&b["medianMs"].as_u64().unwrap_or(u64::MAX))));
 let cancelled=CANCEL.load(Ordering::Acquire);if paused{let restored=runtime::action(app,cfg,"start",old.as_deref());if restored["running"]!=true{return json!({"error":"тест завершён, но предыдущая стратегия не восстановилась","results":results})}}
 let report=json!({"baseline":base,"best":results.first(),"results":results,"cancelled":cancelled,"stoppedOnPerfect":stopped,"presetsTotal":presets.len(),"durationMs":start.elapsed().as_millis() as u64,"finishedAt":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64});save_last(app,&report);event(app,"done",report.clone());report
}
#[cfg(test)]mod tests{use super::*;#[test]fn median_handles_even_odd_empty(){assert_eq!(median(&mut []),None);assert_eq!(median(&mut [9,1,5]),Some(5));assert_eq!(median(&mut [1,8]),Some(5));}
 /// Сеть: HEAD с порта тестера до незаблокированного сайта.
 #[test]#[ignore]fn head_goes_from_tester_port(){let (code,_)=head("www.google.com","/generate_204").unwrap();assert_eq!(code,204);}}
