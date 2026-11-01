//! Сцена окна: фото с зелёным экраном монитора и то, что экран показывает.
//!
//! Фото и экран собирает один шейдер (`scene.wgsl`) прямо по исходному снимку:
//! хромакей по зелени, плоскость экрана в перспективе по четырём углам, бочка
//! кинескопа, развёртка, объём из яркости зелени и свет экрана на корпусе.
//! Масок и подготовленных слоёв нет: новый фон — это новый снимок и его углы.
//!
//! Рисунки на экране (лица, «ЗАГРУЗКА») — фигуры egui, спроецированные на ту же
//! плоскость: текст и линии остаются чёткими на любом масштабе.
//!
//! Без видеокарты та же математика считается на CPU, только при смене
//! состояния: картинка собирается заново и выводится обычной текстурой.
//!
//! Живой фон (`video.rs`): вместо снимка шейдер читает кадры видео той же
//! сцены. Только на видеокарте; без неё — снимок.

use std::sync::Arc;

use std::time::{Duration, Instant};

use egui::{
    epaint::{ClippedShape, Primitive, QuadraticBezierShape},
    pos2, vec2, Color32, ColorImage, FontId, Pos2, Rect, Shape, Stroke, TextureHandle, TextureOptions, Ui, Vec2,
};

use super::{
    glass::{Layers, ScenePass},
    theme,
    video::{Frame, Player},
};

/// Снимок 1672×941 с зелёным экраном монитора.
const PHOTO: &[u8] = include_bytes!("../../assets/scene.jpg");
const SRC: [f32; 2] = [1672.0, 941.0];
/// Углы экрана на снимке, в пикселях: левый верхний, правый верхний, правый нижний, левый нижний.
const QUAD: [[f64; 2]; 4] = [[333.0, 332.0], [522.0, 341.0], [528.0, 490.0], [337.0, 490.0]];
/// Где искать зелень: экран и корпус вокруг. Зелёное за рамкой — это трава.
const ROI: [f32; 4] = [300.0, 290.0, 580.0, 540.0];
/// Плоскость экрана в своих единицах: рисунки экрана задаются в них.
const PLANE: [f32; 2] = [440.0, 352.0];

/// Что показывает экран монитора.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Screen {
    On,
    Off,
    Timeout,
    Error,
    Critical,
    /// Загрузка компонентов, доля 0..1.
    Boot(f32),
    /// Загрузка не удалась: GitHub не ответил. Покажет мастер первого запуска.
    #[allow(dead_code)]
    Fail,
}

impl Screen {
    pub fn from_state(state: &str) -> Self {
        match state {
            "on" => Screen::On,
            "timeout" => Screen::Timeout,
            "error" => Screen::Error,
            "critical" => Screen::Critical,
            _ => Screen::Off,
        }
    }

    /// Цвет свечения (центр, середина, край), свет на корпусе и сила блика.
    fn look(self) -> ([[f32; 3]; 3], [f32; 4], f32) {
        let rgb = |h: u32| [((h >> 16) & 255) as f32 / 255.0, ((h >> 8) & 255) as f32 / 255.0, (h & 255) as f32 / 255.0];
        match self {
            Screen::On => ([rgb(0xFFD27E), rgb(0xF29A33), rgb(0x9C4E12)], [1.0, 0.73, 0.38, 0.55], 1.0),
            Screen::Off => ([rgb(0x2E3532), rgb(0x171B19), rgb(0x0B0D0C)], [0.0, 0.0, 0.0, 0.0], 0.25),
            Screen::Timeout => ([rgb(0xB9D3EC), rgb(0x6F95BC), rgb(0x37526F)], [0.59, 0.71, 0.82, 0.45], 1.0),
            Screen::Error | Screen::Fail => ([rgb(0xFFA88F), rgb(0xE2563F), rgb(0x83281A)], [0.94, 0.47, 0.38, 0.5], 1.0),
            Screen::Critical => ([rgb(0xF06A5A), rgb(0xB02D24), rgb(0x5A100B)], [0.86, 0.24, 0.19, 0.55], 1.0),
            Screen::Boot(_) => ([rgb(0x2A2416), rgb(0x15120B), rgb(0x090805)], [1.0, 0.73, 0.38, 0.18], 0.25),
        }
    }
}

/* ---------- геометрия экрана ------------------------------------------- */

/// Однородная матрица, переводящая четыре точки `from` в `to` (по строкам).
fn homography(from: [[f64; 2]; 4], to: [[f64; 2]; 4]) -> [[f64; 3]; 3] {
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ([x, y], [u, v]) = (from[i], to[i]);
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let piv = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs())).unwrap_or(col);
        m.swap(col, piv);
        let pivot = m[col];
        if pivot[col] == 0.0 {
            continue;
        }
        for (r, row) in m.iter_mut().enumerate() {
            if r != col {
                let f = row[col] / pivot[col];
                for (x, p) in row.iter_mut().zip(pivot).skip(col) {
                    *x -= f * p;
                }
            }
        }
    }
    let h: Vec<f64> = (0..8).map(|i| m[i][8] / m[i][i]).collect();
    [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]]
}

fn apply(m: &[[f64; 3]; 3], x: f64, y: f64) -> (f64, f64) {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    ((m[0][0] * x + m[0][1] * y + m[0][2]) / w, (m[1][0] * x + m[1][1] * y + m[1][2]) / w)
}

const UNIT: [[f64; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

fn quad_uv() -> [[f64; 2]; 4] {
    QUAD.map(|[x, y]| [x / SRC[0] as f64, y / SRC[1] as f64])
}

/// Масштаб, с которым снимок закрывает прямоугольник сцены (по большей стороне,
/// прижат к левому верхнему углу: лишнее уходит вправо и вниз).
fn cover(rect: Rect) -> f32 {
    (rect.width() / SRC[0]).max(rect.height() / SRC[1])
}

/* ---------- параметры шейдера ------------------------------------------ */

fn uniforms(screen: Screen, rect: Rect, srgb_out: bool, video: bool) -> [f32; 40] {
    let scale = cover(rect);
    let (colors, glow, glare) = screen.look();
    let m = homography(quad_uv(), UNIT);
    let row = |r: usize| [m[r][0] as f32, m[r][1] as f32, m[r][2] as f32, 0.0];
    let mut u = [0.0f32; 40];
    u[0..4].copy_from_slice(&[rect.width() / (SRC[0] * scale), rect.height() / (SRC[1] * scale), PLANE[1] / 3.0, glare]);
    u[4..8].copy_from_slice(&[ROI[0] / SRC[0], ROI[1] / SRC[1], ROI[2] / SRC[0], ROI[3] / SRC[1]]);
    u[8..12].copy_from_slice(&row(0));
    u[12..16].copy_from_slice(&row(1));
    u[16..20].copy_from_slice(&row(2));
    for (i, c) in colors.iter().enumerate() {
        u[20 + 4 * i..23 + 4 * i].copy_from_slice(c);
    }
    u[32..36].copy_from_slice(&glow);
    u[36] = if srgb_out { 1.0 } else { 0.0 };
    u[37] = if video { 1.0 } else { 0.0 };
    u
}

/* ---------- видеокарта -------------------------------------------------- */

/// Готовит сцену на только что поднятом устройстве: снимок, конвейер сцены и
/// всё стекло поверх неё (`glass::Layers`). Уходит вместе с устройством.
pub fn install_gpu(rs: &egui_wgpu::RenderState) -> Result<(), String> {
    let photo = decode()?;
    let device = &rs.device;
    let size = wgpu::Extent3d { width: photo.width(), height: photo.height(), depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("plasto scene photo"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Не sRGB: шейдер считает в тех же значениях, что и макет.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let rgba = image::DynamicImage::ImageRgb8(photo).into_rgba8();
    rs.queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        rgba.as_raw(),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * size.width), rows_per_image: Some(size.height) },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let layout = scene_layout(device);
    let sampler = Layers::sampler(device);
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("plasto scene uniforms"),
        size: 40 * 4,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Пока кадров нет — плоскости видео 1×1: привязка нужна, читать нечего.
    let (luma, chroma) = planes(device, 1, 1);
    let video = VideoGpu { layout, photo: view, sampler: sampler.clone(), uniforms: uniforms.clone(), size: (0, 0), luma, chroma, seq: 0 };
    let bind_group = video.bind_group(device);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("plasto scene"), source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()) });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("plasto scene"), bind_group_layouts: &[Some(&video.layout)], immediate_size: 0 });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("plasto scene"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            // Сцена рисуется в текстуру за кадром; в окно её переносит glass.rs.
            targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let layers = Layers::new(rs, ScenePass { pipeline, bind_group, uniforms }, Layers::layout(device), sampler);
    let mut renderer = rs.renderer.write();
    renderer.callback_resources.insert(layers);
    renderer.callback_resources.insert(video);
    Ok(())
}

/// Раскладка сцены: параметры, снимок, сэмплер и две плоскости кадра видео.
fn scene_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let texture = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("plasto scene"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            texture(1),
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            texture(3),
            texture(4),
        ],
    })
}

/// Плоскости NV12 на видеокарте: яркость (R8) и цвет вдвое меньше (RG8).
fn planes(device: &wgpu::Device, width: u32, height: u32) -> (wgpu::Texture, wgpu::Texture) {
    let plane = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };
    (plane("plasto video luma", width, height, wgpu::TextureFormat::R8Unorm), plane("plasto video chroma", width / 2, height / 2, wgpu::TextureFormat::Rg8Unorm))
}

/// Кадры видео на видеокарте: плоскости под размер ролика и номер
/// загруженного кадра. Лежит в ресурсах рендерера рядом со стеклом.
struct VideoGpu {
    layout: wgpu::BindGroupLayout,
    photo: wgpu::TextureView,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    size: (u32, u32),
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    seq: u64,
}

impl VideoGpu {
    fn bind_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        let luma = self.luma.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma = self.chroma.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("plasto scene"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.photo) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&luma) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&chroma) },
            ],
        })
    }

    /// Залить кадр, если он новый. Возвращает новую привязку сцены, когда
    /// пришлось завести плоскости под другой размер.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) -> Option<wgpu::BindGroup> {
        let mut rebound = None;
        if self.size != (frame.width, frame.height) {
            (self.luma, self.chroma) = planes(device, frame.width, frame.height);
            self.size = (frame.width, frame.height);
            self.seq = 0;
            rebound = Some(self.bind_group(device));
        }
        if self.seq != frame.seq {
            let write = |texture: &wgpu::Texture, data: &[u8], w: u32, h: u32| {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(frame.width), rows_per_image: Some(h) },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            };
            write(&self.luma, &frame.y, frame.width, frame.height);
            write(&self.chroma, &frame.uv, frame.width / 2, frame.height / 2);
            self.seq = frame.seq;
        }
        rebound
    }
}

/// Сцену перерисовываем, только когда сменилось то, что показывает экран.
/// Проценты загрузки — фигуры поверх, на свечение они не влияют.
fn screen_key(screen: Screen) -> u64 {
    match screen {
        Screen::On => 1,
        Screen::Off => 2,
        Screen::Timeout => 3,
        Screen::Error => 4,
        Screen::Critical => 5,
        Screen::Boot(_) => 6,
        Screen::Fail => 7,
    }
}

struct SceneCallback {
    screen: Screen,
    rect: Rect,
    /// Кадр живого фона; нет — снимок.
    frame: Option<Arc<Frame>>,
}

impl egui_wgpu::CallbackTrait for SceneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Каждый новый кадр видео — новая сцена: ключ меняется с номером кадра.
        let mut key = screen_key(self.screen);
        let video = self.frame.as_ref().is_some_and(|frame| {
            let Some(gpu) = resources.get_mut::<VideoGpu>() else { return false };
            let rebound = gpu.upload(device, queue, frame);
            if let (Some(bind_group), Some(layers)) = (rebound, resources.get_mut::<Layers>()) {
                layers.set_scene_bind_group(bind_group);
            }
            key |= frame.seq << 8;
            true
        });
        if let Some(layers) = resources.get_mut::<Layers>() {
            let u = uniforms(self.screen, self.rect, false, video);
            layers.prepare_scene(device, queue, encoder, screen, key, &u);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(layers) = resources.get::<Layers>() {
            layers.paint_scene(pass);
        }
    }
}

/* ---------- CPU: та же картинка без видеокарты ------------------------- */

fn decode() -> Result<image::RgbImage, String> {
    image::load_from_memory_with_format(PHOTO, image::ImageFormat::Jpeg).map(|i| i.to_rgb8()).map_err(|e| format!("снимок сцены: {e}"))
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Собрать сцену на CPU — построчный перенос `scene.wgsl` (без развёртки на
/// краях не отличить). Считается раз на смену состояния, не каждый кадр.
fn compose(photo: &image::RgbImage, screen: Screen) -> ColorImage {
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let (colors, glow, glare_k) = screen.look();
    let m = homography(quad_uv(), UNIT);
    let roi = [ROI[0] as usize, ROI[1] as usize, ROI[2] as usize, ROI[3] as usize];
    let mut out = ColorImage::new([w, h], vec![Color32::BLACK; w * h]);
    for (i, px) in photo.pixels().enumerate() {
        let (x, y) = (i % w, i / w);
        let c = [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0];
        let inside = x >= roi[0] && x <= roi[2] && y >= roi[1] && y <= roi[3];
        let color = if !inside {
            c
        } else {
            let dom = c[1] - c[0].max(c[2]);
            let key = smoothstep(0.06, 0.20, dom);
            let (mut u, mut v) = apply(&m, x as f64 / w as f64, y as f64 / h as f64);
            let (qu, qv) = (u - 0.5, v - 0.5);
            let k = 1.0 + 0.06 * (qu * qu + qv * qv);
            u = 0.5 + qu * k;
            v = 0.5 + qv * k;
            let (u, v) = (u.clamp(0.0, 1.0) as f32, v.clamp(0.0, 1.0) as f32);
            let d = (((u - 0.5) * PLANE[0]).powi(2) + ((v - 0.45) * PLANE[1]).powi(2)).sqrt() / (0.62 * PLANE[0]);
            let mix = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
            let scr = if d < 0.6 { mix(colors[0], colors[1], d / 0.6) } else { mix(colors[1], colors[2], ((d - 0.6) / 0.4).clamp(0.0, 1.0)) };
            let scan = if (v * PLANE[1] / 3.0).fract() >= 0.5 { 1.0 } else { 0.87 };
            let shade = (c[1] / 0.86).clamp(0.0, 1.0).powf(1.3);
            let (gu, gv) = ((u - 0.24) * 3.2, (v - 0.2) * 4.4);
            let glare = (-(gu * gu + gv * gv)).exp() * 0.26 * glare_k;
            let screen_col = scr.map(|s| s * scan * shade + glare);
            let lum = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
            let greenish = ((c[1] - 0.9 * c[0]) / 0.12).clamp(0.0, 1.0) * smoothstep(0.43, 0.67, lum);
            let spill = greenish.max((dom - 0.015) / 0.16).clamp(0.0, 1.0) * (1.0 - key);
            let beige = [1.0, 0.94, 0.8];
            let bl = 0.3 * beige[0] + 0.59 * beige[1] + 0.11 * beige[2];
            let a = (spill * 1.4).clamp(0.0, 1.0);
            let base: [f32; 3] = std::array::from_fn(|j| {
                let b = c[j] + (beige[j] * lum / bl - c[j]) * a;
                b + glow[j] * glow[3] * spill * (1.0 - b)
            });
            mix(base, screen_col, key)
        };
        out.pixels[i] = Color32::from_rgb((color[0].clamp(0.0, 1.0) * 255.0) as u8, (color[1].clamp(0.0, 1.0) * 255.0) as u8, (color[2].clamp(0.0, 1.0) * 255.0) as u8);
    }
    out
}

/* ---------- сцена ------------------------------------------------------- */

/// Состояние сцены между кадрами. На видеокарте почти пустое; без неё держит
/// собранную картинку, пока окно на экране.
#[derive(Default)]
pub struct Scene {
    cpu: Option<(Screen, TextureHandle)>,
    life: Life,
    /// Живой фон: есть, пока окно на экране и фон — видео.
    player: Option<Player>,
}

/* ---------- живое лицо -------------------------------------------------- */

/// Смена состояния — как у кинескопа: старая картинка сжимается в полоску,
/// новая из полоски разворачивается.
const COLLAPSE: f32 = 0.16;
const EXPAND: f32 = 0.22;
/// Моргание: длительность и пауза между морганиями.
const BLINK: f32 = 0.14;
const BLINK_GAP: (f32, f32) = (2.5, 6.5);
/// На сколько единиц плоскости лицо смещается к курсору.
const LOOK: f32 = 7.0;

/// То, что меняется со временем: смена картинки, моргание, взгляд.
/// Кадры просятся только на время движения; между морганиями окно стоит.
#[derive(Default)]
struct Life {
    screen: Option<Screen>,
    from: Option<Screen>,
    since: Option<Instant>,
    blink_at: Option<Instant>,
    wink: bool,
    look: Vec2,
    seed: u64,
}

/// Что рисовать в этом кадре.
#[derive(Clone, Copy, Default)]
struct Pose {
    /// Сжатие по вертикали: 1 — обычное лицо, 0 — полоска.
    squash: f32,
    /// Закрытость глаз 0..1; `wink` — закрывается только правый.
    blink: f32,
    wink: bool,
    /// Смещение к курсору в единицах плоскости.
    look: Vec2,
    /// Секунды для бегущих точек «ожидания».
    t: f32,
}

/// Одно и то же ли состояние: проценты загрузки — не смена картинки.
fn same(a: Screen, b: Screen) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x) * (1.0 - x)
}

impl Life {
    /// Псевдослучайное 0..1 для пауз между морганиями.
    fn rand(&mut self) -> f32 {
        if self.seed == 0 {
            self.seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        }
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 40) as f32 / (1u64 << 24) as f32
    }

    fn next_blink(&mut self, now: Instant) {
        let gap = BLINK_GAP.0 + (BLINK_GAP.1 - BLINK_GAP.0) * self.rand();
        self.blink_at = Some(now + Duration::from_secs_f32(gap));
        self.wink = self.rand() < 0.15;
    }

    /// Какой экран показать сейчас, в какой позе, и когда нужен следующий кадр.
    fn step(&mut self, screen: Screen, now: Instant, target: Vec2, dt: f32, alive: bool) -> (Screen, Pose, Option<Duration>) {
        if !self.screen.is_some_and(|s| same(s, screen)) {
            // Первый кадр — только разворот, без сжатия прежней картинки.
            self.from = self.screen;
            self.since = Some(now);
        }
        self.screen = Some(screen);
        if !alive {
            self.since = None;
            self.look = Vec2::ZERO;
            return (screen, Pose { squash: 1.0, ..Pose::default() }, None);
        }
        let mut wake: Option<Duration> = None;
        let mut soon = |d: Duration| wake = Some(wake.map_or(d, |w| w.min(d)));
        let frame = Duration::from_millis(16);

        // Смена картинки.
        let mut shown = screen;
        let mut squash = 1.0;
        if let Some(since) = self.since {
            let mut t = now.duration_since(since).as_secs_f32();
            if let Some(from) = self.from {
                if t < COLLAPSE {
                    shown = from;
                    squash = 1.0 - ease(t / COLLAPSE);
                }
                t -= COLLAPSE;
            }
            if t >= 0.0 {
                squash = ease(t / EXPAND);
            }
            if t < EXPAND {
                soon(frame);
            } else {
                self.since = None;
                self.from = None;
            }
        }

        // Моргание: только у лиц с открытыми глазами.
        let mut blink = 0.0;
        if matches!(shown, Screen::On | Screen::Timeout) {
            let at = match self.blink_at {
                Some(at) => at,
                None => {
                    self.next_blink(now);
                    self.blink_at.unwrap_or(now)
                }
            };
            if now >= at {
                let t = now.duration_since(at).as_secs_f32();
                if t < BLINK {
                    blink = (std::f32::consts::PI * t / BLINK).sin();
                    soon(frame);
                } else {
                    self.next_blink(now);
                }
            }
            if let Some(at) = self.blink_at.filter(|at| *at > now) {
                soon(at - now);
            }
        } else {
            self.blink_at = None;
        }

        // Взгляд догоняет курсор плавно; ушёл курсор — лицо вернётся в центр.
        self.look += (target - self.look) * (dt * 10.0).min(1.0);
        if (target - self.look).length() > 0.05 {
            soon(frame);
        } else {
            self.look = target;
        }

        (shown, Pose { squash, blink, wink: self.wink, look: self.look, t: 0.0 }, wake)
    }
}

impl Scene {
    /// Окно ушло в трей: собранную картинку не держим.
    pub fn release(&mut self) {
        self.cpu = None;
        self.player = None;
    }

    /// `video` — фон из настроек: видео или снимок. Видео идёт только на
    /// видеокарте и с включёнными эффектами анимации Windows; без фокуса окна
    /// стоит на последнем кадре.
    pub fn paint(&mut self, ui: &Ui, rect: Rect, screen: Screen, gpu: bool, video: bool) {
        let now = Instant::now();
        let alive = crate::platform::animations_enabled();
        if video && gpu && alive {
            let player = self.player.get_or_insert_with(|| Player::start(ui.ctx().clone()));
            player.set_playing(ui.ctx().input(|i| i.viewport().focused.unwrap_or(true)));
        } else {
            self.player = None;
        }
        let frame = self.player.as_ref().and_then(Player::frame);
        let (to_quad, scale, _) = projection(rect);
        // Курсор относительно середины экрана монитора: куда смотреть.
        let (cx, cy) = apply(&to_quad, 0.5, 0.5);
        let center = rect.min + vec2(cx as f32, cy as f32) * scale;
        let target = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)).map_or(Vec2::ZERO, |p| {
            let d = p - center;
            d.normalized() * (d.length() / 300.0).min(1.0) * LOOK
        });
        let dt = ui.ctx().input(|i| i.stable_dt).min(0.1);
        let (shown, mut pose, wake) = self.life.step(screen, now, target, dt, alive);
        // Бегущие точки «ожидания» — пока оно на экране; без видеокарты реже.
        let mut wake = wake;
        if alive && shown == Screen::Timeout {
            pose.t = ui.ctx().input(|i| i.time) as f32;
            let tick = Duration::from_millis(if gpu { 33 } else { 100 });
            wake = Some(wake.map_or(tick, |w| w.min(tick)));
        }
        if let Some(wake) = wake {
            ui.ctx().request_repaint_after(wake);
        }

        let p = ui.painter_at(rect);
        if gpu {
            p.add(egui_wgpu::Callback::new_paint_callback(rect, SceneCallback { screen: shown, rect, frame }));
        } else {
            // Загрузка меняет только проценты на экране: свечение одно и то же.
            let key = if let Screen::Boot(_) = shown { Screen::Boot(0.0) } else { shown };
            if self.cpu.as_ref().is_none_or(|(s, _)| *s != key) {
                if let Ok(photo) = decode() {
                    let tex = ui.ctx().load_texture("plasto-scene", compose(&photo, key), TextureOptions::LINEAR);
                    self.cpu = Some((key, tex));
                }
            }
            if let Some((_, tex)) = &self.cpu {
                let size = vec2(SRC[0], SRC[1]) * cover(rect);
                p.image(tex.id(), Rect::from_min_size(rect.min, size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        paint_content(ui, rect, shown, pose);
    }
}

/* ---------- рисунки на экране ------------------------------------------ */

/// Плоскость экрана → снимок, масштаб снимка в окне и сколько точек окна
/// приходится на единицу плоскости (для толщины линий и шрифтов).
fn projection(rect: Rect) -> ([[f64; 3]; 3], f32, f32) {
    let scale = cover(rect);
    let to_quad = homography(UNIT, QUAD);
    let (x0, _) = apply(&to_quad, 0.0, 0.5);
    let (x1, _) = apply(&to_quad, 1.0, 0.5);
    (to_quad, scale, ((x1 - x0) as f32 * scale) / PLANE[0])
}

/// Рисует лицо или надпись на плоскости экрана: фигуры строятся в плоской
/// системе (в точках, как будто экран смотрит прямо на нас), затем каждая
/// вершина переносится на снимок однородной матрицей. Поза сжимает рисунок к
/// средней линии экрана и сдвигает его к курсору.
fn paint_content(ui: &Ui, rect: Rect, screen: Screen, pose: Pose) {
    let (to_quad, scale, k) = projection(rect);
    let plane = Rect::from_min_size(Pos2::ZERO, vec2(PLANE[0], PLANE[1]) * k);
    let shapes = content_shapes(ui, screen, k, plane, pose);
    if shapes.is_empty() {
        return;
    }
    let ctx = ui.ctx();
    let clipped: Vec<ClippedShape> = shapes.into_iter().map(|shape| ClippedShape { clip_rect: Rect::EVERYTHING, shape }).collect();
    let mid = plane.center().y;
    // Полоска на месте сжатого рисунка не тоньше пары единиц: кинескоп гаснет в линию.
    let squash = pose.squash.max(0.02);
    let to_window = |p: Pos2| {
        let p = pos2(p.x, mid + (p.y - mid) * squash) + pose.look * k;
        let (sx, sy) = apply(&to_quad, (p.x / plane.width()) as f64, (p.y / plane.height()) as f64);
        rect.min + vec2(sx as f32, sy as f32) * scale
    };
    let painter = ui.painter_at(rect);
    for prim in ctx.tessellate(clipped, ctx.pixels_per_point()) {
        if let Primitive::Mesh(mut mesh) = prim.primitive {
            for v in &mut mesh.vertices {
                v.pos = to_window(v.pos);
            }
            painter.add(Shape::mesh(Arc::new(mesh)));
        }
    }
}

fn content_shapes(ui: &Ui, screen: Screen, k: f32, plane: Rect, pose: Pose) -> Vec<Shape> {
    let hex = |h: u32| Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    let mut shapes = Vec::new();
    // Лица — в координатах 120×90, как в макете, вписанные в середину экрана.
    let (s, ox, oy) = (2.0 * k, (PLANE[0] - 240.0) / 2.0 * k, (PLANE[1] - 180.0) / 2.0 * k);
    let at = |x: f32, y: f32| pos2(ox + x * s, oy + y * s);
    let width = 7.0 * s;
    let line = |pts: &[(f32, f32)], ink: Color32, out: &mut Vec<Shape>| {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| at(x, y)).collect();
        for &p in &pts {
            out.push(Shape::circle_filled(p, width / 2.0, ink));
        }
        out.push(Shape::line(pts, Stroke::new(width, ink)));
    };
    // Глаз при моргании сжимается к своей середине (`cy`); при подмигивании — только правый.
    let shut = |right: bool| if pose.wink && !right { 0.0 } else { pose.blink * 0.85 };
    let eye = |pts: &[(f32, f32)], cy: f32, right: bool| -> Vec<(f32, f32)> { pts.iter().map(|&(x, y)| (x, cy + (y - cy) * (1.0 - shut(right)))).collect() };
    let curve = |a: (f32, f32), c: (f32, f32), b: (f32, f32), ink: Color32, out: &mut Vec<Shape>| {
        out.push(Shape::QuadraticBezier(QuadraticBezierShape::from_points_stroke([at(a.0, a.1), at(c.0, c.1), at(b.0, b.1)], false, Color32::TRANSPARENT, Stroke::new(width, ink))));
        out.push(Shape::circle_filled(at(a.0, a.1), width / 2.0, ink));
        out.push(Shape::circle_filled(at(b.0, b.1), width / 2.0, ink));
    };
    match screen {
        Screen::On => {
            let ink = hex(0x5A3A12);
            line(&eye(&[(22.0, 34.0), (32.0, 24.0), (42.0, 34.0)], 31.0, false), ink, &mut shapes);
            line(&eye(&[(78.0, 34.0), (88.0, 24.0), (98.0, 34.0)], 31.0, true), ink, &mut shapes);
            curve((38.0, 58.0), (60.0, 76.0), (82.0, 58.0), ink, &mut shapes);
        }
        Screen::Off => {
            let ink = hex(0x56605B);
            line(&[(22.0, 34.0), (42.0, 34.0)], ink, &mut shapes);
            line(&[(78.0, 34.0), (98.0, 34.0)], ink, &mut shapes);
            line(&[(50.0, 62.0), (70.0, 62.0)], ink, &mut shapes);
        }
        Screen::Timeout => {
            let ink = hex(0x2E3F52);
            for (x, right) in [(32.0, false), (88.0, true)] {
                shapes.push(Shape::ellipse_filled(at(x, 32.0), vec2(7.0, 7.0 * (1.0 - shut(right))) * s, ink));
            }
            // Рот — три точки, подпрыгивают по очереди, как «печатает…».
            for (i, x) in [46.0, 60.0, 74.0].into_iter().enumerate() {
                let hop = (std::f32::consts::TAU * (pose.t / 1.2 - i as f32 * 0.15)).sin().max(0.0);
                shapes.push(Shape::circle_filled(at(x, 64.0 - 5.0 * hop), 4.5 * s, ink));
            }
        }
        Screen::Error => {
            let ink = hex(0x5C1A10);
            line(&[(24.0, 24.0), (40.0, 40.0)], ink, &mut shapes);
            line(&[(40.0, 24.0), (24.0, 40.0)], ink, &mut shapes);
            line(&[(80.0, 24.0), (96.0, 40.0)], ink, &mut shapes);
            line(&[(96.0, 24.0), (80.0, 40.0)], ink, &mut shapes);
            curve((40.0, 70.0), (60.0, 54.0), (80.0, 70.0), ink, &mut shapes);
        }
        Screen::Critical => {
            let ink = hex(0xFFD9D2);
            shapes.push(Shape::rect_filled(Rect::from_min_size(at(52.0, -6.0), vec2(16.0, 62.0) * s), 8.0 * s, ink));
            shapes.push(Shape::circle_filled(at(60.0, 78.0), 9.0 * s, ink));
        }
        Screen::Boot(frac) => {
            let amber = hex(0xFFC266);
            let text = |t: &str, size: f32, y: f32, out: &mut Vec<Shape>| {
                let g = ui.painter().layout_no_wrap(t.to_owned(), FontId::new(size * k, theme::semibold(10.0).family), amber);
                out.push(Shape::galley(pos2(plane.center().x - g.size().x / 2.0, y * k - g.size().y / 2.0), g, amber));
            };
            text("ЗАГРУЗКА", 34.0, PLANE[1] * 0.34, &mut shapes);
            let (bw, bh) = (260.0 * k, 26.0 * k);
            let bar = Rect::from_min_size(pos2(plane.center().x - bw / 2.0, PLANE[1] * 0.47 * k), vec2(bw, bh));
            shapes.push(Shape::rect_stroke(bar, 0.0, Stroke::new(4.0 * k, amber), egui::StrokeKind::Inside));
            let inner = bar.shrink(6.0 * k);
            shapes.push(Shape::rect_filled(Rect::from_min_size(inner.min, vec2(inner.width() * frac.clamp(0.0, 1.0), inner.height())), 0.0, amber));
            text(&format!("{}%", (frac.clamp(0.0, 1.0) * 100.0).round()), 40.0, PLANE[1] * 0.74, &mut shapes);
        }
        Screen::Fail => {
            let ink = hex(0x5C1A10);
            let g = ui.painter().layout_no_wrap("НЕТ СВЯЗИ".to_owned(), FontId::new(34.0 * k, theme::semibold(10.0).family), ink);
            shapes.push(Shape::galley(pos2(plane.center().x - g.size().x / 2.0, PLANE[1] * 0.36 * k - g.size().y / 2.0), g, ink));
            let g = ui.painter().layout_no_wrap("с GitHub".to_owned(), FontId::new(26.0 * k, theme::regular(10.0).family), ink);
            shapes.push(Shape::galley(pos2(plane.center().x - g.size().x / 2.0, PLANE[1] * 0.56 * k - g.size().y / 2.0), g, ink));
        }
    }
    shapes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn углы_экрана_переходят_в_углы_плоскости_и_обратно() {
        let to_plane = homography(quad_uv(), UNIT);
        let to_quad = homography(UNIT, QUAD);
        for (q, unit) in quad_uv().iter().zip(UNIT) {
            let (u, v) = apply(&to_plane, q[0], q[1]);
            assert!((u - unit[0]).abs() < 1e-6 && (v - unit[1]).abs() < 1e-6, "{u} {v}");
        }
        for (corner, unit) in QUAD.iter().zip(UNIT) {
            let (x, y) = apply(&to_quad, unit[0], unit[1]);
            assert!((x - corner[0]).abs() < 1e-6 && (y - corner[1]).abs() < 1e-6, "{x} {y}");
        }
    }

    #[test]
    fn снимок_читается_и_экран_зелёный() {
        let photo = decode().unwrap();
        assert_eq!((photo.width(), photo.height()), (SRC[0] as u32, SRC[1] as u32));
        // Середина экрана — чистая зелень, иначе хромакей не сработает.
        let (cx, cy) = (QUAD.iter().map(|q| q[0]).sum::<f64>() / 4.0, QUAD.iter().map(|q| q[1]).sum::<f64>() / 4.0);
        let px = photo.get_pixel(cx as u32, cy as u32);
        assert!(px[1] as i32 - (px[0].max(px[2]) as i32) > 60, "{px:?}");
    }

    #[test]
    fn без_видеокарты_экран_перекрашен_а_трава_нет() {
        let photo = decode().unwrap();
        let img = compose(&photo, Screen::Error);
        let at = |x: usize, y: usize| img.pixels[y * photo.width() as usize + x];
        // Центр экрана — красный, зелени не осталось.
        let c = at(430, 410);
        assert!(c.r() > c.g() + 40, "{c:?}");
        // Трава вне рамки поиска зелени не тронута.
        let g = at(100, 800);
        let o = photo.get_pixel(100, 800);
        assert_eq!((g.r(), g.g(), g.b()), (o[0], o[1], o[2]));
    }

    #[test]
    fn смена_экрана_сжимает_старый_и_разворачивает_новый() {
        let mut life = Life::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Первый кадр: только разворот из полоски.
        let (shown, pose, wake) = life.step(Screen::Off, t0, Vec2::ZERO, 0.016, true);
        assert_eq!(shown, Screen::Off);
        assert!(pose.squash < 0.05 && wake.is_some());
        let (_, pose, wake) = life.step(Screen::Off, at(300), Vec2::ZERO, 0.016, true);
        assert_eq!(pose.squash, 1.0);
        assert!(wake.is_none(), "выключенное лицо не моргает и кадров не просит");
        // Включили: сначала сжимается прежняя картинка, потом разворачивается новая.
        let (shown, _, _) = life.step(Screen::On, at(400), Vec2::ZERO, 0.016, true);
        assert_eq!(shown, Screen::Off);
        let (shown, pose, _) = life.step(Screen::On, at(400 + 150), Vec2::ZERO, 0.016, true);
        assert_eq!(shown, Screen::Off);
        assert!(pose.squash < 0.2);
        let (shown, pose, _) = life.step(Screen::On, at(400 + 500), Vec2::ZERO, 0.016, true);
        assert_eq!(shown, Screen::On);
        assert_eq!(pose.squash, 1.0);
        // Проценты загрузки — не смена картинки.
        let mut boot = Life::default();
        boot.step(Screen::Boot(0.1), t0, Vec2::ZERO, 0.016, true);
        let (shown, _, _) = boot.step(Screen::Boot(0.5), at(1000), Vec2::ZERO, 0.016, true);
        assert_eq!(shown, Screen::Boot(0.5));
    }

    #[test]
    fn моргает_в_срок_и_ждёт_до_следующего() {
        let mut life = Life::default();
        let t0 = Instant::now();
        life.step(Screen::On, t0, Vec2::ZERO, 0.016, true);
        let at = life.blink_at.expect("моргание назначено");
        let gap = at.duration_since(t0).as_secs_f32();
        assert!((BLINK_GAP.0..=BLINK_GAP.1).contains(&gap), "{gap}");
        let after = at + Duration::from_millis(70);
        let (_, pose, wake) = life.step(Screen::On, after, Vec2::ZERO, 0.016, true);
        assert!(pose.blink > 0.9);
        assert_eq!(wake, Some(Duration::from_millis(16)));
        let (_, pose, wake) = life.step(Screen::On, at + Duration::from_millis(200), Vec2::ZERO, 0.016, true);
        assert_eq!(pose.blink, 0.0);
        assert!(wake.is_some_and(|w| w.as_secs_f32() >= BLINK_GAP.0 - 0.3), "{wake:?}");
    }

    #[test]
    fn без_анимаций_лицо_стоит() {
        let mut life = Life::default();
        let t0 = Instant::now();
        let (shown, pose, wake) = life.step(Screen::On, t0, vec2(5.0, 0.0), 0.016, false);
        assert_eq!(shown, Screen::On);
        assert_eq!((pose.squash, pose.blink, pose.look), (1.0, 0.0, Vec2::ZERO));
        assert!(wake.is_none());
    }

    /// Видеокарта, без окна: сцена собирается из снимка и из кадра видео без
    /// ошибок проверки wgpu, и кадр видео действительно попадает в картинку.
    /// `cargo test --lib -- --ignored сцена_на_видеокарте --nocapture`.
    #[test]
    #[ignore]
    fn сцена_на_видеокарте_берёт_кадр_видео() {
        use egui_wgpu::CallbackTrait;
        let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(desc);
        let config = egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() };
        let rs = super::super::gpu::block_on(egui_wgpu::RenderState::create(&config, &instance, None, egui_wgpu::RendererOptions::default())).expect("видеокарта");
        let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&errors);
        rs.device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| sink.lock().unwrap().push(e.to_string())));
        install_gpu(&rs).unwrap();

        let player = Player::start(egui::Context::default());
        player.set_playing(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut frames: Vec<Arc<Frame>> = Vec::new();
        while frames.len() < 2 {
            if let Some(f) = player.frame().filter(|f| frames.last().is_none_or(|l| f.seq >= l.seq + 6)) {
                frames.push(f);
            }
            assert!(Instant::now() < deadline, "нет кадров");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(player);
        let frame = Arc::clone(&frames[0]);

        let (w, h) = (640u32, 360u32);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32));
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: 1.0 };
        let target = rs.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: rs.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = rs.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (w * 4 * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let render = |frame: Option<Arc<Frame>>| -> Vec<u8> {
            let cb = SceneCallback { screen: Screen::On, rect, frame };
            let mut encoder = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut renderer = rs.renderer.write();
                cb.prepare(&rs.device, &rs.queue, &screen, &mut encoder, &mut renderer.callback_resources);
                let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                let mut pass = pass.forget_lifetime();
                let info = egui::PaintCallbackInfo { viewport: rect, clip_rect: rect, pixels_per_point: 1.0, screen_size_px: [w, h] };
                cb.paint(info, &mut pass, &renderer.callback_resources);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) } },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            rs.queue.submit([encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let data = readback.slice(..).get_mapped_range().to_vec();
            readback.unmap();
            data
        };
        let photo = render(None);
        let video = render(Some(frame));
        let later = render(Some(Arc::clone(&frames[1])));
        // Корпус вокруг кинескопа (без самого экрана): между кадрами видео он
        // меняться почти не должен — иначе маска «дышит».
        let ring = |a: &[u8], b: &[u8]| -> f64 {
            let (mut sum, mut n) = (0u64, 0u64);
            for y in 110..202u32 {
                for x in 105..225u32 {
                    if (128..202).contains(&x) && (128..186).contains(&y) {
                        continue;
                    }
                    let i = ((y * w + x) * 4) as usize;
                    sum += (0..3).map(|k| (a[i + k] as i32 - b[i + k] as i32).unsigned_abs() as u64).sum::<u64>();
                    n += 3;
                }
            }
            sum as f64 / n as f64
        };
        println!("корпус между кадрами видео: {:.2}", ring(&video, &later));
        let later = render(Some(Arc::clone(&frames[1])));
        // Корпус вокруг кинескопа (без самого экрана): между кадрами видео он
        // меняться почти не должен — иначе маска «дышит».
        let ring = |a: &[u8], b: &[u8]| -> f64 {
            let (mut sum, mut n) = (0u64, 0u64);
            for y in 110..202u32 {
                for x in 105..225u32 {
                    if (128..202).contains(&x) && (128..186).contains(&y) {
                        continue;
                    }
                    let i = ((y * w + x) * 4) as usize;
                    sum += (0..3).map(|k| (a[i + k] as i32 - b[i + k] as i32).unsigned_abs() as u64).sum::<u64>();
                    n += 3;
                }
            }
            sum as f64 / n as f64
        };
        println!("корпус между кадрами видео: {:.2}", ring(&video, &later));
        // Посмотреть глазами: PLASTO_SCENE_DUMP=<папка> — снимок и кадр видео в PNG.
        if let Ok(dir) = std::env::var("PLASTO_SCENE_DUMP") {
            let bgra = matches!(rs.target_format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
            for (name, img) in [("photo", &photo), ("video", &video), ("later", &later)] {
                let rgb: Vec<u8> = img.chunks(4).flat_map(|p| if bgra { [p[2], p[1], p[0]] } else { [p[0], p[1], p[2]] }).collect();
                image::RgbImage::from_raw(w, h, rgb).unwrap().save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
            }
        }
        assert!(errors.lock().unwrap().is_empty(), "{:?}", errors.lock().unwrap());
        // Небо над монитором: на снимке и в кадре видео облака разные.
        let sky = |img: &[u8]| -> Vec<u8> { (40..120).flat_map(|y| (300..600).flat_map(move |x| { let i = ((y * w + x) * 4) as usize; [i, i + 1, i + 2] })).map(|i| img[i]).collect() };
        let (a, b) = (sky(&photo), sky(&video));
        let diff = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum::<u64>() as f64 / a.len() as f64;
        println!("разница неба, снимок против видео: {diff:.1}");
        assert!(a.iter().any(|v| *v > 40), "сцена пустая");
        assert!(diff > 2.0);
    }
}
