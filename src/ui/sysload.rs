//! Загрузка системы для вкладки «Система»: процессор, память, видеокарта и
//! системный диск. Замеры идут в своём потоке, только пока вкладку смотрят:
//! через несколько секунд без взгляда поток закрывает счётчики и выходит,
//! так что в трее ничего не крутится.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Storage::FileSystem::GetDiskFreeSpaceExW,
    System::{
        Performance::*,
        SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
    },
};

#[derive(Clone, Default)]
pub struct Load {
    /// Проценты, 0–100.
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    /// Активность системного диска.
    pub disk_busy: Option<f64>,
    /// Занято и всего, байты.
    pub ram: Option<(u64, u64)>,
    /// Буква системного диска, свободно и всего, байты.
    pub disk: Option<(String, u64, u64)>,
}

struct Shared {
    load: Option<Load>,
    seen: Option<Instant>,
    running: bool,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { load: None, seen: None, running: false });

/// Последний замер; заодно не даёт потоку замеров уснуть.
pub fn current() -> Option<Load> {
    let mut s = SHARED.lock().ok()?;
    s.seen = Some(Instant::now());
    if !s.running {
        s.running = true;
        std::thread::spawn(sample);
    }
    s.load.clone()
}

const IDLE: Duration = Duration::from_secs(5);
const STEP: Duration = Duration::from_secs(1);

fn sample() {
    let drive = std::env::var("SystemDrive").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "C:".into());
    let q = Query::open(&drive);
    loop {
        std::thread::sleep(STEP);
        let q = q.as_ref().filter(|q| q.collect());
        let load = Load {
            cpu: q.and_then(|q| q.value(q.cpu)).map(|v| v.clamp(0.0, 100.0)),
            gpu: q.and_then(|q| q.sum(q.gpu)).map(|v| v.clamp(0.0, 100.0)),
            disk_busy: q.and_then(|q| q.value(q.disk)).map(|idle| (100.0 - idle).clamp(0.0, 100.0)),
            ram: ram(),
            disk: disk(&drive),
        };
        let Ok(mut s) = SHARED.lock() else { return };
        if s.seen.is_none_or(|t| t.elapsed() > IDLE) {
            s.running = false;
            s.load = None;
            return;
        }
        s.load = Some(load);
    }
}

fn ram() -> Option<(u64, u64)> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..unsafe { std::mem::zeroed() } };
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then(|| (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys))
}

fn disk(drive: &str) -> Option<(String, u64, u64)> {
    let root: Vec<u16> = format!("{drive}\\").encode_utf16().chain([0]).collect();
    let (mut free, mut total) = (0u64, 0u64);
    (unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) } != 0).then(|| (drive.to_owned(), free, total))
}

/// Запрос PDH со счётчиками. Имена английские: PDH сам переводит их под язык
/// системы.
struct Query {
    h: PDH_HQUERY,
    cpu: PDH_HCOUNTER,
    gpu: PDH_HCOUNTER,
    disk: PDH_HCOUNTER,
}

/// `PDH_FMT_NOCAP100`: загрузка процессора на турбочастоте бывает выше 100.
const FMT: PDH_FMT = PDH_FMT_DOUBLE | 0x8000;

impl Query {
    fn open(drive: &str) -> Option<Query> {
        let mut h = std::ptr::null_mut();
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut h) } != 0 {
            return None;
        }
        let add = |path: &str| {
            let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
            let mut c = std::ptr::null_mut();
            if unsafe { PdhAddEnglishCounterW(h, w.as_ptr(), 0, &mut c) } == 0 { c } else { std::ptr::null_mut() }
        };
        // «Процессорная загрузка» как в диспетчере задач; на старых системах — время.
        let mut cpu = add(r"\Processor Information(_Total)\% Processor Utility");
        if cpu.is_null() {
            cpu = add(r"\Processor(_Total)\% Processor Time");
        }
        let q = Query { h, cpu, gpu: add(r"\GPU Engine(*engtype_3D)\Utilization Percentage"), disk: add(&format!(r"\LogicalDisk({drive})\% Idle Time")) };
        // Первый сбор — только опорная точка: проценты считаются по разнице.
        q.collect();
        Some(q)
    }

    fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.h) == 0 }
    }

    fn value(&self, c: PDH_HCOUNTER) -> Option<f64> {
        if c.is_null() {
            return None;
        }
        let mut v = PDH_FMT_COUNTERVALUE::default();
        (unsafe { PdhGetFormattedCounterValue(c, FMT, std::ptr::null_mut(), &mut v) } == 0 && v.CStatus <= 1).then(|| unsafe { v.Anonymous.doubleValue })
    }

    /// Сумма по всем экземплярам: загрузка видеокарты считается по процессам.
    fn sum(&self, c: PDH_HCOUNTER) -> Option<f64> {
        if c.is_null() {
            return None;
        }
        let (mut size, mut count) = (0u32, 0u32);
        if unsafe { PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, std::ptr::null_mut()) } != PDH_MORE_DATA {
            // Нет экземпляров — видеокарта простаивает.
            return Some(0.0);
        }
        let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = vec![Default::default(); size as usize / item + 1];
        if unsafe { PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, buf.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(buf[..count as usize].iter().filter(|i| i.FmtValue.CStatus <= 1).map(|i| unsafe { i.FmtValue.Anonymous.doubleValue }).sum())
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.h) };
    }
}
