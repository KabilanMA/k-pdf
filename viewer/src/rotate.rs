//! Rotate mode: a panel for turning pages: the page being read, every
//! page, or a list of pages ("1, 3-5"), a quarter turn either way or
//! upside down. Ctrl+R and Ctrl+Shift+R turn the current page without it.

use eframe::egui::{self, Color32, Id, Pos2, Rect};

use crate::icons::{self, Icon};

/// Which pages to turn.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// The page being read.
    #[default]
    Current,
    All,
    /// The pages listed in the field.
    Listed,
}

/// What the user asked for in the panel.
pub enum Action {
    /// Turn pages (positions in the view, 0-based) by `degrees`
    /// (clockwise).
    Rotate { positions: Vec<usize>, degrees: i32 },
    /// Leave rotate mode.
    Done,
}

#[derive(Default)]
pub struct Rotate {
    scope: Scope,
    /// The pages field, e.g. "1, 3-5".
    pages: String,
    /// Why the pages field can't be used, if it can't.
    pages_error: Option<String>,
}

impl Rotate {
    /// The panel at the top of the view. `current` is the position of
    /// the page being read, `count` the number of pages.
    pub fn panel(&mut self, ctx: &egui::Context, view: Rect, current: usize, count: usize) -> Option<Action> {
        let mut action = None;
        let mut turn = None;
        egui::Area::new(Id::new("rotate-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                crate::theme::floating_frame(ui).show(ui, |ui| {
                    ui.set_max_width((view.width() - 24.0).max(200.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Rotate");
                        ui.selectable_value(&mut self.scope, Scope::Current, format!("Page {}", current + 1))
                            .on_hover_text("The page being read");
                        ui.selectable_value(&mut self.scope, Scope::All, "All pages");
                        let listed = ui.selectable_value(&mut self.scope, Scope::Listed, "Pages:");
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.pages).hint_text("e.g. 1, 3-5").desired_width(90.0),
                        );
                        if field.gained_focus() || field.changed() || listed.clicked() {
                            self.scope = Scope::Listed;
                        }
                        let field = match &self.pages_error {
                            Some(e) => field.on_hover_text(e.as_str()),
                            None => field.on_hover_text("Page numbers and ranges to rotate"),
                        };
                        ui.separator();
                        if icons::button(ui, Icon::RotateLeft, Some("Left"), false, true)
                            .on_hover_text("A quarter turn counterclockwise (Ctrl+Shift+R for the current page)")
                            .clicked()
                        {
                            turn = Some(-90);
                        }
                        if icons::button(ui, Icon::RotateRight, Some("Right"), false, true)
                            .on_hover_text("A quarter turn clockwise (Ctrl+R for the current page)")
                            .clicked()
                        {
                            turn = Some(90);
                        }
                        if ui.button("180°").on_hover_text("Upside down").clicked() {
                            turn = Some(180);
                        }
                        // Enter in the field: a quarter turn clockwise.
                        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            turn = Some(90);
                        }
                        if let Some(e) = &self.pages_error {
                            ui.colored_label(Color32::from_rgb(220, 80, 80), e);
                        }
                        ui.separator();
                        if ui.button("Done").clicked() {
                            action = Some(Action::Done);
                        }
                    });
                });
            });
        if let Some(degrees) = turn {
            let positions = match self.scope {
                Scope::Current => Ok(vec![current]),
                Scope::All => Ok((0..count).collect()),
                Scope::Listed => crate::crop::parse_pages(&self.pages, count),
            };
            match positions {
                Ok(positions) => {
                    self.pages_error = None;
                    action = Some(Action::Rotate { positions, degrees });
                }
                Err(e) => self.pages_error = Some(e),
            }
        }
        action
    }
}
