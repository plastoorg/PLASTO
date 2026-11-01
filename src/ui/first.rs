//! Первый запуск: компонентов zapret ещё нет. Справа вместо «Что работает» —
//! шаги установки, на экране монитора — проценты, внизу — что делать дальше.

use egui::{pos2, vec2, Align2, Rect, Ui};
use serde_json::Value;

use super::{
    glass,
    icons::{self, Icon},
    kit::{self, Btn},
    scene::Screen,
    App, DiagTab, Op,
};

/// Где сейчас первый запуск.
#[derive(Default)]
pub struct First {
    /// Загрузка не началась: автозагрузка выключена, ждём выбора человека.
    pub waiting: bool,
    /// Установка не удалась — текст ошибки.
    pub failed: Option<String>,
    /// Всё готово: осталось выбрать, что дальше.
    pub done: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Wait,
    Run,
    Done,
    Fail,
}

impl App {
    /// Что показывает экран монитора на первом запуске.
    pub(super) fn first_screen(&self) -> Option<Screen> {
        let first = self.first.as_ref()?;
        Some(if first.done {
            Screen::On
        } else if first.failed.is_some() {
            Screen::Fail
        } else if first.waiting {
            Screen::Off
        } else {
            Screen::Boot(self.boot.as_ref().map_or(0.0, |b| b.frac))
        })
    }

    /// Номер идущего шага (1..5) по событиям установки.
    fn first_step(&self) -> usize {
        match &self.boot {
            None => 2,
            Some(b) if b.title.starts_with("Распаков") => 5,
            Some(b) if b.frac >= 1.0 => 4,
            Some(b) if b.frac > 0.0 => 3,
            Some(_) => 2,
        }
    }

    pub(super) fn first_title(&self) -> (String, String) {
        let Some(first) = &self.first else { return (String::new(), String::new()) };
        if first.done {
            ("Всё готово".into(), "Осталось выбрать стратегию: способ, которым обходятся блокировки.".into())
        } else if first.failed.is_some() {
            ("Не удалось скачать компоненты".into(), "Можно попробовать ещё раз или указать папку zapret, скачанную вручную.".into())
        } else if first.waiting {
            ("Нужны компоненты обхода".into(), "PLASTO скачает последний релиз zapret с GitHub или возьмёт вашу папку.".into())
        } else {
            ("Устанавливаю компоненты".into(), "Это нужно один раз. Обычно занимает меньше минуты.".into())
        }
    }

    /// Первый шаг: права и то, что найдёт проверка системы.
    fn system_line(&self) -> String {
        if !self.elevated {
            return "нет прав администратора: обход не запустится".into();
        }
        if self.diag.busy.is_some() {
            return "права, WinDivert, конфликтующие программы…".into();
        }
        let count = |level: &str| self.diag.rows.iter().filter(|r| r["level"] == level).count();
        match (count("fail"), count("warn")) {
            _ if self.diag.rows.is_empty() => "права администратора есть".into(),
            (0, 0) => "всё в порядке".into(),
            (0, w) => format!("замечаний: {w}, подробности в «Диагностике»"),
            (f, _) => format!("проблем: {f}, подробности в «Диагностике»"),
        }
    }

    /// Начать первый запуск: заодно проверить систему.
    pub(super) fn begin_first(&mut self, first: First) {
        self.first = Some(first);
        if self.diag.rows.is_empty() && self.diag.busy.is_none() {
            self.run_diag();
        }
    }

    fn install_components(&mut self) {
        if let Some(first) = self.first.as_mut() {
            first.failed = None;
            first.waiting = false;
        }
        self.boot = Some(super::Boot { title: "Загружаю компоненты".into(), sub: "последний релиз zapret".into(), frac: 0.0 });
        self.call(Op::ZapretInstall { auto: true }, "zapret:install", Value::Null);
    }

    /// Справа сверху: шаги установки.
    pub(super) fn first_panel(&mut self, ui: &mut Ui, rect: Rect) {
        let Some(first) = &self.first else { return };
        let running = self.first_step();
        let checking = self.diag.busy.is_some();
        let state = |n: usize| {
            if n == 1 && checking {
                Step::Run
            } else if first.done || n == 1 || n < running && first.failed.is_none() && !first.waiting {
                Step::Done
            } else if first.failed.is_some() && n == running {
                Step::Fail
            } else if n == running && !first.waiting && first.failed.is_none() {
                Step::Run
            } else {
                Step::Wait
            }
        };
        let count = self.strategies.len();
        let steps: [(&str, String); 5] = [
            ("Проверка системы", self.system_line()),
            ("Последняя версия zapret", "релиз Flowseal zapret-discord-youtube на GitHub".into()),
            ("Загрузка", self.boot.as_ref().filter(|_| running == 3).map(|b| b.sub.clone()).unwrap_or_default()),
            ("Проверка подлинности", "размер и SHA-256 сверяются с GitHub".into()),
            ("Распаковка", if first.done && count > 0 { format!("найдено стратегий: {count}") } else { String::new() }),
        ];
        let row_h = 60.0;
        let panel = Rect::from_min_size(pos2(rect.right() - 24.0 - 440.0, rect.top() + 76.0), vec2(440.0, 72.0 + row_h * 5.0 + 72.0));
        glass::panel(ui, 40, panel, 16.0, self.gpu);
        let p = ui.painter().clone();
        p.text(pos2(panel.left() + 24.0, panel.top() + 20.0), Align2::LEFT_TOP, "Первый запуск", kit::semibold(17.0), kit::INK);
        p.text(pos2(panel.left() + 24.0, panel.top() + 44.0), Align2::LEFT_TOP, "Компоненты обхода: Flowseal zapret-discord-youtube", kit::regular(13.0), kit::INK_3);
        let mut y = panel.top() + 72.0;
        for (i, (name, sub)) in steps.iter().enumerate() {
            let n = i + 1;
            let st = state(n);
            let row = Rect::from_min_size(pos2(panel.left() + 12.0, y), vec2(panel.width() - 24.0, row_h));
            if st == Step::Run {
                p.rect_filled(row, 12.0, kit::FILL_1);
            }
            let c = pos2(row.left() + 26.0, row.top() + 22.0);
            match st {
                Step::Done => {
                    p.circle_filled(c, 14.0, kit::OK);
                    icons::paint(&p, c, Icon::Check, 16.0, kit::OK_INK);
                }
                Step::Fail => {
                    p.circle_filled(c, 14.0, kit::BAD_STRONG);
                    icons::paint(&p, c, Icon::Cross, 14.0, kit::INK_STRONG);
                }
                Step::Run => {
                    p.circle_stroke(c, 12.5, egui::Stroke::new(3.0_f32, kit::FILL_2));
                    let t = ui.ctx().input(|i| i.time) as f32;
                    let a0 = (t / 0.9).fract() * std::f32::consts::TAU;
                    let arc: Vec<_> = (0..=12).map(|k| c + egui::Vec2::angled(a0 + k as f32 / 12.0 * 1.6) * 12.5).collect();
                    p.add(egui::Shape::line(arc, egui::Stroke::new(3.0_f32, kit::OK)));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                }
                Step::Wait => {
                    p.circle_stroke(c, 13.0, egui::Stroke::new(2.0_f32, kit::HAIR_2));
                    p.text(c, Align2::CENTER_CENTER, n.to_string(), kit::semibold(13.0), kit::INK_3);
                }
            }
            let color = if st == Step::Wait { kit::INK_3 } else { kit::INK };
            p.text(pos2(row.left() + 56.0, row.top() + 12.0), Align2::LEFT_TOP, *name, kit::semibold(15.0), color);
            let sub = if st == Step::Fail { first.failed.clone().unwrap_or_default() } else if st == Step::Wait && n > 1 { String::new() } else { sub.clone() };
            if !sub.is_empty() {
                let g = p.layout(sub, kit::regular(13.0), if st == Step::Fail { kit::BAD_TEXT } else { kit::INK_3 }, row.width() - 72.0);
                p.galley(pos2(row.left() + 56.0, row.top() + 34.0), g, kit::INK_3);
            }
            if st == Step::Run && n == 3 {
                let bar = Rect::from_min_size(pos2(row.left() + 56.0, row.bottom() - 8.0), vec2(row.width() - 72.0, 4.0));
                kit::paint_bar(&p, bar, self.boot.as_ref().map_or(0.0, |b| b.frac));
            }
            y += row_h;
        }
        let foot = if first.failed.is_some() {
            "Скачайте архив zapret-discord-youtube с GitHub на другом устройстве или через VPN, распакуйте и укажите папку.".to_owned()
        } else {
            format!("Файлы: {}", if self.dir.is_empty() { "папка данных PLASTO\\zapret".to_owned() } else { self.dir.clone() })
        };
        p.line_segment([pos2(panel.left() + 24.0, panel.bottom() - 64.0), pos2(panel.right() - 24.0, panel.bottom() - 64.0)], egui::Stroke::new(1.0_f32, kit::HAIR));
        // Ошибка — переносом, путь к файлам — в одну строку с многоточием.
        let g = if first.failed.is_some() { p.layout(foot, kit::regular(13.0), kit::INK_3, panel.width() - 48.0) } else { super::theme::elided(ui, &foot, kit::regular(13.0), kit::INK_3, panel.width() - 48.0) };
        p.galley(pos2(panel.left() + 24.0, panel.bottom() - 52.0), g, kit::INK_3);
    }

    /// Внизу справа: что делать дальше на этом шаге.
    pub(super) fn first_actions(&mut self, ui: &mut Ui, rect: Rect) {
        let Some(first) = &self.first else { return };
        let (done, failed, waiting) = (first.done, first.failed.is_some(), first.waiting);
        let bottom = rect.bottom() - 32.0;
        let place = |w: f32, right: f32, big: bool| {
            let h = if big { 52.0 } else { 40.0 };
            Rect::from_min_size(pos2(right - w, bottom - (52.0 + h) / 2.0), vec2(w, h))
        };
        let right = rect.right() - 24.0;
        if done {
            let main = Btn::primary("Подобрать стратегию").icon(Icon::Gauge).big();
            let mw = main.width(ui).max(240.0);
            if main.show_at(ui, place(mw, right, true), "first-tests").clicked() {
                self.first = None;
                self.open_diag(DiagTab::Tests);
            }
            let alt = Btn::secondary("Сразу включить");
            let aw = alt.width(ui);
            if alt.show_at(ui, place(aw, right - mw - 16.0, false), "first-on").clicked() {
                self.first = None;
                self.power();
            }
        } else if failed || waiting {
            let main = Btn::primary(if failed { "Повторить" } else { "Скачать компоненты" }).icon(Icon::Refresh).big();
            let mw = main.width(ui).max(240.0);
            if main.show_at(ui, place(mw, right, true), "first-retry").clicked() {
                self.install_components();
            }
            let alt = Btn::secondary("Указать папку вручную");
            let aw = alt.width(ui);
            if alt.show_at(ui, place(aw, right - mw - 16.0, false), "first-pick").clicked() {
                self.call(Op::PickDir, "zapret:pickDir", Value::Null);
            }
        } else {
            let alt = Btn::secondary("Указать свою папку zapret");
            let aw = alt.width(ui);
            if alt.show_at(ui, place(aw, right, false), "first-pick").clicked() {
                self.call(Op::PickDir, "zapret:pickDir", Value::Null);
            }
        }
    }
}
