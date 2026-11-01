//! Windows: ICMP, маршрут по умолчанию, счётчики адаптера, Wi-Fi, драйвер из
//! реестра и журнал событий. Всё — штатные API, без сторонних утилит.
//!
//! На других системах — заглушки с тем же видом: сборщик просто ничего не
//! намеряет, а логика и её тесты собираются и на Linux (CI на Raspberry Pi).

use std::net::Ipv4Addr;

/// Ответ на эхо-запрос.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Echo {
    Reply { from: Ipv4Addr, ms: f32 },
    /// Узел на пути вернул «время жизни истекло» — так ищется traceroute.
    Expired { from: Ipv4Addr },
    Timeout,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Iface {
    pub index: u32,
    pub guid: String,
    pub name: String,
    pub wifi: bool,
    pub up: bool,
    pub link_mbps: u32,
    pub errors: u64,
    pub discards: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wifi {
    pub connected: bool,
    pub ssid: String,
    pub bssid: String,
    pub quality: u8,
    pub rssi: Option<i32>,
    pub rx_mbps: u32,
    pub channel: Option<u32>,
}

/// Запись журнала событий: только то, что нужно мониторингу.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    pub record_id: u64,
    pub ts: i64,
    pub provider: String,
    pub id: u32,
    pub level: u8,
    /// Поля `EventData` по порядку: (имя, значение).
    pub data: Vec<(String, String)>,
    pub message: Option<String>,
}

/// Разбирает XML события из `EvtRender`. Отдельно от вызова — чтобы
/// проверялось тестом и на Linux.
pub fn parse_event_xml(xml: &str) -> Option<Record> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let at = xml.find(&format!("<{tag} "))?;
        let rest = &xml[at..];
        let end = rest.find('>')?;
        let head = &rest[..end];
        let key = format!("{name}='");
        let (q, start) = match head.find(&key) {
            Some(i) => ('\'', i + key.len()),
            None => ('"', head.find(&format!("{name}=\""))? + name.len() + 2),
        };
        let len = head[start..].find(q)?;
        Some(head[start..start + len].to_owned())
    };
    let text = |tag: &str| -> Option<String> {
        let open = xml.find(&format!("<{tag}"))?;
        let body = open + xml[open..].find('>')? + 1;
        let len = xml[body..].find(&format!("</{tag}>"))?;
        Some(unescape(&xml[body..body + len]))
    };
    let mut data = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<Data") {
        rest = &rest[at + 5..];
        let close = rest.find('>')?;
        let head = &rest[..close];
        let name = head.split_once("Name='").or_else(|| head.split_once("Name=\"")).and_then(|(_, t)| t.split(['\'', '"']).next()).unwrap_or("").to_owned();
        if head.ends_with('/') {
            data.push((name, String::new()));
            continue;
        }
        let body = &rest[close + 1..];
        let len = body.find("</Data>")?;
        data.push((name, unescape(&body[..len])));
    }
    Some(Record {
        record_id: text("EventRecordID")?.trim().parse().ok()?,
        ts: super::analyze::parse_utc(&attr("TimeCreated", "SystemTime")?)?,
        provider: attr("Provider", "Name")?,
        id: text("EventID")?.trim().parse().ok()?,
        level: text("Level").and_then(|l| l.trim().parse().ok()).unwrap_or(4),
        data,
        message: None,
    })
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(windows)]
pub use imp::*;
#[cfg(not(windows))]
pub use stub::*;

#[cfg(windows)]
mod imp {
    use std::{collections::HashMap, net::Ipv4Addr, time::Instant};

    use windows_sys::{
        core::GUID,
        Win32::{
            Foundation::{HANDLE, INVALID_HANDLE_VALUE},
            NetworkManagement::{
                IpHelper::{GetBestRoute2, GetIfEntry2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY, IF_TYPE_IEEE80211, IP_OPTION_INFORMATION, IP_SUCCESS, IP_TTL_EXPIRED_TRANSIT, MIB_IF_ROW2, MIB_IPFORWARD_ROW2},
                Ndis::MediaConnectStateConnected,
                WiFi::{
                    wlan_intf_opcode_channel_number, wlan_intf_opcode_current_connection, wlan_intf_opcode_rssi, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
                    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
                },
            },
            Networking::WinSock::{AF_INET, SOCKADDR_INET},
            System::{
                EventLog::{EvtClose, EvtFormatMessage, EvtFormatMessageEvent, EvtNext, EvtOpenPublisherMetadata, EvtQuery, EvtQueryChannelPath, EvtQueryForwardDirection, EvtRender, EvtRenderEventXml},
                Registry::{RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_SZ},
            },
        },
    };

    use super::{parse_event_xml, Echo, Iface, Record, Wifi};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    pub fn guid_string(g: &GUID) -> String {
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
        )
    }

    /* ---------- ICMP ---------------------------------------------------- */

    /// Эхо-запрос; `ttl` — для поиска узлов на пути. Время — по своим часам:
    /// ICMP API отдаёт целые миллисекунды, а до роутера бывает 0,3 мс.
    pub fn ping(to: Ipv4Addr, ttl: Option<u8>, timeout_ms: u32) -> Echo {
        unsafe {
            let h: HANDLE = IcmpCreateFile();
            if h == INVALID_HANDLE_VALUE || h.is_null() {
                return Echo::Timeout;
            }
            let payload = [0x50u8; 32];
            let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + payload.len() + 64];
            let opts = IP_OPTION_INFORMATION { Ttl: ttl.unwrap_or(128), Tos: 0, Flags: 0, OptionsSize: 0, OptionsData: std::ptr::null_mut() };
            let start = Instant::now();
            let n = IcmpSendEcho(
                h,
                u32::from_ne_bytes(to.octets()),
                payload.as_ptr().cast(),
                payload.len() as u16,
                &opts,
                reply.as_mut_ptr().cast(),
                reply.len() as u32,
                timeout_ms,
            );
            let ms = start.elapsed().as_secs_f32() * 1000.0;
            IcmpCloseHandle(h);
            if n == 0 {
                return Echo::Timeout;
            }
            let r = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
            let from = Ipv4Addr::from(r.Address.to_ne_bytes());
            match r.Status {
                IP_SUCCESS => Echo::Reply { from, ms },
                IP_TTL_EXPIRED_TRANSIT => Echo::Expired { from },
                _ => Echo::Timeout,
            }
        }
    }

    /* ---------- маршрут и адаптер -------------------------------------- */

    /// Через какой интерфейс и шлюз Windows отправит пакет на `to`.
    pub fn route(to: Ipv4Addr) -> Option<(u32, Option<Ipv4Addr>)> {
        unsafe {
            let mut dest: SOCKADDR_INET = std::mem::zeroed();
            dest.Ipv4.sin_family = AF_INET;
            dest.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(to.octets());
            let mut row: MIB_IPFORWARD_ROW2 = std::mem::zeroed();
            let mut src: SOCKADDR_INET = std::mem::zeroed();
            if GetBestRoute2(std::ptr::null(), 0, std::ptr::null(), &dest, 0, &mut row, &mut src) != 0 {
                return None;
            }
            let hop = row.NextHop.Ipv4.sin_addr.S_un.S_addr;
            let gw = (row.NextHop.si_family == AF_INET && hop != 0).then(|| Ipv4Addr::from(hop.to_ne_bytes()));
            Some((row.InterfaceIndex, gw))
        }
    }

    pub fn iface(index: u32) -> Option<Iface> {
        unsafe {
            let mut row: MIB_IF_ROW2 = std::mem::zeroed();
            row.InterfaceIndex = index;
            if GetIfEntry2(&mut row) != 0 {
                return None;
            }
            Some(Iface {
                index,
                guid: guid_string(&row.InterfaceGuid),
                name: from_wide(&row.Description),
                wifi: row.Type == IF_TYPE_IEEE80211,
                up: row.MediaConnectState == MediaConnectStateConnected,
                link_mbps: (row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed) / 1_000_000).min(u32::MAX as u64) as u32,
                errors: row.InErrors + row.OutErrors,
                discards: row.InDiscards + row.OutDiscards,
            })
        }
    }

    /* ---------- Wi-Fi --------------------------------------------------- */

    /// Открытая сессия WLAN: открывать её каждую секунду дорого.
    pub struct Wlan(HANDLE);

    // Хэндл WLAN можно передавать между потоками.
    unsafe impl Send for Wlan {}

    impl Wlan {
        pub fn open() -> Option<Wlan> {
            unsafe {
                let (mut ver, mut h) = (0u32, std::ptr::null_mut());
                (WlanOpenHandle(2, std::ptr::null(), &mut ver, &mut h) == 0).then_some(Wlan(h))
            }
        }

        fn query<T: Copy>(&self, guid: &GUID, op: i32) -> Option<T> {
            unsafe {
                let (mut size, mut data, mut kind) = (0u32, std::ptr::null_mut(), 0);
                if WlanQueryInterface(self.0, guid, op, std::ptr::null(), &mut size, &mut data, &mut kind) != 0 || data.is_null() {
                    return None;
                }
                let v = (size as usize >= std::mem::size_of::<T>()).then(|| *(data as *const T));
                WlanFreeMemory(data);
                v
            }
        }

        /// Состояние Wi-Fi адаптера с этим GUID (строкой, как в `Iface`).
        pub fn state(&self, guid: &str) -> Option<Wifi> {
            unsafe {
                let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
                if WlanEnumInterfaces(self.0, std::ptr::null(), &mut list) != 0 || list.is_null() {
                    return None;
                }
                let n = (*list).dwNumberOfItems as usize;
                let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
                let found = items.iter().find(|i| guid_string(&i.InterfaceGuid).eq_ignore_ascii_case(guid)).map(|i| i.InterfaceGuid);
                WlanFreeMemory(list.cast());
                let g = found?;
                let Some(conn) = self.query::<WLAN_CONNECTION_ATTRIBUTES>(&g, wlan_intf_opcode_current_connection) else {
                    return Some(Wifi::default());
                };
                let a = &conn.wlanAssociationAttributes;
                let len = (a.dot11Ssid.uSSIDLength as usize).min(32);
                Some(Wifi {
                    connected: true,
                    ssid: String::from_utf8_lossy(&a.dot11Ssid.ucSSID[..len]).into_owned(),
                    bssid: a.dot11Bssid.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"),
                    quality: a.wlanSignalQuality.min(100) as u8,
                    rssi: self.query::<i32>(&g, wlan_intf_opcode_rssi),
                    rx_mbps: a.ulRxRate / 1000,
                    channel: self.query::<u32>(&g, wlan_intf_opcode_channel_number),
                })
            }
        }
    }

    impl Drop for Wlan {
        fn drop(&mut self) {
            unsafe {
                WlanCloseHandle(self.0, std::ptr::null());
            }
        }
    }

    /* ---------- драйвер из реестра ------------------------------------- */

    struct Key(HKEY);

    impl Key {
        fn open(parent: HKEY, path: &str) -> Option<Key> {
            unsafe {
                let mut h: HKEY = std::ptr::null_mut();
                (RegOpenKeyExW(parent, wide(path).as_ptr(), 0, KEY_READ, &mut h) == 0).then_some(Key(h))
            }
        }

        fn string(&self, name: &str) -> Option<String> {
            unsafe {
                let (mut kind, mut size) = (0u32, 0u32);
                let n = wide(name);
                if RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size) != 0 || kind != REG_SZ {
                    return None;
                }
                let mut buf = vec![0u16; size as usize / 2 + 1];
                if RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null(), &mut kind, buf.as_mut_ptr().cast(), &mut size) != 0 {
                    return None;
                }
                Some(from_wide(&buf))
            }
        }

        fn dword(&self, name: &str) -> Option<u32> {
            unsafe {
                let (mut kind, mut v, mut size) = (0u32, 0u32, 4u32);
                (RegQueryValueExW(self.0, wide(name).as_ptr(), std::ptr::null(), &mut kind, (&mut v as *mut u32).cast(), &mut size) == 0 && kind == REG_DWORD).then_some(v)
            }
        }

        fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            for i in 0.. {
                let mut buf = [0u16; 256];
                let mut len = buf.len() as u32;
                let rc = unsafe { RegEnumKeyExW(self.0, i, buf.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
                if rc != 0 {
                    break;
                }
                out.push(from_wide(&buf));
            }
            out
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    const NET_CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";

    /// Драйвер адаптера: версия, дата, производитель, имя службы (так драйвер
    /// подписывается в журнале событий) и разрешено ли Windows отключать
    /// адаптер ради экономии энергии.
    pub fn adapter_info(guid: &str) -> serde_json::Value {
        let Some(class) = Key::open(HKEY_LOCAL_MACHINE, NET_CLASS) else {
            return serde_json::Value::Null;
        };
        for sub in class.subkeys() {
            let Some(k) = Key::open(class.0, &sub) else { continue };
            if !k.string("NetCfgInstanceId").is_some_and(|g| g.eq_ignore_ascii_case(guid)) {
                continue;
            }
            // PnPCapabilities: бит 0x10 — «не отключать для экономии энергии».
            let power_saving = k.dword("PnPCapabilities").is_none_or(|v| v & 0x10 == 0);
            let service = Key::open(HKEY_LOCAL_MACHINE, &format!(r"SYSTEM\CurrentControlSet\Control\Network\{{4D36E972-E325-11CE-BFC1-08002BE10318}}\{guid}\Connection"))
                .and_then(|c| c.string("PnPInstanceId"))
                .and_then(|id| Key::open(HKEY_LOCAL_MACHINE, &format!(r"SYSTEM\CurrentControlSet\Enum\{id}")))
                .and_then(|e| e.string("Service"));
            return serde_json::json!({
                "name": k.string("DriverDesc"), "provider": k.string("ProviderName"),
                "version": k.string("DriverVersion"), "date": k.string("DriverDate"),
                "service": service, "powerSaving": power_saving,
            });
        }
        serde_json::Value::Null
    }

    /* ---------- журнал событий ----------------------------------------- */

    struct Evt(isize);

    impl Drop for Evt {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    EvtClose(self.0);
                }
            }
        }
    }

    fn render(event: isize) -> Option<String> {
        unsafe {
            let (mut used, mut props) = (0u32, 0u32);
            EvtRender(0, event, EvtRenderEventXml, 0, std::ptr::null_mut(), &mut used, &mut props);
            if used == 0 {
                return None;
            }
            let mut buf = vec![0u16; used as usize / 2 + 1];
            (EvtRender(0, event, EvtRenderEventXml, used, buf.as_mut_ptr().cast(), &mut used, &mut props) != 0).then(|| from_wide(&buf))
        }
    }

    fn message(publishers: &mut HashMap<String, Evt>, provider: &str, event: isize) -> Option<String> {
        unsafe {
            let meta = publishers.entry(provider.to_owned()).or_insert_with(|| Evt(EvtOpenPublisherMetadata(0, wide(provider).as_ptr(), std::ptr::null(), 0, 0)));
            if meta.0 == 0 {
                return None;
            }
            let mut used = 0u32;
            EvtFormatMessage(meta.0, event, 0, 0, std::ptr::null(), EvtFormatMessageEvent, 0, std::ptr::null_mut(), &mut used);
            if used == 0 {
                return None;
            }
            let mut buf = vec![0u16; used as usize + 1];
            (EvtFormatMessage(meta.0, event, 0, 0, std::ptr::null(), EvtFormatMessageEvent, used, buf.as_mut_ptr(), &mut used) != 0).then(|| from_wide(&buf).trim().to_owned())
        }
    }

    /// События канала по XPath-запросу, от старых к новым. `with_text` —
    /// для каких провайдеров нужен текст сообщения (он дорогой).
    pub fn events(channel: &str, query: &str, with_text: &dyn Fn(&Record) -> bool, limit: usize) -> Vec<Record> {
        let mut out = Vec::new();
        let mut publishers = HashMap::new();
        unsafe {
            let q = Evt(EvtQuery(0, wide(channel).as_ptr(), wide(query).as_ptr(), EvtQueryChannelPath | EvtQueryForwardDirection));
            if q.0 == 0 {
                return out;
            }
            'outer: loop {
                let mut batch = [0isize; 32];
                let mut got = 0u32;
                if EvtNext(q.0, batch.len() as u32, batch.as_mut_ptr(), 1000, 0, &mut got) == 0 {
                    break;
                }
                for &h in &batch[..got as usize] {
                    let ev = Evt(h);
                    if let Some(mut r) = render(ev.0).as_deref().and_then(parse_event_xml) {
                        if with_text(&r) {
                            r.message = message(&mut publishers, &r.provider, ev.0);
                        }
                        out.push(r);
                    }
                    if out.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod stub {
    use std::net::Ipv4Addr;

    use super::{Echo, Iface, Record, Wifi};

    pub fn ping(_to: Ipv4Addr, _ttl: Option<u8>, _timeout_ms: u32) -> Echo {
        Echo::Timeout
    }

    pub fn route(_to: Ipv4Addr) -> Option<(u32, Option<Ipv4Addr>)> {
        None
    }

    pub fn iface(_index: u32) -> Option<Iface> {
        None
    }

    pub struct Wlan;

    impl Wlan {
        pub fn open() -> Option<Wlan> {
            None
        }

        pub fn state(&self, _guid: &str) -> Option<Wifi> {
            None
        }
    }

    pub fn adapter_info(_guid: &str) -> serde_json::Value {
        serde_json::Value::Null
    }

    pub fn events(_channel: &str, _query: &str, _with_text: &dyn Fn(&Record) -> bool, _limit: usize) -> Vec<Record> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_события_wlan_разбирается() {
        let xml = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Microsoft-Windows-WLAN-AutoConfig' Guid='{9580d7dd}'/><EventID>8003</EventID><Version>0</Version><Level>4</Level><TimeCreated SystemTime='2026-10-16T14:11:24.1234567Z'/><EventRecordID>854</EventRecordID></System><EventData><Data Name='InterfaceGuid'>{b9f927b8}</Data><Data Name='SSID'>slav&amp;5g</Data><Data Name='Empty'/><Data Name='Reason'>Сеть отключена</Data></EventData></Event>"#;
        let r = parse_event_xml(xml).unwrap();
        assert_eq!((r.record_id, r.id, r.level, r.ts), (854, 8003, 4, 1_792_159_884));
        assert_eq!(r.provider, "Microsoft-Windows-WLAN-AutoConfig");
        assert_eq!(r.data[1], ("SSID".into(), "slav&5g".into()));
        assert_eq!(r.data[2], ("Empty".into(), String::new()));
        assert_eq!(r.data[3].1, "Сеть отключена");
        assert!(parse_event_xml("<Event/>").is_none());
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "живая система: cargo test --lib -- --ignored --nocapture живая_система"]
    fn живая_система() {
        let target = Ipv4Addr::new(77, 88, 8, 8);
        let (index, gw) = route(target).expect("маршрут");
        println!("маршрут: интерфейс {index}, шлюз {gw:?}");
        let ifc = iface(index).expect("адаптер");
        println!("адаптер: {ifc:?}");
        println!("драйвер: {}", adapter_info(&ifc.guid));
        if let Some(gw) = gw {
            println!("пинг шлюза: {:?}", ping(gw, None, 1000));
        }
        println!("пинг 77.88.8.8: {:?}", ping(target, None, 1000));
        for ttl in 1..=4 {
            println!("ttl {ttl}: {:?}", ping(target, Some(ttl), 1000));
        }
        if let Some(w) = Wlan::open() {
            println!("wifi этого адаптера: {:?}", w.state(&ifc.guid));
        }
        let ev = events("System", "*[System[Provider[@Name='Microsoft-Windows-NDIS'] and (EventID=10400 or EventID=10401)]]", &|_| true, 3);
        println!("NDIS: {ev:#?}");
    }
}
