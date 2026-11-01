//! Диалог закрытия и захваты по краям окна. Само окно — сцена и главная —
//! в `home.rs`.

use egui::{self, pos2, vec2, Color32, CursorIcon, Rect, ResizeDirection, Sense, Ui, ViewportCommand};

use super::{
    glass,
    kit::{self, Btn},
    App,
};

impl App {
    /// «Свернуть в трей или выйти?» — стеклянная панель поверх затемнения.
    pub(super) fn close_dialog_ui(&mut self, ui: &mut Ui) {
        let Some(remember) = self.close_dialog else { return };
        let screen = ui.max_rect();
        // Слой поверх всего: клики по окну под диалогом не проходят.
        ui.interact(screen, ui.id().with("close-modal"), Sense::click());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(120));
        let running = self.state == "on" || self.state == "timeout";
        let detail = if running {
            "При выходе обход остановится, и заблокированные сайты снова перестанут открываться."
        } else {
            "В трее PLASTO остаётся под рукой и почти не расходует ресурсы."
        };
        let panel = egui::Rect::from_center_size(screen.center(), vec2(480.0, 236.0));
        glass::panel(ui, 90, panel, 16.0, self.gpu);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink2(vec2(28.0, 24.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
        kit::text(ui, "Свернуть в трей или выйти?", kit::semibold(17.0), kit::INK);
        kit::hint(ui, detail);
        let mut remember = remember;
        if kit::checkbox(ui, remember, "Запомнить выбор и больше не спрашивать", true).clicked() {
            remember = !remember;
        }
        let mut choice = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if Btn::primary("Свернуть в трей").show(ui).clicked() {
                choice = Some("tray");
            }
            if Btn::secondary("Выйти").show(ui).clicked() {
                choice = Some("quit");
            }
            if Btn::secondary("Отмена").show(ui).clicked() {
                choice = Some("cancel");
            }
        });
        self.close_dialog = Some(remember);
        match choice {
            Some("tray") => {
                self.close_dialog = None;
                if remember {
                    self.close_action = "tray".into();
                    self.set_config(serde_json::json!({ "closeAction": "tray" }));
                }
                self.hide();
            }
            Some("quit") => {
                self.close_dialog = None;
                if remember {
                    self.close_action = "quit".into();
                    self.set_config(serde_json::json!({ "closeAction": "quit" }));
                }
                self.quit();
            }
            Some(_) => self.close_dialog = None,
            None => {}
        }
    }
}

/// Полосы по краю окна для растягивания: рамки у окна нет, тянет сама ОС.
pub fn resize_grips(ui: &mut Ui) {
    let r = ui.max_rect();
    let (e, c) = (7.0, 14.0);
    let grips = [
        (Rect::from_min_max(pos2(r.left() + c, r.top()), pos2(r.right() - c, r.top() + e)), ResizeDirection::North, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(r.left() + c, r.bottom() - e), pos2(r.right() - c, r.bottom())), ResizeDirection::South, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(r.left(), r.top() + c), pos2(r.left() + e, r.bottom() - c)), ResizeDirection::West, CursorIcon::ResizeHorizontal),
        (Rect::from_min_max(pos2(r.right() - e, r.top() + c), pos2(r.right(), r.bottom() - c)), ResizeDirection::East, CursorIcon::ResizeHorizontal),
        (Rect::from_min_size(r.left_top(), vec2(c, c)), ResizeDirection::NorthWest, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(pos2(r.right() - c, r.bottom() - c), vec2(c, c)), ResizeDirection::SouthEast, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(pos2(r.right() - c, r.top()), vec2(c, c)), ResizeDirection::NorthEast, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(pos2(r.left(), r.bottom() - c), vec2(c, c)), ResizeDirection::SouthWest, CursorIcon::ResizeNeSw),
    ];
    for (i, (rect, dir, cursor)) in grips.into_iter().enumerate() {
        let resp = ui.interact(rect, ui.id().with(("grip", i)), Sense::drag()).on_hover_cursor(cursor);
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}
