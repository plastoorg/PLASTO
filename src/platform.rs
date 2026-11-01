//! То, что требует Win32 напрямую: вывод кадра в окно, значок в трее, звук,
//! автозапуск и единственный экземпляр. Сторонних крейтов здесь нет
//! сознательно — всё это есть в самой Windows и не должно тянуть рантаймы.

use std::sync::{mpsc, Mutex, OnceLock};

/* ---------- вывод кадра -------------------------------------------------- */

/// Кадр, нарисованный на CPU, уходит в окно через `UpdateLayeredWindow`:
/// альфа попиксельная (скругления и тень на прозрачном фоне), а GPU-драйвер
/// в процесс не загружается вовсе. Пиксели — премультиплицированный BGRA.
#[cfg(windows)]
pub struct Layered {
    hwnd: windows_sys::Win32::Foundation::HWND,
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    bitmap: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut [u8; 4],
    size: (usize, usize),
}

#[cfg(windows)]
impl Layered {
    pub fn new(hwnd: isize) -> Self {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED};
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
        }
        Layered { hwnd, dc: std::ptr::null_mut(), bitmap: std::ptr::null_mut(), bits: std::ptr::null_mut(), size: (0, 0) }
    }

    /// Буфер кадра нужного размера; старый пересоздаётся при смене размера.
    pub fn pixels(&mut self, w: usize, h: usize) -> Option<&mut [[u8; 4]]> {
        use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
        if self.size != (w, h) || self.bits.is_null() {
            self.release();
            unsafe {
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                if dc.is_null() {
                    return None;
                }
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // Отрицательная высота — строки сверху вниз, как в буфере egui.
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut bits = std::ptr::null_mut();
                let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
                if bitmap.is_null() || bits.is_null() {
                    windows_sys::Win32::Graphics::Gdi::DeleteDC(dc);
                    return None;
                }
                SelectObject(dc, bitmap);
                self.dc = dc;
                self.bitmap = bitmap;
                self.bits = bits.cast();
                self.size = (w, h);
            }
        }
        Some(unsafe { std::slice::from_raw_parts_mut(self.bits, w * h) })
    }

    pub fn present(&self) -> bool {
        use windows_sys::Win32::{
            Foundation::{POINT, SIZE},
            Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION},
            UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED};
        if self.bits.is_null() {
            return false;
        }
        // winit пересобирает расширенный стиль окна при каждой смене своих
        // флагов (показ, «поверх окон», сворачивание) и снимает WS_EX_LAYERED.
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            if ex & WS_EX_LAYERED as isize == 0 {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            }
        }
        let size = SIZE { cx: self.size.0 as i32, cy: self.size.1 as i32 };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe { UpdateLayeredWindow(self.hwnd, std::ptr::null_mut(), std::ptr::null(), &size, self.dc, &origin, 0, &blend, ULW_ALPHA) != 0 }
    }

    /// Пока окно спрятано, буфер кадра не держим.
    pub fn release(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        unsafe {
            if !self.dc.is_null() {
                DeleteDC(self.dc);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
        }
        self.dc = std::ptr::null_mut();
        self.bitmap = std::ptr::null_mut();
        self.bits = std::ptr::null_mut();
        self.size = (0, 0);
    }
}

#[cfg(windows)]
impl Drop for Layered {
    fn drop(&mut self) {
        self.release();
    }
}

/// Скругления и тень Windows 11 у окна без системной рамки: при выводе через
/// видеокарту окно непрозрачное, и форму ему задаёт сама система. На Windows 10
/// атрибута нет — вызов ничего не делает, окно остаётся прямоугольным.
#[cfg(windows)]
pub fn round_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    let preference = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            hwnd as windows_sys::Win32::Foundation::HWND,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&preference as *const i32).cast(),
            std::mem::size_of_val(&preference) as u32,
        );
    }
}

/// Отдать неиспользуемые страницы системе: после сворачивания в трей
/// рабочий набор процесса сжимается до того, что реально нужно в фоне.
#[cfg(windows)]
pub fn trim_memory() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
pub fn trim_memory() {}

/// Задание Windows (Job Object), которое при закрытии снимает все свои
/// процессы вместе с их потомками. Последний дескриптор закрывает система,
/// когда PLASTO завершается любым путём — сбоем или из диспетчера задач тоже.
#[cfg(windows)]
pub struct Job(isize);

#[cfg(windows)]
impl Job {
    pub fn new() -> Option<Job> {
        use windows_sys::Win32::System::JobObjects::{CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE};
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let job = Job(handle as isize);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            (SetInformationJobObject(handle, JobObjectExtendedLimitInformation, (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), size) != 0).then_some(job)
        }
    }

    /// Взять процесс в задание. Его будущие потомки попадают туда сами.
    pub fn adopt(&self, child: &std::process::Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.0 as _, child.as_raw_handle() as _) != 0 }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}

/// Дочерние программы PLASTO (скачанный Telegram-прокси) не переживают его:
/// одно задание на всё приложение, дескриптор живёт до конца процесса.
#[cfg(windows)]
pub fn bind_to_app(child: &std::process::Child) {
    static JOB: std::sync::OnceLock<Option<Job>> = std::sync::OnceLock::new();
    if let Some(job) = JOB.get_or_init(Job::new) {
        if !job.adopt(child) {
            eprintln!("[plasto] процесс {} не привязан к PLASTO", child.id());
        }
    }
}

#[cfg(not(windows))]
pub fn bind_to_app(_child: &std::process::Child) {}

/// Включены ли в Windows эффекты анимации («Специальные возможности →
/// Визуальные эффекты»). Выключены — лицо на мониторе не двигается.
#[cfg(windows)]
pub fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION};
    let mut on: i32 = 1;
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut on as *mut i32).cast(), 0) };
    ok == 0 || on != 0
}

#[cfg(not(windows))]
pub fn animations_enabled() -> bool {
    true
}

/// Подождать, пока процесс `pid` выйдет, но не дольше `timeout`. Нужно после
/// обновления: новая версия стартует, когда старая отпустит единственную копию.
#[cfg(windows)]
pub fn wait_exit(pid: u32, timeout: std::time::Duration) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, timeout.as_millis().min(u32::MAX as u128) as u32);
            CloseHandle(h);
        }
    }
}

/// Что пришло из трея или от второй копии приложения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCmd {
    Toggle,
    Show,
    Quit,
}

#[derive(Clone, Copy, Debug)]
pub enum Sound {
    Start,
    Stop,
    Error,
}

/* ---------- время -------------------------------------------------------- */

/// Метка времени из бэкенда (мс от эпохи, UTC) → «ЧЧ:ММ:СС» по местному времени.
#[cfg(windows)]
pub fn local_time(ts_ms: u64) -> String {
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };
    // FILETIME считает сотни наносекунд от 1601 года.
    let ticks = (ts_ms + 11_644_473_600_000) * 10_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0 || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            return "--:--:--".into();
        }
        format!("{:02}:{:02}:{:02}", local.wHour, local.wMinute, local.wSecond)
    }
}

/// Локальные дата и время: «29.10.2026 04:30».
#[cfg(windows)]
pub fn local_stamp(ts_ms: u64) -> String {
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };
    let ticks = (ts_ms + 11_644_473_600_000) * 10_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0 || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            return "—".into();
        }
        format!("{:02}.{:02}.{} {:02}:{:02}", local.wDay, local.wMonth, local.wYear, local.wHour, local.wMinute)
    }
}

#[cfg(not(windows))]
pub fn local_stamp(ts_ms: u64) -> String {
    format!("{} (UTC)", local_time(ts_ms))
}

#[cfg(not(windows))]
pub fn local_time(ts_ms: u64) -> String {
    let s = ts_ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600 % 24, s / 60 % 60, s % 60)
}

/* ---------- автозапуск --------------------------------------------------- */

const TASK_NAME: &str = "PLASTO";

/// Автозапуск через Планировщик, а не через ключ Run: exe с
/// requireAdministrator из Run Windows молча не запускает, а задача с
/// «наивысшими правами» стартует без запроса UAC.
pub fn set_autostart(on: bool) -> Result<(), String> {
    if !on {
        let out = hidden("schtasks.exe").args(["/Delete", "/TN", TASK_NAME, "/F"]).output().map_err(|e| e.to_string())?;
        // Задачи может и не быть — это тоже «выключено».
        let text = console_text(&out.stderr).to_lowercase();
        return if out.status.success() || text.contains("cannot find") || text.contains("не удается найти") { Ok(()) } else { Err(text.trim().to_owned()) };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Err("не удалось определить пользователя".into()),
    };
    let xml = task_xml(&exe.to_string_lossy(), &user);
    let mut file = tempfile::Builder::new().prefix("plasto-task-").suffix(".xml").tempfile().map_err(|e| e.to_string())?;
    // schtasks ждёт UTF-16 с BOM, как в объявлении XML.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::io::Write::write_all(&mut file, &bytes).map_err(|e| e.to_string())?;
    let out = hidden("schtasks.exe").args(["/Create", "/TN", TASK_NAME, "/F", "/XML"]).arg(file.path()).output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(console_text(&out.stderr).trim().to_owned()) }
}

fn task_xml(exe: &str, user: &str) -> String {
    // От батареи не зависим: по умолчанию Планировщик не стартует задачи на
    // ноутбуке без зарядки, и обход «иногда не включается» без видимой причины.
    format!(r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--autostart</Arguments></Exec></Actions>
</Task>
"#, user = xml_escape(user), exe = xml_escape(exe))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Текст из вывода консольной программы. schtasks, sc, netsh и прочие пишут в
/// трубу в OEM-кодировке (в русской Windows это CP866), а не в UTF-8, — без
/// перекодирования человек видит вопросики вместо сообщения об ошибке.
pub fn console_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP};
        let Ok(len) = i32::try_from(bytes.len()) else { return String::from_utf8_lossy(bytes).into_owned() };
        unsafe {
            let n = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
            if n > 0 {
                let mut wide = vec![0u16; n as usize];
                if MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n) == n {
                    return String::from_utf16_lossy(&wide);
                }
            }
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn hidden(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/* ---------- звук --------------------------------------------------------- */

/// mp3 играет MCI — он есть в любой Windows, аудио-крейты не нужны.
/// Отдельный поток: открытое MCI-устройство живёт в потоке, который его открыл.
pub fn play(sound: Sound) {
    static TX: OnceLock<Mutex<mpsc::Sender<Sound>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Sound>();
        std::thread::spawn(move || sound_loop(rx));
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send(sound);
    }
}

#[cfg(windows)]
fn sound_loop(rx: mpsc::Receiver<Sound>) {
    use windows_sys::Win32::Media::Multimedia::mciSendStringW;
    let mci = |cmd: &str| unsafe {
        let w = wide(cmd);
        mciSendStringW(w.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut())
    };
    let dir = std::env::temp_dir();
    for sound in rx {
        let (name, bytes): (&str, &[u8]) = match sound {
            Sound::Start => ("start", include_bytes!("../assets/start.mp3")),
            Sound::Stop => ("stop", include_bytes!("../assets/stop.mp3")),
            Sound::Error => ("error", include_bytes!("../assets/error.mp3")),
        };
        let path = dir.join(format!("plasto-{name}.mp3"));
        if std::fs::metadata(&path).map(|m| m.len() != bytes.len() as u64).unwrap_or(true) && std::fs::write(&path, bytes).is_err() {
            continue;
        }
        mci("close plasto_sfx");
        if mci(&format!("open \"{}\" type mpegvideo alias plasto_sfx", path.display())) == 0 {
            mci("play plasto_sfx");
        }
    }
}

#[cfg(not(windows))]
fn sound_loop(rx: mpsc::Receiver<Sound>) {
    for _ in rx {}
}

/* ---------- трей и единственный экземпляр ------------------------------- */

#[cfg(windows)]
const TRAY_CLASS: &str = "PlastoTrayWindow";

/// Вторая копия не запускается, а будит первую. `false` — мы не первые.
#[cfg(windows)]
pub fn claim_single_instance() -> bool {
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{FindWindowW, PostMessageW},
    };
    unsafe {
        let name = wide("Local\\PLASTO-single-instance");
        // Хэндл намеренно не закрываем: мьютекс живёт, пока жив процесс.
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() || GetLastError() != ERROR_ALREADY_EXISTS {
            return true;
        }
        let class = wide(TRAY_CLASS);
        let other = FindWindowW(class.as_ptr(), std::ptr::null());
        if !other.is_null() {
            PostMessageW(other, WM_SHOW_MAIN, 0, 0);
        }
        false
    }
}

#[cfg(not(windows))]
pub fn claim_single_instance() -> bool {
    true
}

#[cfg(windows)]
const WM_TRAY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 1;
#[cfg(windows)]
const WM_SHOW_MAIN: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
#[cfg(windows)]
const WM_SET_TIP: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;

#[cfg_attr(not(windows), allow(dead_code))]
struct Tray {
    on_cmd: Box<dyn Fn(TrayCmd) + Send + Sync>,
    hwnd: Mutex<isize>,
    tip: Mutex<String>,
}

static TRAY: OnceLock<Tray> = OnceLock::new();

/// Значок в трее на своём потоке со своей очередью сообщений: так он не
/// зависит от цикла окна и откликается, даже пока окно спрятано.
pub fn spawn_tray(on_cmd: impl Fn(TrayCmd) + Send + Sync + 'static) {
    let tray = Tray { on_cmd: Box::new(on_cmd), hwnd: Mutex::new(0), tip: Mutex::new("PLASTO".into()) };
    if TRAY.set(tray).is_err() {
        return;
    }
    #[cfg(windows)]
    std::thread::spawn(tray_thread);
}

/// Подсказка при наведении: состояние видно, не открывая окно.
pub fn set_tray_tip(text: &str) {
    let Some(tray) = TRAY.get() else { return };
    if let Ok(mut tip) = tray.tip.lock() {
        if *tip == text {
            return;
        }
        *tip = text.to_owned();
    }
    #[cfg(windows)]
    if let Ok(hwnd) = tray.hwnd.lock() {
        if *hwnd != 0 {
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(*hwnd as _, WM_SET_TIP, 0, 0) };
        }
    }
}

/// Всплывающее уведомление у значка: обход сломался, пока окно в трее.
pub fn notify(title: &str, text: &str) {
    #[cfg(windows)]
    if let Some(tray) = TRAY.get() {
        if let Ok(hwnd) = tray.hwnd.lock() {
            if *hwnd != 0 {
                unsafe {
                    use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_INFO, NIIF_WARNING, NIM_MODIFY};
                    let mut data = notify_data(*hwnd as _);
                    data.uFlags = NIF_INFO;
                    data.dwInfoFlags = NIIF_WARNING;
                    let copy = |dst: &mut [u16], src: &str| {
                        let w: Vec<u16> = src.encode_utf16().take(dst.len() - 1).collect();
                        dst[..w.len()].copy_from_slice(&w);
                        dst[w.len()] = 0;
                    };
                    copy(&mut data.szInfoTitle, title);
                    copy(&mut data.szInfo, text);
                    Shell_NotifyIconW(NIM_MODIFY, &data);
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (title, text);
}

/// Убрать значок сразу: иначе после выхода он висит, пока на него не наведут мышь.
pub fn remove_tray() {
    #[cfg(windows)]
    if let Some(tray) = TRAY.get() {
        if let Ok(hwnd) = tray.hwnd.lock() {
            if *hwnd != 0 {
                unsafe {
                    let mut data = notify_data(*hwnd as _);
                    windows_sys::Win32::UI::Shell::Shell_NotifyIconW(windows_sys::Win32::UI::Shell::NIM_DELETE, &mut data);
                }
            }
        }
    }
}

#[cfg(windows)]
fn tray_thread() {
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage, MSG, WNDCLASSW},
    };
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide(TRAY_CLASS);
        let wc = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        // Обычное невидимое окно, а не message-only: только такие получают
        // широковещательное TaskbarCreated после перезапуска проводника.
        let title = wide("PLASTO tray");
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null());
        if hwnd.is_null() {
            return;
        }
        if let Some(tray) = TRAY.get() {
            if let Ok(mut h) = tray.hwnd.lock() {
                *h = hwnd as isize;
            }
        }
        add_icon(hwnd);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(windows)]
unsafe fn notify_data(hwnd: windows_sys::Win32::Foundation::HWND) -> windows_sys::Win32::UI::Shell::NOTIFYICONDATAW {
    use windows_sys::Win32::UI::Shell::NOTIFYICONDATAW;
    let mut data: NOTIFYICONDATAW = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data
}

#[cfg(windows)]
unsafe fn fill_tip(data: &mut windows_sys::Win32::UI::Shell::NOTIFYICONDATAW) {
    let text = TRAY.get().and_then(|t| t.tip.lock().ok().map(|s| s.clone())).unwrap_or_else(|| "PLASTO".into());
    let w: Vec<u16> = text.encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip[..w.len()].copy_from_slice(&w);
    data.szTip[w.len()] = 0;
}

#[cfg(windows)]
unsafe fn add_icon(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD},
            WindowsAndMessaging::{GetSystemMetrics, LoadImageW, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXSMICON, SM_CYSMICON},
        },
    };
    let mut data = notify_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    // Значок вшит в exe ресурсом №1 (assets/plasto.rc): система сама берёт
    // нужный размер из .ico под текущий масштаб экрана.
    data.hIcon = LoadImageW(
        GetModuleHandleW(std::ptr::null()),
        1 as _,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON),
        GetSystemMetrics(SM_CYSMICON),
        LR_DEFAULTCOLOR,
    ) as _;
    fill_tip(&mut data);
    Shell_NotifyIconW(NIM_ADD, &data);
}

#[cfg(windows)]
unsafe extern "system" fn tray_proc(hwnd: windows_sys::Win32::Foundation::HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    use windows_sys::Win32::UI::{
        Shell::{Shell_NotifyIconW, NIF_TIP, NIM_MODIFY},
        WindowsAndMessaging::{DefWindowProcW, RegisterWindowMessageW, WM_LBUTTONUP, WM_RBUTTONUP},
    };
    static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
    let taskbar_created = *TASKBAR_CREATED.get_or_init(|| {
        let name = wide("TaskbarCreated");
        RegisterWindowMessageW(name.as_ptr())
    });
    let send = |cmd: TrayCmd| {
        if let Some(tray) = TRAY.get() {
            (tray.on_cmd)(cmd);
        }
    };
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => send(TrayCmd::Toggle),
                WM_RBUTTONUP => {
                    if let Some(cmd) = tray_menu(hwnd) {
                        send(cmd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_SHOW_MAIN => {
            send(TrayCmd::Show);
            0
        }
        WM_SET_TIP => {
            let mut data = notify_data(hwnd);
            data.uFlags = NIF_TIP;
            fill_tip(&mut data);
            Shell_NotifyIconW(NIM_MODIFY, &data);
            0
        }
        m if m == taskbar_created => {
            add_icon(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(windows)]
unsafe fn tray_menu(hwnd: windows_sys::Win32::Foundation::HWND) -> Option<TrayCmd> {
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow, TrackPopupMenu, MF_SEPARATOR, MF_STRING,
            TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
        },
    };
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return None;
    }
    let show = wide("Показать");
    let quit = wide("Выход");
    AppendMenuW(menu, MF_STRING, 1, show.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, 2, quit.as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // Без этого меню не закрывается кликом мимо него (известная особенность Shell).
    SetForegroundWindow(hwnd);
    let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, std::ptr::null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    match picked {
        1 => Some(TrayCmd::Show),
        2 => Some(TrayCmd::Quit),
        _ => None,
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Закрыли задание — его процесс и потомок, запущенный уже после
    /// привязки, сняты.
    #[cfg(windows)]
    #[test]
    fn job_takes_children_down_with_it() {
        use std::os::windows::process::CommandExt;
        let job = Job::new().expect("задание создано");
        let mut child = std::process::Command::new("cmd.exe").args(["/c", "ping -n 30 127.0.0.1 >nul"]).creation_flags(0x08000000).spawn().unwrap();
        assert!(job.adopt(&child));
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(job);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "процесс пережил задание");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn console_text_reads_utf8_and_oem() {
        assert_eq!(console_text("Ошибка".as_bytes()), "Ошибка");
        // «Ошибка» в CP866 — так её пишет schtasks в русской Windows.
        #[cfg(windows)]
        if unsafe { windows_sys::Win32::Globalization::GetOEMCP() } == 866 {
            assert_eq!(console_text(&[0x8E, 0xE8, 0xA8, 0xA1, 0xAA, 0xA0]), "Ошибка");
        }
    }

    #[test]
    fn task_xml_escapes_paths() {
        let xml = task_xml(r"C:\Tom & Jerry\plasto.exe", r"PC\user");
        assert!(xml.contains(r"<Command>C:\Tom &amp; Jerry\plasto.exe</Command>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
    }
}
