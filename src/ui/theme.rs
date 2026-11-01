//! Шрифты и то, что у бэкенда и интерфейса общее про состояния. Цвета,
//! компоненты и тёмный стиль нового вида — в `kit.rs`.

use std::sync::Arc;

use egui::{
    self,
    text::{LayoutJob, TextWrapping},
    Color32, FontData, FontDefinitions, FontFamily, FontId, Galley, Ui,
};

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Шрифты урезаны до нужного: Roboto — латиница, кириллица и типографские
/// знаки; два крошечных файла со значками (✔ ✖ ⚠ ☠ и → ⊘ ▴ ▾), которых в
/// Roboto нет. Встроенные шрифты egui (~1.4 МБ) не подключаются.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    for (name, bytes) in [
        ("Roboto-Regular", &include_bytes!("../../assets/Roboto-Regular.ttf")[..]),
        ("Roboto-SemiBold", &include_bytes!("../../assets/Roboto-SemiBold.ttf")[..]),
        ("Symbols-Icons", &include_bytes!("../../assets/Symbols-Icons.ttf")[..]),
        ("Symbols-Marks", &include_bytes!("../../assets/Symbols-Marks.ttf")[..]),
    ] {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    let with = |first: &str| [first, "Symbols-Icons", "Symbols-Marks"].map(str::to_owned).to_vec();
    fonts.families.insert(FontFamily::Proportional, with("Roboto-Regular"));
    fonts.families.insert(FontFamily::Monospace, with("Roboto-Regular"));
    fonts.families.insert(FontFamily::Name("semibold".into()), with("Roboto-SemiBold"));
    ctx.set_fonts(fonts);
}

/// Одна строка с многоточием на конце.
pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// Тон сообщения бэкенда: нейтральный, удача, внимание, ошибка.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tone {
    #[default]
    Plain,
    Ok,
    Warn,
    Bad,
}

/// Сообщение о состоянии; `**…**` в тексте — выделение, его снимает интерфейс.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub text: String,
    pub tone: Tone,
}

impl Status {
    pub fn new(tone: Tone, text: impl Into<String>) -> Self {
        Status { text: text.into(), tone }
    }
}
