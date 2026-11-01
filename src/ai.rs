use std::{collections::BTreeSet,fs,path::Path};
use serde_json::{json,Value};
use regex_lite::Regex;
const BEGIN:&str="# >>> PLASTO AI — блок управляется приложением";
const END:&str="# <<< PLASTO AI";
pub fn services()->Value { serde_json::from_str(include_str!("ai-services.json")).unwrap_or(json!([])) }
fn domain(raw:&str)->Option<String> { let d=raw.trim().to_lowercase();let d=d.strip_prefix("https://").or_else(||d.strip_prefix("http://")).unwrap_or(&d);let d=d.strip_prefix("www.").unwrap_or(d);let d=d.split(['/', '?', '#', ':']).next().unwrap_or("");let valid=Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$").ok()?;if valid.is_match(d){Some(d.into())}else{None} }
pub fn apply(dir:&Path, payload:&Value)->Result<Value,String>{
 let ids=payload["bypass"].as_array().ok_or("не указан список сервисов")?;let mut domains=BTreeSet::new();
 for svc in services().as_array().into_iter().flatten() {if ids.iter().any(|id|id==&svc["id"]) {for d in svc["domains"].as_array().into_iter().flatten(){if let Some(d)=d.as_str(){domains.insert(d.to_owned());}}}}
 if let Some(extra)=payload["extra"].as_str(){for d in extra.split(|c:char|c.is_whitespace()||c==','||c==';'){if let Some(d)=domain(d){domains.insert(d);}}}
 let file=dir.join("lists/list-general-user.txt");let old=match fs::read_to_string(&file){Ok(s)=>s,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>String::new(),Err(e)=>return Err(e.to_string())};
 let mut lines=Vec::new();let mut inside=false;let mut seen=false;for line in old.lines(){let trimmed=line.trim();if trimmed==BEGIN {if inside||seen{return Err("повреждён блок PLASTO в списке: автоматическая запись остановлена".into())}inside=true;seen=true;continue}if trimmed==END {if !inside{return Err("повреждён блок PLASTO в списке: автоматическая запись остановлена".into())}inside=false;continue}if !inside && trimmed!="domain.example.abc"{lines.push(line.to_owned());}}
 if inside{return Err("незакрытый блок PLASTO в списке: автоматическая запись остановлена".into())}
 if !domains.is_empty(){lines.push(BEGIN.into());lines.push("# Домены ИИ-сервисов под обходом.".into());lines.extend(domains.iter().cloned());lines.push(END.into());}
 if !lines.iter().any(|l|!l.trim().is_empty()&&!l.trim().starts_with('#')){lines.push("domain.example.abc".into())}
 let text=lines.join("\r\n")+"\r\n";fs::create_dir_all(file.parent().ok_or("нет папки списка")?).map_err(|e|e.to_string())?;fs::write(&file,text).map_err(|e|e.to_string())?;
 Ok(json!({"list":{"file":file,"added":domains.len(),"count":domains.len(),"changed":true},"restarted":false}))
}

/// Загрузить и сохранить каталог, не заменяя старую копию при ошибке.
pub fn refresh(root:&Path)->Result<Value,String>{
 let text=crate::probe::fetch_source(crate::hosts::SOURCE_URL,std::time::Duration::from_secs(30))?;
 let parsed=crate::hosts::parse(&text);
 if parsed.unblock.is_empty(){return Err("каталог не содержит адресов прокси".into())}
 fs::create_dir_all(root).map_err(|e|e.to_string())?;
 let mut temp=tempfile::NamedTempFile::new_in(root).map_err(|e|e.to_string())?;
 use std::io::Write;temp.write_all(text.as_bytes()).map_err(|e|e.to_string())?;
 temp.persist(root.join("proxy-catalog.txt")).map_err(|e|e.to_string())?;
 Ok(json!({"unblock":parsed.unblock.len(),"blocklist":parsed.blocklist.len(),"updated":parsed.updated}))
}
pub fn catalog(root:&Path)->Option<crate::hosts::Parsed>{fs::read_to_string(root.join("proxy-catalog.txt")).ok().map(|s|crate::hosts::parse(&s)).filter(|p|!p.unblock.is_empty())}
/// Только проверенные прокси; недоступные имена остаются на обычном DNS.
pub fn apply_hosts(payload:&Value,root:&Path,file:&Path,source:&crate::hosts::Parsed)->Result<Value,String>{
 let ids=payload["proxy"].as_array().ok_or("не указан список прокси")?;
 let mut domains=Vec::new();for svc in services().as_array().into_iter().flatten(){if ids.iter().any(|id|id==&svc["id"]){domains.extend(svc["domains"].as_array().into_iter().flatten().filter_map(|v|v.as_str().map(str::to_owned)));}}
 let own=match payload["entry"].as_str().map(str::trim).filter(|x|!x.is_empty()){Some(x)=>Some(own_entry(x)?),None=>None};
 // Свой прокси — одна точка: замен искать негде, пул из него одного.
 let (wanted,pool)=match &own {
  Some(ip)=>{let mut known=source.unblock.clone();known.extend(known_names());(crate::hosts::own_entries(ip,&domains,&known),vec![ip.clone()])},
  None=>{let w=crate::hosts::entries_for_domains(&source.unblock,&domains);let mut p:Vec<String>=Vec::new();for e in &source.unblock{if !p.contains(&e.ip){p.push(e.ip.clone())}}(w,p)}
 };
 if !ids.is_empty()&&wanted.is_empty(){return Err("каталог не содержит прокси для выбранных сервисов".into())}
 let choice=if wanted.is_empty(){crate::hosts::Choice::default()}else{
  // Мёртвые адреса отсеиваем одним заходом: иначе замена ждала бы таймаут на каждом имени.
  let probe:Vec<crate::hosts::Entry>=pool.iter().map(|ip|crate::hosts::Entry::new(ip,"")).collect();
  let dead=crate::probe::filter_alive(&probe,443,std::time::Duration::from_secs(3)).dead;
  let serves=|ip:&str,host:&str|!dead.iter().any(|d|d==ip)&&crate::probe::serves_clean(ip,host,std::time::Duration::from_secs(4));
  crate::hosts::choose(&wanted,&pool,&serves,&|ip:&str|crate::probe::geo_of(ip,std::time::Duration::from_secs(4)))
 };
 let accepted=&choice.entries;
 if !wanted.is_empty()&&accepted.is_empty(){return Err(if own.is_some(){"свой прокси не отдал ни одного выбранного имени; hosts не изменён"}else{"ни один выбранный прокси не прошёл проверку TLS; hosts не изменён"}.into())}
 let blocks=if payload["blocklist"]==true {source.blocklist.as_slice()}else{&[]};
 // Не превращать ошибку чтения в пустой hosts: в нём могут быть пользовательские записи.
 let existing=fs::read_to_string(file).map_err(|e|format!("не удалось прочитать hosts: {e}"))?;
 let begin=existing.matches(crate::hosts::MARK_BEGIN).count();let end=existing.matches(crate::hosts::MARK_END).count();
 if begin!=end || begin>1 || (begin==1 && existing.find(crate::hosts::MARK_BEGIN)>existing.find(crate::hosts::MARK_END)){return Err("повреждён блок PLASTO в hosts; запись отменена".into())}
 // Помимо исходной копии, отдельная копия перед каждой правкой сохраняет недавние изменения пользователя.
 fs::create_dir_all(root).map_err(|e|e.to_string())?;
 let mut backup=tempfile::Builder::new().prefix("hosts-before-change-").suffix(".backup").tempfile_in(root).map_err(|e|e.to_string())?;
 use std::io::Write;backup.write_all(existing.as_bytes()).map_err(|e|format!("не удалось сохранить копию hosts: {e}"))?;
 backup.keep().map_err(|e|format!("не удалось сохранить копию hosts: {e}"))?;
 let out=crate::hosts::apply(accepted,blocks,Some(root),file).map_err(|e|e.to_string())?;
 let swaps:Vec<Value>=choice.swaps.iter().map(|s|json!({"from":s.from,"to":s.to,"hosts":s.hosts})).collect();
 let country=own.as_ref().and_then(|ip|choice.geo.iter().find(|(g,_)|g==ip)).and_then(|(_,c)|c.clone());
 Ok(json!({"proxies":out.proxies,"blocked":out.blocked,"shadowed":out.shadowed,"changed":out.changed,"skipped":choice.skipped,"swaps":swaps,"domestic":choice.domestic,"own":own,"ownCountry":country}))
}
/// Поддомены сервисов из списка ai-sni-proxy: своему прокси каталог адресов
/// не нужен, а имена без него взять неоткуда. Адрес здесь — заглушка.
fn known_names()->Vec<crate::hosts::Entry>{include_str!("ai-names.txt").lines().map(str::trim).filter(|l|!l.is_empty()&&!l.starts_with('#')).map(|h|crate::hosts::Entry::new("0.0.0.0",&h.to_lowercase())).collect()}
/// Адрес своего SNI-прокси. Только IPv4: блок hosts разбирается по IPv4, и
/// 0.0.0.0 там означает блоклист.
pub fn own_entry(raw:&str)->Result<String,String>{
 match raw.trim().parse::<std::net::Ipv4Addr>(){Ok(ip) if !ip.is_unspecified()&&!ip.is_broadcast()=>Ok(ip.to_string()),_=>Err(format!("«{}» — не IPv4-адрес прокси",raw.trim()))}
}
#[cfg(test)]mod tests{use super::*;#[test]fn preserves_user_lines_and_rejects_broken_block(){let d=tempfile::tempdir().unwrap();fs::create_dir_all(d.path().join("lists")).unwrap();let p=d.path().join("lists/list-general-user.txt");fs::write(&p,"custom.example\n").unwrap();let r=apply(d.path(),&json!({"bypass":["chatgpt"],"proxy":[],"extra":"bad host,example.org"})).unwrap();assert!(r["list"]["added"].as_u64().unwrap()>1);let result=fs::read_to_string(&p).unwrap();assert!(result.contains("custom.example"));assert!(result.contains("chatgpt.com"));fs::write(&p,format!("keep\n{BEGIN}\n")).unwrap();assert!(apply(d.path(),&json!({"bypass":[],"proxy":[]})).is_err());assert!(fs::read_to_string(&p).unwrap().contains("keep"));} }

#[cfg(test)] mod host_integration_tests {
 use super::*;
 #[test] fn preserves_existing_entries_and_backup_and_clears_own_block() {
  let tmp=tempfile::tempdir().unwrap();let f=tmp.path().join("hosts");
  fs::write(&f,"127.0.0.1 localhost\r\n192.0.2.4 custom.example\r\n").unwrap();
  let src=crate::hosts::Parsed {unblock:vec![],blocklist:vec![crate::hosts::Entry::new("0.0.0.0","ads.example")],updated:None};
  let r=apply_hosts(&json!({"proxy":[],"blocklist":true}),tmp.path(),&f,&src).unwrap();assert_eq!(r["blocked"],1);
  assert!(fs::read_to_string(&f).unwrap().contains("custom.example"));assert!(fs::read_to_string(crate::hosts::backup_path(tmp.path())).unwrap().contains("custom.example"));
  apply_hosts(&json!({"proxy":[],"blocklist":false}),tmp.path(),&f,&src).unwrap();let text=fs::read_to_string(&f).unwrap();assert!(!text.contains("ads.example"));assert!(text.contains("custom.example"));
 }
 #[test] fn rejects_malformed_hosts_without_modifying_it() {
  let tmp=tempfile::tempdir().unwrap();let f=tmp.path().join("hosts");let text=format!("{}\nkeep.example\n",crate::hosts::MARK_BEGIN);fs::write(&f,&text).unwrap();
  assert!(apply_hosts(&json!({"proxy":[],"blocklist":false}),tmp.path(),&f,&crate::hosts::Parsed::default()).is_err());assert_eq!(fs::read_to_string(&f).unwrap(),text);
 }
}

/// Показывает только сетевой ответ; работа после входа и причина 403 неизвестны.
pub fn probe_services(payload:&Value,cfg:&serde_json::Map<String,Value>,app:&crate::host::Host)->Value {
 use std::time::{Duration,Instant};
 static BUSY:std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
 struct Guard;impl Drop for Guard{fn drop(&mut self){BUSY.store(false,std::sync::atomic::Ordering::Release)}}
 if BUSY.swap(true,std::sync::atomic::Ordering::AcqRel){return json!({"error":"проверка уже выполняется"})}let _guard=Guard;
 let proxy=if payload["useProxy"]==true {match cfg.get("aiProbeProxy").and_then(Value::as_str).filter(|x|!x.is_empty()){Some(x)=>Some(x),None=>return json!({"error":"сначала укажите адрес HTTP-прокси"})}}else{None};
 let all=services();let selected=payload["ids"].as_array();let items:Vec<&Value>=all.as_array().into_iter().flatten().filter(|s|selected.map_or(true,|a|a.iter().any(|id|id==&s["id"]))).filter(|s|s["probe"]["host"].as_str().is_some()).collect();
 let mut rows=Vec::new();let mut summary=json!({"total":0,"ok":0,"reachable":0,"blocked":0,"geo":0,"hostsInactive":0,"proxyDead":0,"failed":0});
 for svc in &items {
  let host=svc["probe"]["host"].as_str().unwrap_or("");let path=svc["probe"]["path"].as_str().unwrap_or("/");let started=Instant::now();
  let tls=match native_tls::TlsConnector::new(){Ok(v)=>v,Err(e)=>return json!({"error":e.to_string()})};
  let mut builder=ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).timeout(Duration::from_secs(7));
  if let Some(address)=proxy{let proxy=match ureq::Proxy::new(address){Ok(p)=>p,Err(e)=>return json!({"error":format!("адрес прокси: {e}")})};builder=builder.proxy(proxy)}
  let response=builder.build().get(&format!("https://{host}{path}")).set("Connection","close").set("User-Agent","Mozilla/5.0").call();
  let (state,detail,fix)=match response {Ok(r)=>("reachable",format!("HTTP {}; доступ после входа не проверен",r.status()),Value::Null),Err(ureq::Error::Status(403,_))=>("partial","HTTP 403: причина не установлена".to_string(),Value::Null),Err(ureq::Error::Status(451,_))=>("geo","HTTP 451: доступ ограничен".to_string(),Value::Null),Err(e)=>("unknown",format!("нет ответа: {e}"),Value::Null)};
  let row=json!({"id":svc["id"],"name":svc["name"],"host":host,"state":state,"detail":detail,"fix":fix,"ms":started.elapsed().as_millis() as u64});
  summary["total"]=json!(summary["total"].as_u64().unwrap_or(0)+1);let key=match state{"reachable"=>"reachable","geo"=>"geo",_=>"failed"};summary[key]=json!(summary[key].as_u64().unwrap_or(0)+1);
  rows.push(row.clone());let _=app.emit("plasto:ai:progress",json!({"done":rows.len(),"total":items.len(),"last":row}));
 }
 json!({"results":rows,"summary":summary,"viaProxy":proxy.is_some()})
}
#[cfg(test)] mod names_tests {
 use super::*;
 #[test] fn own_proxy_gets_subdomains_from_bundled_names() {
  let domains=vec!["anthropic.com".to_string(),"jetbrains.com".to_string()];
  let hosts:Vec<String>=crate::hosts::own_entries("203.0.113.10",&domains,&known_names()).into_iter().map(|e|e.host).collect();
  assert!(hosts.contains(&"api.anthropic.com".to_string()));assert!(hosts.contains(&"account.jetbrains.com".to_string()));
  assert!(hosts.iter().all(|h|h.ends_with("anthropic.com")||h.ends_with("jetbrains.com")),"чужие имена из списка не берутся");
 }
}
