//! Стекло: панели с настоящим размытием сцены под ними, как `backdrop-filter`
//! в макете.
//!
//! Сцена рисуется в текстуру за кадром — только когда сменились состояние или
//! размер окна — и из неё сразу строится пирамида: ½, ¼, ⅛. Две ступени
//! размываются по Гауссу в два прохода: слабо — для затемнения за внутренними
//! страницами (6 точек), сильно — для стеклянных панелей (24 точки). На
//! маленьких копиях это почти бесплатно. Каждый кадр сцена лишь копируется в
//! окно, а панели берут из размытой копии то, что лежит под ними.
//!
//! Без видеокарты стекло — полупрозрачная заливка с тем же тоном.

use std::collections::HashMap;

use egui::{pos2, Color32, Rect, Shadow, Stroke, Ui};

/// Тон стекла сверху-слева и снизу-справа (sRGB, доля) — из макета.
const GLASS_TINT: [[f32; 4]; 2] = [[82.0 / 255.0, 80.0 / 255.0, 78.0 / 255.0, 0.42], [26.0 / 255.0, 29.0 / 255.0, 29.0 / 255.0, 0.60]];
/// Затемнение за внутренними страницами.
const VEIL_TINT: [f32; 4] = [14.0 / 255.0, 16.0 / 255.0, 18.0 / 255.0, 0.24];
/// Размытие в точках: стекло и затемнение (как `blur(24px)` и `blur(6px)`).
const GLASS_BLUR: f32 = 24.0;
const VEIL_BLUR: f32 = 6.0;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Чем рисует сама сцена: её конвейер и параметры готовит `scene.rs`.
pub struct ScenePass {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    pub uniforms: wgpu::Buffer,
}

struct Tex {
    view: wgpu::TextureView,
}

/// Проход по пирамиде: куда рисуем, чем и с какими параметрами.
struct Pass {
    target: usize,
    blur: bool,
    bind_group: wgpu::BindGroup,
}

/// Текстуры одного размера окна; при смене размера или масштаба строятся заново.
struct Targets {
    size: [u32; 2],
    ppp: f32,
    /// Состояние, по которому сцена нарисована сейчас.
    drawn: Option<u64>,
    tex: Vec<Tex>,
    passes: Vec<Pass>,
    blit: wgpu::BindGroup,
    generation: u64,
}

// Номера текстур в `Targets::tex`.
const FULL: usize = 0;
const VEIL: usize = 5;
const GLASS: usize = 7;

struct Slot {
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    generation: u64,
    veil: bool,
}

/// Всё, что держит видеокарта для сцены и стекла. Лежит в ресурсах рендерера
/// egui и уходит вместе с устройством, когда окно прячется в трей.
pub struct Layers {
    scene: ScenePass,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    copy: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    blit_params: wgpu::Buffer,
    srgb: bool,
    targets: Option<Targets>,
    slots: HashMap<u64, Slot>,
    generation: u64,
}

fn params(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn bytes(v: &[f32; 16]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

impl Layers {
    /// Общая раскладка: параметры, текстура, сэмплер — у сцены и у всех проходов стекла.
    pub fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("plasto layers"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        })
    }

    pub fn sampler(device: &wgpu::Device) -> wgpu::Sampler {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("plasto linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        })
    }

    pub fn new(rs: &egui_wgpu::RenderState, scene: ScenePass, layout: wgpu::BindGroupLayout, sampler: wgpu::Sampler) -> Self {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("plasto glass"), source: wgpu::ShaderSource::Wgsl(include_str!("glass.wgsl").into()) });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("plasto glass"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = |entry: &str, format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let layers = Layers {
            copy: pipeline("fs_copy", FORMAT, None),
            blur: pipeline("fs_blur", FORMAT, None),
            blit: pipeline("fs_blit", rs.target_format, None),
            glass: pipeline("fs_glass", rs.target_format, Some(wgpu::BlendState::ALPHA_BLENDING)),
            blit_params: params(device, "plasto blit"),
            scene,
            layout,
            sampler,
            srgb: rs.target_format.is_srgb(),
            targets: None,
            slots: HashMap::new(),
            generation: 0,
        };
        let mut p = [0.0f32; 16];
        p[0] = if layers.srgb { 1.0 } else { 0.0 };
        rs.queue.write_buffer(&layers.blit_params, 0, &bytes(&p));
        layers
    }

    fn bind(&self, device: &wgpu::Device, buffer: &wgpu::Buffer, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("plasto layer"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Текстуры и проходы пирамиды под размер окна в пикселях.
    fn build(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 2], ppp: f32) {
        let dim = |d: u32| [(size[0] / d).max(1), (size[1] / d).max(1)];
        // full, ½, ¼, ⅛; затемнение — на ¼ (черновик и итог), стекло — на ⅛.
        let sizes = [dim(1), dim(2), dim(4), dim(8), dim(4), dim(4), dim(8), dim(8)];
        let tex: Vec<Tex> = sizes
            .iter()
            .map(|&[w, h]| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("plasto layer"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                Tex { view: t.create_view(&wgpu::TextureViewDescriptor::default()) }
            })
            .collect();
        let mut passes = Vec::new();
        let mut step = |from: usize, to: usize, blur: Option<(f32, f32, f32)>| {
            let buffer = params(device, "plasto pass");
            let mut p = [0.0f32; 16];
            if let Some((dx, dy, sigma)) = blur {
                let [w, h] = sizes[from];
                p[0] = dx / w as f32;
                p[1] = dy / h as f32;
                p[2] = sigma;
            }
            queue.write_buffer(&buffer, 0, &bytes(&p));
            passes.push(Pass { target: to, blur: blur.is_some(), bind_group: self.bind(device, &buffer, &tex[from].view) });
        };
        // Сигма в текселях ступени: размытие задано в точках, ступень мельче окна.
        let veil = VEIL_BLUR * ppp / 4.0;
        let glass = GLASS_BLUR * ppp / 8.0;
        step(0, 1, None);
        step(1, 2, None);
        step(2, 3, None);
        step(2, 4, Some((1.0, 0.0, veil)));
        step(4, VEIL, Some((0.0, 1.0, veil)));
        step(3, 6, Some((1.0, 0.0, glass)));
        step(6, GLASS, Some((0.0, 1.0, glass)));
        let blit = self.bind(device, &self.blit_params, &tex[FULL].view);
        self.generation += 1;
        self.targets = Some(Targets { size, ppp, drawn: None, tex, passes, blit, generation: self.generation });
    }

    /// Перед кадром: если сменились размер окна или то, что показывает экран, —
    /// нарисовать сцену заново и перестроить размытые копии.
    pub fn prepare_scene(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, screen: &egui_wgpu::ScreenDescriptor, key: u64, uniforms: &[f32]) {
        let (size, ppp) = (screen.size_in_pixels, screen.pixels_per_point);
        if size[0] == 0 || size[1] == 0 {
            return;
        }
        if self.targets.as_ref().is_none_or(|t| t.size != size || t.ppp != ppp) {
            self.build(device, queue, size, ppp);
        }
        let Some(targets) = self.targets.as_mut() else { return };
        if targets.drawn == Some(key) {
            return;
        }
        targets.drawn = Some(key);
        let raw: Vec<u8> = uniforms.iter().flat_map(|f| f.to_le_bytes()).collect();
        queue.write_buffer(&self.scene.uniforms, 0, &raw);
        let run = |encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline, bind: &wgpu::BindGroup| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("plasto layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        };
        run(encoder, &targets.tex[FULL].view, &self.scene.pipeline, &self.scene.bind_group);
        for pass in &targets.passes {
            run(encoder, &targets.tex[pass.target].view, if pass.blur { &self.blur } else { &self.copy }, &pass.bind_group);
        }
    }

    /// Сцена теперь читает другие текстуры (пришли кадры видео другого
    /// размера): новая привязка, и при следующем кадре сцена рисуется заново.
    pub fn set_scene_bind_group(&mut self, bind_group: wgpu::BindGroup) {
        self.scene.bind_group = bind_group;
        if let Some(t) = self.targets.as_mut() {
            t.drawn = None;
        }
    }

    /// Готовая сцена — в окно.
    pub fn paint_scene(&self, pass: &mut wgpu::RenderPass<'static>) {
        if let Some(t) = &self.targets {
            pass.set_pipeline(&self.blit);
            pass.set_bind_group(0, &t.blit, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn prepare_glass(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u64, veil: bool, values: [f32; 16]) {
        let Some(targets) = &self.targets else { return };
        let generation = targets.generation;
        let stale = self.slots.get(&id).is_none_or(|s| s.generation != generation || s.veil != veil);
        if stale {
            let uniforms = params(device, "plasto glass");
            let view = &targets.tex[if veil { VEIL } else { GLASS }].view;
            let bind_group = self.bind(device, &uniforms, view);
            self.slots.insert(id, Slot { uniforms, bind_group, generation, veil });
        }
        if let Some(slot) = self.slots.get(&id) {
            queue.write_buffer(&slot.uniforms, 0, &bytes(&values));
        }
    }

    fn paint_glass(&self, id: u64, pass: &mut wgpu::RenderPass<'static>) {
        if let Some(slot) = self.slots.get(&id) {
            pass.set_pipeline(&self.glass);
            pass.set_bind_group(0, &slot.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/* ---------- панели ------------------------------------------------------ */

struct GlassCallback {
    id: u64,
    rect: Rect,
    radius: f32,
    tint: [[f32; 4]; 2],
    veil: bool,
}

impl egui_wgpu::CallbackTrait for GlassCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(layers) = resources.get_mut::<Layers>() {
            let k = screen.pixels_per_point;
            let mut v = [0.0f32; 16];
            v[0..4].copy_from_slice(&[self.rect.left() * k, self.rect.top() * k, self.rect.width() * k, self.rect.height() * k]);
            v[4..8].copy_from_slice(&[self.radius * k, screen.size_in_pixels[0] as f32, screen.size_in_pixels[1] as f32, if layers.srgb { 1.0 } else { 0.0 }]);
            v[8..12].copy_from_slice(&self.tint[0]);
            v[12..16].copy_from_slice(&self.tint[1]);
            layers.prepare_glass(device, queue, self.id, self.veil, v);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(layers) = resources.get::<Layers>() {
            layers.paint_glass(self.id, pass);
        }
    }
}

fn rgba(c: [f32; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, (c[3] * 255.0) as u8)
}

/// Панель без видеокарты: размытия нет, поэтому заливка плотнее и сплошная —
/// средний тон стекла поверх тёмной основы, сцена сквозь неё не рябит.
fn paint_flat(ui: &Ui, rect: Rect, radius: f32, tint: [[f32; 4]; 2]) {
    let base = [22.0 / 255.0, 22.0 / 255.0, 24.0 / 255.0];
    let avg: [f32; 4] = std::array::from_fn(|i| (tint[0][i] + tint[1][i]) / 2.0);
    let c: [f32; 3] = std::array::from_fn(|i| base[i] + (avg[i] - base[i]) * avg[3]);
    ui.painter().rect_filled(rect, radius, rgba([c[0], c[1], c[2], 0.86]));
}

/// Стеклянная панель: тень, размытая сцена с тоном, кромка и блик сверху.
/// `id` — постоянный номер панели: под него на видеокарте живут её параметры.
pub fn panel(ui: &Ui, id: u64, rect: Rect, radius: f32, gpu: bool) {
    let p = ui.painter();
    p.add(Shadow { offset: [0, 12], blur: 32, spread: 0, color: Color32::from_black_alpha(46) }.as_shape(rect, radius as u8));
    if gpu {
        p.add(egui_wgpu::Callback::new_paint_callback(rect.expand(1.0), GlassCallback { id, rect, radius, tint: GLASS_TINT, veil: false }));
    } else {
        paint_flat(ui, rect, radius, GLASS_TINT);
    }
    // Кромка и блик по верхнему краю — толщина стекла.
    p.rect_stroke(rect, radius, Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(248, 246, 238, 41)), egui::StrokeKind::Inside);
    p.line_segment([pos2(rect.left() + radius, rect.top() + 1.5), pos2(rect.right() - radius, rect.top() + 1.5)], Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 253, 246, 26)));
}

/// Затемнение с лёгким размытием за внутренними страницами: сцена остаётся видна.
pub fn veil(ui: &Ui, rect: Rect, gpu: bool) {
    if gpu {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, GlassCallback { id: u64::MAX, rect, radius: 0.0, tint: [VEIL_TINT, VEIL_TINT], veil: true }));
    } else {
        ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(14, 16, 18, 110));
    }
}
