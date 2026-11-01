//! Диагностика только читает состояние Windows; ничего не останавливает.
use serde_json::{json,Value};
use crate::dns::{Runner,SystemRunner};
use crate::host::Host;
fn run_cmd(r:&dyn Runner,file:&str,args:&[&str])->Result<String,String>{let a=args.iter().map(|x|x.to_string()).collect::<Vec<_>>();let o=r.run(file,&a);if o.ok{Ok(o.out)}else{Err(if o.err.trim().is_empty(){"команда не выполнена".into()}else{o.err.trim().into()})}}
const TOTAL:usize=13;
fn item(id:&str,title:&str,level:&str,message:&str)->Value{json!({"id":id,"title":title,"level":level,"message":message})}
/// Строгий DoH в Chromium: `dns_over_https.mode` в Local State. «automatic» не считаем — он включается, только если системный DNS сам умеет DoH.
fn chromium_secure(local_state:&str)->bool{serde_json::from_str::<Value>(local_state).ok().is_some_and(|v|v["dns_over_https"]["mode"]=="secure")}
/// Режим TRR в prefs.js Firefox: 2 — DoH первым, 3 — только DoH; без строки — выключен.
fn firefox_trr(prefs:&str)->u32{prefs.lines().filter_map(|l|l.trim().strip_prefix(r#"user_pref("network.trr.mode","#)).filter_map(|r|r.trim().trim_end_matches(");").trim().parse().ok()).last().unwrap_or(0)}
/// Браузеры со своим DoH. hosts они могут обойти: ping покажет прокси, а сайт — «недоступно в регионе».
fn browser_doh()->Vec<&'static str>{
 let local=std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);let roaming=std::env::var_os("APPDATA").map(std::path::PathBuf::from);let mut out=Vec::new();
 if let Some(l)=&local{for (name,dir) in [("Chrome","Google/Chrome"),("Edge","Microsoft/Edge"),("Яндекс.Браузер","Yandex/YandexBrowser"),("Brave","BraveSoftware/Brave-Browser")]{if std::fs::read_to_string(l.join(dir).join("User Data/Local State")).is_ok_and(|s|chromium_secure(&s)){out.push(name)}}}
 if let Some(r)=&roaming{if std::fs::read_dir(r.join("Mozilla/Firefox/Profiles")).into_iter().flatten().flatten().any(|p|std::fs::read_to_string(p.path().join("prefs.js")).is_ok_and(|s|matches!(firefox_trr(&s),2|3))){out.push("Firefox")}}
 out
}
fn checked(id:&str,title:&str,result:Result<String,String>,check:impl FnOnce(&str)->(&'static str,String))->Value{match result{Ok(s)=>{let(level,msg)=check(&s);item(id,title,level,&msg)},Err(e)=>item(id,title,"warn",&format!("не удалось проверить: {e}"))}}
pub fn run(app:&Host,dir:&str)->Value{
 let r=SystemRunner;let mut rows=Vec::new();
 let mut add=|row:Value|{rows.push(row.clone());let _=app.emit("plasto:diag:progress",json!({"done":rows.len(),"total":TOTAL,"last":row}));};
 add(checked("bfe","Base Filtering Engine",run_cmd(&r,"sc.exe",&["query","BFE"]),|s|if s.contains("RUNNING"){("ok","служба работает".into())}else{("fail","служба не запущена".into())}));
 let driver=std::fs::read_dir(std::path::Path::new(dir).join("bin")).ok().is_some_and(|it|it.flatten().any(|e|e.path().extension().is_some_and(|x|x.eq_ignore_ascii_case("sys"))));add(item("driver","Драйвер WinDivert",if driver{"ok"}else{"warn"},if driver{"файл драйвера найден"}else{"файл драйвера не найден или папка недоступна"}));
 let svc=crate::service::status();add(if svc["error"].is_string(){item("windivert","WinDivert","warn",svc["error"].as_str().unwrap_or("ошибка"))}else if svc["windivert"].is_object(){if crate::runtime::other_winws().unwrap_or(false){item("windivert","WinDivert","ok","драйвер загружен запущенным zapret")}else{item("windivert","WinDivert","warn","драйвер зарегистрирован, хотя zapret не запущен; если обход не включается, его можно снять — zapret зарегистрирует свой заново")}}else{item("windivert","WinDivert","ok","зарегистрированный драйвер не найден")});
 // Служба из старых версий перехватывает трафик сама: приложение с ней не работает.
 add(if svc["error"].is_string(){item("oldservice","Служба PLASTO из старой версии","warn",svc["error"].as_str().unwrap_or("ошибка"))}else if svc["installed"]==true{item("oldservice","Служба PLASTO из старой версии","fail","установлена: обходом управляет она, а не приложение")}else{item("oldservice","Служба PLASTO из старой версии","ok","не установлена")});
 let all=run_cmd(&r,"sc.exe",&["query","type=","service","state=","all"]);let active=run_cmd(&r,"sc.exe",&["query","type=","service"]);
 for (id,title,patterns) in [("conflicts","Конфликтующие службы",vec!["killer","smartbyte","goodbyedpi","tracsrvwrapper","epwd","discordfix_zapret"]),("intel","Intel Connectivity",vec!["connectivitynetworkservice","intel connectivity"])] {add(match &all{Ok(s)=>{let low=s.to_lowercase();let hits=patterns.iter().filter(|p|low.contains(**p)).copied().collect::<Vec<_>>();if hits.is_empty(){item(id,title,"ok","известные совпадения не найдены")}else{item(id,title,"warn",&format!("найдены: {} — возможен конфликт",hits.join(", ")))}},Err(e)=>item(id,title,"warn",&format!("не удалось проверить: {e}"))});}
 add(checked("processes","Процессы",run_cmd(&r,"tasklist.exe",&["/FO","CSV","/NH"]),|s|if s.to_ascii_lowercase().contains("adguardsvc.exe"){("warn","AdGuard запущен; возможен конфликт".into())}else{("ok","AdGuard не обнаружен".into())}));
 add(checked("vpn","VPN",active,|s|{let names=s.lines().filter(|l|l.contains("SERVICE_NAME")&&l.to_ascii_lowercase().contains("vpn")).map(str::trim).collect::<Vec<_>>();if names.is_empty(){("ok","службы с VPN в имени не найдены".into())}else{("warn",format!("обнаружены: {}",names.join(", ")))}}));
 add(checked("proxy","Системный прокси",run_cmd(&r,"reg.exe",&["query",r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings","/v","ProxyEnable"]),|s|if s.contains("0x1"){("warn","системный прокси включён".into())}else{("ok","системный прокси выключен".into())}));
 add(checked("tcpts","TCP timestamps",run_cmd(&r,"netsh.exe",&["interface","tcp","show","global"]),|s|{let l=s.lines().find(|l|l.contains("1323")).unwrap_or("").to_ascii_lowercase();if l.contains("enabled"){("ok","включены".into())}else{("warn","состояние не подтверждено; некоторым стратегиям нужны timestamps".into())}}));
 let ps="(Get-ChildItem -Recurse -Path 'HKLM:System\\CurrentControlSet\\Services\\Dnscache\\InterfaceSpecificParameters\\' -ErrorAction SilentlyContinue | Get-ItemProperty | Where-Object { $_.DohFlags -gt 0 } | Measure-Object).Count";
 add(checked("dns","Secure DNS",run_cmd(&r,"powershell.exe",&["-NoProfile","-NonInteractive","-Command",ps]),|s|if s.trim().parse::<u32>().unwrap_or(0)>0{("ok","найдены системные настройки DoH".into())}else{("ok","системный DoH не включён — обходу это не мешает".into())}));
 // Важно только при прокси ИИ в hosts: обходу zapret DoH браузера не мешает.
 let doh=browser_doh();let proxied=crate::hosts::current(&crate::hosts::hosts_path()).proxies>0;
 add(if doh.is_empty(){item("browserdoh","DNS в браузерах","ok","свой DoH в браузерах не включён")}else if proxied{item("browserdoh","DNS в браузерах","warn",&format!("свой DoH включён: {}. Браузер может обойти hosts — если сервис пишет про регион, выключите «Безопасный DNS» и перезапустите браузер",doh.join(", ")))}else{item("browserdoh","DNS в браузерах","ok",&format!("свой DoH включён: {}; прокси ИИ в hosts не стоит, обходу это не мешает",doh.join(", ")))});
 let file=crate::hosts::hosts_path();let hosts=std::fs::read_to_string(file).map_err(|e|e.to_string());add(checked("hosts","Файл hosts",hosts,|s|{let n=s.lines().filter(|l|!l.trim_start().starts_with('#')&&["youtube.com","youtu.be","googlevideo"].iter().any(|d|l.to_ascii_lowercase().contains(d))).count();if n==0{("ok","записей YouTube не найдено".into())}else{("warn",format!("найдено записей YouTube: {n}; проверьте их назначение"))}}));
 let fails=rows.iter().filter(|x|x["level"]=="fail").count();let warns=rows.iter().filter(|x|x["level"]=="warn").count();json!({"results":rows,"fails":fails,"warns":warns,"oks":TOTAL-fails-warns,"verdict":if fails>0{"fail"}else if warns>0{"warn"}else{"ok"},"summary":format!("Ошибок: {fails}, замечаний: {warns}")})
}
#[cfg(test)]mod tests{use super::*;
#[test]fn chromium_only_secure_mode_counts(){assert!(chromium_secure(r#"{"dns_over_https":{"mode":"secure","templates":"https://x/dns-query"}}"#));assert!(!chromium_secure(r#"{"dns_over_https":{"mode":"automatic"}}"#));assert!(!chromium_secure(r#"{"browser":{}}"#));assert!(!chromium_secure("мусор"));}
#[test]fn firefox_trr_last_line_wins_and_default_is_off(){assert_eq!(firefox_trr("user_pref(\"network.trr.mode\", 2);\nuser_pref(\"network.trr.mode\", 5);"),5);assert_eq!(firefox_trr("user_pref(\"network.trr.uri\", \"https://x\");"),0);assert_eq!(firefox_trr("  user_pref(\"network.trr.mode\", 3);\r"),3);}
#[test]fn failed_system_query_is_not_reported_as_ok(){let r=checked("x","test",Err("access".into()),|_|("ok","ok".into()));assert_eq!(r["level"],"warn");}}
