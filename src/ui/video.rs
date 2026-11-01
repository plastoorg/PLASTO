//! Живой фон: зацикленное видео той же сцены, что на снимке.
//!
//! Ролик (`assets/scene.mp4`, H.264, 8 с, концы склеены плавным переходом)
//! разбирает Media Foundation — декодер, встроенный в Windows. Кадры идут в
//! NV12 как есть: яркость и цвет отдельными плоскостями, в RGB их переводит
//! шейдер сцены. Своим потоком, в темпе ролика; новый кадр будит окно.
//!
//! Плеер живёт, пока окно на экране и фон — видео; без фокуса стоит на паузе
//! и держит последний кадр. Нет Media Foundation (Windows N без пакета
//! мультимедиа) — поток тихо завершается, остаётся снимок.

use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use windows::{
    core::GUID,
    Win32::{
        Media::MediaFoundation::{
            IMFSourceReader, MFCreateAttributes, MFCreateMFByteStreamOnStream, MFCreateMediaType, MFCreateSourceReaderFromByteStream, MFMediaType_Video, MFShutdown, MFStartup,
            MFVideoFormat_NV12, MFSTARTUP_LITE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
        },
        System::{
            Com::{CoInitializeEx, CoUninitialize, StructuredStorage::PROPVARIANT, COINIT_MULTITHREADED},
            Variant::VT_I8,
        },
        UI::Shell::SHCreateMemStream,
    },
};

const VIDEO: &[u8] = include_bytes!("../../assets/scene.mp4");

/// Кадр в NV12: плоскость яркости `width×height` и чередующиеся U, V вдвое
/// меньше по обеим осям. `seq` растёт с каждым кадром — по нему сцена знает,
/// что пора перерисоваться.
pub struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

#[derive(Default)]
struct Ctl {
    playing: bool,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    ctl: Mutex<Ctl>,
    wake: Condvar,
    latest: Mutex<Option<Arc<Frame>>>,
}

pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn start(ctx: egui::Context) -> Player {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("plasto-video".into())
            .spawn(move || {
                if let Err(e) = play(&worker, &ctx) {
                    eprintln!("[plasto] видео фона: {e}");
                }
            })
            .ok();
        Player { shared, thread }
    }

    /// Играть или стоять на последнем кадре.
    pub fn set_playing(&self, on: bool) {
        if let Ok(mut ctl) = self.shared.ctl.lock() {
            if ctl.playing != on {
                ctl.playing = on;
                self.shared.wake.notify_all();
            }
        }
    }

    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.shared.latest.lock().ok().and_then(|f| f.clone())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Ok(mut ctl) = self.shared.ctl.lock() {
            ctl.stop = true;
        }
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Media Foundation и COM этого потока — до конца воспроизведения.
struct Session;

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

fn reader() -> windows::core::Result<(IMFSourceReader, u32, u32, u32)> {
    unsafe {
        let stream = SHCreateMemStream(Some(VIDEO)).ok_or_else(windows::core::Error::from_thread)?;
        let bytes = MFCreateMFByteStreamOnStream(&stream)?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 0)?;
        let reader = MFCreateSourceReaderFromByteStream(&bytes, attrs.as_ref())?;
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(video, true)?;
        let want = MFCreateMediaType()?;
        want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader.SetCurrentMediaType(video, None, &want)?;
        let got = reader.GetCurrentMediaType(video)?;
        let size = got.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (width, height) = ((size >> 32) as u32, size as u32);
        let stride = got.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(width, |s| (s as i32).unsigned_abs());
        Ok((reader, width, height, stride))
    }
}

/// Поток плеера: кадр за кадром в темпе ролика, по кругу.
fn play(shared: &Shared, ctx: &egui::Context) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| e.to_string())?;
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
            CoUninitialize();
            return Err(format!("Media Foundation недоступен: {e}"));
        }
    }
    let _session = Session;
    let (reader, width, height, stride) = reader().map_err(|e| e.to_string())?;
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // Когда показывать кадр с отметкой 0; на каждом круге сдвигается на длину ролика.
    let mut epoch = Instant::now();
    let mut last = Duration::ZERO;
    let mut seq = 0u64;
    loop {
        // Пауза: ждём, и после неё ролик продолжается с того же места.
        {
            let Ok(mut ctl) = shared.ctl.lock() else { return Ok(()) };
            let paused = !ctl.playing;
            while !ctl.playing && !ctl.stop {
                ctl = shared.wake.wait(ctl).map_err(|_| "плеер".to_string())?;
            }
            if ctl.stop {
                return Ok(());
            }
            if paused {
                epoch = Instant::now() - last;
            }
        }
        let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
        unsafe { reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)) }.map_err(|e| e.to_string())?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            let mut start = PROPVARIANT::default();
            unsafe {
                (*start.Anonymous.Anonymous).vt = VT_I8;
                reader.SetCurrentPosition(&GUID::zeroed(), &start).map_err(|e| e.to_string())?;
            }
            // Следующий круг начинается через кадр после последнего.
            epoch += last + Duration::from_millis(42);
            last = Duration::ZERO;
            continue;
        }
        let Some(sample) = sample else { continue };
        let at = Duration::from_nanos(ts.max(0) as u64 * 100);
        last = at;
        // Ждём своего времени; пауза или выход прерывают ожидание.
        let due = epoch + at;
        let now = Instant::now();
        if due > now {
            let Ok(ctl) = shared.ctl.lock() else { return Ok(()) };
            let (ctl, _) = shared.wake.wait_timeout_while(ctl, due - now, |c| c.playing && !c.stop).map_err(|_| "плеер".to_string())?;
            if ctl.stop {
                return Ok(());
            }
        } else if now - due > Duration::from_millis(500) {
            // Отстали (система была занята) — не догоняем, а продолжаем отсюда.
            epoch = now - at;
        }
        let frame = unsafe { copy(&sample, width, height, stride) }.map_err(|e| e.to_string())?;
        seq += 1;
        if let Ok(mut latest) = shared.latest.lock() {
            *latest = Some(Arc::new(Frame { seq, ..frame }));
        }
        ctx.request_repaint();
    }
}

/// Плоскости кадра без отступов в конце строк. Высота буфера может быть
/// выровнена (1088 у 1080p): где начинается цвет — по длине буфера.
unsafe fn copy(sample: &windows::Win32::Media::MediaFoundation::IMFSample, width: u32, height: u32, stride: u32) -> windows::core::Result<Frame> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
        buffer.Lock(&mut data, None, Some(&mut len))?;
        let raw = std::slice::from_raw_parts(data, len as usize);
        let (w, h, s) = (width as usize, height as usize, stride as usize);
        let rows = (raw.len() / s.max(1)) * 2 / 3;
        let mut y = Vec::with_capacity(w * h);
        let mut uv = Vec::with_capacity(w * h / 2);
        for r in 0..h {
            y.extend_from_slice(&raw[r * s..r * s + w]);
        }
        for r in 0..h / 2 {
            let at = (rows + r) * s;
            uv.extend_from_slice(&raw[at..at + w]);
        }
        buffer.Unlock()?;
        Ok(Frame { seq: 0, width, height, y, uv })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ролик разбирается и идёт по кругу: после конца снова приходят кадры.
    #[test]
    fn ролик_декодируется_и_зацикливается() {
        let ctx = egui::Context::default();
        let player = Player::start(ctx);
        player.set_playing(true);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = 0;
        let mut prev = 0;
        while Instant::now() < deadline && seen < 220 {
            if let Some(f) = player.frame() {
                if f.seq != prev {
                    assert_eq!((f.width, f.height), (1280, 720));
                    assert_eq!(f.y.len(), 1280 * 720);
                    assert_eq!(f.uv.len(), 1280 * 360);
                    prev = f.seq;
                    seen += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // 8 с ролика — 192 кадра; 220 — значит, второй круг пошёл.
        assert!(seen >= 220, "кадров: {seen}");
        // Пауза: новых кадров нет.
        player.set_playing(false);
        std::thread::sleep(Duration::from_millis(200));
        let held = player.frame().map(|f| f.seq);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(player.frame().map(|f| f.seq), held);
    }

    /// NV12 → RGB по BT.601 в ограниченном диапазоне — как в `scene.wgsl`.
    pub fn rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
        let y = (y as f32 - 16.0) * 255.0 / 219.0;
        let (cb, cr) = ((u as f32 - 128.0) * 255.0 / 224.0, (v as f32 - 128.0) * 255.0 / 224.0);
        let c = |x: f32| x.round().clamp(0.0, 255.0) as u8;
        [c(y + 1.402 * cr), c(y - 0.344136 * cb - 0.714136 * cr), c(y + 1.772 * cb)]
    }

    /// Первый кадр сверяется с тем же кадром из ffmpeg:
    /// `PLASTO_FRAME_CHECK=<png первого кадра> cargo test --lib -- --ignored цвета_кадра --nocapture`.
    #[test]
    #[ignore]
    fn цвета_кадра_как_у_ffmpeg() {
        let reference = image::open(std::env::var("PLASTO_FRAME_CHECK").unwrap()).unwrap().to_rgb8();
        let player = Player::start(egui::Context::default());
        player.set_playing(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        let f = loop {
            if let Some(f) = player.frame() {
                break f;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        let w = f.width as usize;
        let (mut sum, mut n) = (0u64, 0u64);
        for yy in (0..f.height as usize).step_by(7) {
            for xx in (0..w).step_by(7) {
                let uvi = (yy / 2) * w + (xx / 2) * 2;
                let got = rgb(f.y[yy * w + xx], f.uv[uvi], f.uv[uvi + 1]);
                let want = reference.get_pixel(xx as u32, yy as u32).0;
                sum += (0..3).map(|i| (got[i] as i32 - want[i] as i32).unsigned_abs() as u64).sum::<u64>();
                n += 3;
            }
        }
        let mean = sum as f64 / n as f64;
        println!("средняя разница с ffmpeg: {mean:.2}");
        assert!(mean < 3.0);
    }
}
