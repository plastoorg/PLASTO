//! Вывод кадра через видеокарту: wgpu поверх DirectX 12.
//!
//! Видеопамять нужна только пока окно на экране. При уходе в трей `Gpu`
//! уничтожается целиком — устройство, поверхность окна и все текстуры, — а при
//! показе создаётся заново: окно в трее не держит ни байта видеопамяти.

use std::{
    future::Future,
    num::NonZeroU32,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use egui::ViewportId;
use egui_wgpu::{winit::Painter, RendererOptions, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use winit::window::Window;

pub struct Gpu {
    painter: Painter,
    size: (u32, u32),
}

impl Gpu {
    /// Поднимает устройство и поверхность окна. Ошибка — нет подходящей
    /// видеокарты или драйвера: тогда окно рисуется на CPU.
    pub fn new(ctx: &egui::Context, window: Arc<Window>) -> Result<Self, String> {
        let mut setup = WgpuSetupCreateNew::without_display_handle();
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        // Окну хватает встроенной графики, дискретную будить незачем.
        setup.power_preference = wgpu::PowerPreference::LowPower;
        // По умолчанию wgpu берёт видеопамять крупными блоками под игры; окну
        // с парой текстур нужны маленькие — иначе десятки мегабайт впустую.
        setup.device_descriptor = Arc::new(|_| wgpu::DeviceDescriptor {
            label: Some("plasto"),
            required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..wgpu::Limits::default() },
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        });
        let config = WgpuConfiguration {
            present_mode: wgpu::PresentMode::AutoVsync,
            // Кадры редкие и по событию: копить их в очереди незачем.
            desired_maximum_frame_latency: Some(1),
            wgpu_setup: WgpuSetup::CreateNew(setup),
            ..Default::default()
        };
        let size = window.inner_size();
        let mut painter = block_on(Painter::new(ctx.clone(), config, false, RendererOptions::default()));
        block_on(painter.set_window(ViewportId::ROOT, Some(window))).map_err(|e| e.to_string())?;
        // Сцену готовим сразу на новом устройстве: при уходе в трей она уйдёт вместе с ним.
        if let Some(rs) = painter.render_state() {
            if let Err(e) = super::scene::install_gpu(&rs) {
                eprintln!("[plasto] сцена не загрузилась: {e}");
            }
        }
        Ok(Gpu { painter, size: (size.width, size.height) })
    }

    pub fn max_texture_side(&self) -> Option<usize> {
        self.painter.max_texture_side()
    }

    /// Кадр в окно размером `size` в пикселях; поверхность перестраивается,
    /// только когда размер изменился.
    pub fn paint(&mut self, size: (u32, u32), pixels_per_point: f32, clear: [f32; 4], prims: &[egui::ClippedPrimitive], delta: &egui::TexturesDelta) {
        if size != self.size {
            if let (Some(w), Some(h)) = (NonZeroU32::new(size.0), NonZeroU32::new(size.1)) {
                self.painter.on_window_resized(ViewportId::ROOT, w, h);
                self.size = size;
            }
        }
        self.painter.paint_and_update_textures(ViewportId::ROOT, pixels_per_point, clear, prims, delta, Vec::new());
    }
}

/// wgpu на Windows отвечает сразу, без настоящего ожидания: исполнитель
/// с пустым будильником достаточен и не тянет за собой рантайм.
pub(super) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}
