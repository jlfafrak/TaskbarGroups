use super::{Manager, platform, store_path};
use eframe::egui::{self, Color32, RichText, Vec2};

#[derive(Clone, Copy)]
enum ActionIcon {
    Up,
    Down,
    Remove,
    Add,
}

fn icon_button(ui: &mut egui::Ui, icon: ActionIcon, enabled: bool) -> egui::Response {
    let label = match icon {
        ActionIcon::Up => "Move up",
        ActionIcon::Down => "Move down",
        ActionIcon::Remove => "Remove application",
        ActionIcon::Add => "Add application",
    };
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(Vec2::new(36.0, 32.0)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let color = if enabled {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let stroke = egui::Stroke::new(1.8, color);
    let center = response.rect.center();
    let point = |x: f32, y: f32| center + egui::vec2(x, y);
    let painter = ui.painter();
    match icon {
        ActionIcon::Up | ActionIcon::Down => {
            let sign = if matches!(icon, ActionIcon::Up) {
                -1.0
            } else {
                1.0
            };
            painter.line_segment([point(0.0, -6.0), point(0.0, 6.0)], stroke);
            painter.line_segment([point(-5.0, sign), point(0.0, 6.0 * sign)], stroke);
            painter.line_segment([point(5.0, sign), point(0.0, 6.0 * sign)], stroke);
        }
        ActionIcon::Add => {
            painter.line_segment([point(-6.0, 0.0), point(6.0, 0.0)], stroke);
            painter.line_segment([point(0.0, -6.0), point(0.0, 6.0)], stroke);
        }
        ActionIcon::Remove => {
            painter.line_segment([point(-6.0, -5.0), point(6.0, -5.0)], stroke);
            painter.line_segment([point(-2.5, -7.0), point(2.5, -7.0)], stroke);
            painter.line_segment([point(-4.5, -3.0), point(-4.0, 6.0)], stroke);
            painter.line_segment([point(4.5, -3.0), point(4.0, 6.0)], stroke);
            painter.line_segment([point(-4.0, 6.0), point(4.0, 6.0)], stroke);
        }
    }
    response.on_hover_text(label)
}

fn startup_buttons(ui: &mut egui::Ui, enabled: Option<bool>) -> (egui::Response, egui::Response) {
    let disable = ui
        .add_enabled(enabled == Some(true), egui::Button::new("Disable startup"))
        .on_hover_text("Stop launching hover detection when you sign in");
    let enable = ui
        .add_enabled(enabled == Some(false), egui::Button::new("Enable startup"))
        .on_hover_text("Launch hover detection when you sign in");
    (enable, disable)
}

impl eframe::App for Manager {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let window = frame
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        if let Some(window) = window {
            platform::mark_manager_window(window);
        }
        let colors = self.theme.update(ui.ctx(), window);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
        if self.load_failed {
            ui.heading("Taskbar Groups");
            ui.colored_label(colors.text, &self.status);
            return;
        }
        self.refresh();
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(colors.header)
                    .inner_margin(egui::Margin::symmetric(22, 15)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Taskbar Groups")
                                .size(22.0)
                                .strong()
                                .color(colors.text),
                        );
                        ui.label(
                            RichText::new("Your apps, organized for the taskbar")
                                .size(13.0)
                                .color(colors.muted),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (enable, disable) =
                            startup_buttons(ui, self.startup_state.as_ref().ok().copied());
                        if disable.clicked() {
                            self.status = match platform::disable_startup() {
                                Ok(()) => "Startup disabled.".into(),
                                Err(e) => format!("Could not disable startup: {e}"),
                            };
                            self.startup_state = platform::startup_enabled();
                        }
                        if enable.clicked() {
                            self.status = match platform::enable_startup() {
                                Ok(()) => "Hover will start when you sign in.".into(),
                                Err(e) => format!("Could not enable startup: {e}"),
                            };
                            self.startup_state = platform::startup_enabled();
                        }
                        if let Err(error) = &self.startup_state {
                            ui.label(
                                RichText::new("Startup status unavailable").color(colors.muted),
                            )
                            .on_hover_text(error);
                        }
                    });
                });
            });
        if !self.status.is_empty() {
            egui::Panel::bottom("status")
                .frame(
                    egui::Frame::new()
                        .fill(colors.surface)
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show(ui, |ui| {
                    ui.label(RichText::new(&self.status).color(colors.text));
                });
        }
        egui::Panel::top("flyout-preferences")
            .frame(
                egui::Frame::new()
                    .fill(colors.surface)
                    .inner_margin(egui::Margin::symmetric(22, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Icons per row").strong().color(colors.text));
                    let mut value = self.store.icons_per_row;
                    egui::ComboBox::from_id_salt("icons-per-row")
                        .selected_text(value.to_string())
                        .show_ui(ui, |ui| {
                            for count in 1..=taskbar_groups::MAX_ICONS_PER_ROW {
                                ui.selectable_value(&mut value, count, count.to_string());
                            }
                        });
                    if value != self.store.icons_per_row {
                        if let Err(error) = self.store.set_icons_per_row(value) {
                            self.status = error;
                        } else {
                            self.save();
                        }
                    }
                    ui.label(RichText::new("Applies to all groups").color(colors.muted));
                });
            });
        egui::Panel::left("groups")
            .resizable(false)
            .exact_size(235.0)
            .frame(
                egui::Frame::new()
                    .fill(colors.header)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(ui, |ui| {
                ui.label(
                    RichText::new("YOUR GROUPS")
                        .size(12.0)
                        .strong()
                        .color(colors.muted),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 115.0).max(80.0))
                    .show(ui, |ui| {
                        for group in &self.store.groups {
                            let selected = self.selected == Some(group.id);
                            let label = format!("{}   ·   {} apps", group.name, group.apps.len());
                            let text = if selected {
                                colors.accent_text
                            } else {
                                colors.text
                            };
                            let fill = if selected {
                                colors.accent
                            } else {
                                colors.surface
                            };
                            let button = egui::Button::new(RichText::new(label).color(text))
                                .selected(selected)
                                .fill(fill)
                                .min_size(Vec2::new(ui.available_width(), 40.0));
                            if ui.add(button).clicked() {
                                self.selected = Some(group.id);
                            }
                        }
                    });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(colors.background)
                    .stroke(egui::Stroke::new(1.0, colors.border))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.add_sized(
                            [ui.available_width(), 24.0],
                            egui::TextEdit::singleline(&mut self.name)
                                .frame(egui::Frame::NONE)
                                .hint_text("Group name"),
                        );
                    });
                if ui
                    .add_sized(
                        [ui.available_width(), 36.0],
                        egui::Button::new("+ Create group"),
                    )
                    .clicked()
                {
                    match self.store.add_group(&self.name) {
                        Ok(id) => {
                            self.selected = Some(id);
                            self.name.clear();
                            self.save();
                        }
                        Err(e) => self.status = e,
                    }
                }
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(colors.background).inner_margin(egui::Margin::same(24))).show(ui, |ui| {
            let Some(index) = self.store.groups.iter().position(|g| Some(g.id) == self.selected) else {
                ui.add_space(24.0);
                ui.label(RichText::new("Create your first group").size(22.0).strong().color(colors.text));
                ui.label(RichText::new("Add a group on the left, then choose the apps you want to keep together.").color(colors.muted));
                return;
            };
            let group = self.store.groups[index].clone();
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&group.name).size(24.0).strong().color(colors.text));
                    ui.label(RichText::new(format!("{} applications in this group", group.apps.len())).color(colors.muted));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Delete group").clicked() { self.store.remove_group(group.id); self.selected = self.store.groups.first().map(|g| g.id); self.save(); }
                    if ui.button("Preview").clicked() { let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg("--group").arg(group.id.to_string()).spawn(); }
                });
            });
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.label(RichText::new("APPLICATIONS · FIRST APP SETS GROUP ICON").size(12.0).strong().color(colors.muted));
            ui.add_space(4.0);
            let available = (ui.available_height() - 90.0).max(80.0);
            egui::ScrollArea::vertical().max_height(available).show(ui, |ui| {
                let row_width = ui.available_width();
                if group.apps.is_empty() { ui.label(RichText::new("No applications yet. Add one below.").color(colors.muted)); }
                for (app_index, app) in group.apps.iter().enumerate() {
                    egui::Frame::new().fill(colors.surface).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                        ui.set_min_width((row_width - 20.0).max(0.0));
                        ui.horizontal(|ui| {
                            let tex = self.textures.entry(app.id).or_insert_with(|| {
                                let image = platform::icon(&app.path).unwrap_or_else(|| egui::ColorImage::filled([32, 32], Color32::from_rgb(80, 110, 155)));
                                ui.ctx().load_texture(app.id.to_string(), image, Default::default())
                            });
                            ui.add(egui::Image::new((tex.id(), Vec2::splat(32.0))));
                            let text_width = (row_width - 276.0).max(80.0);
                            ui.allocate_ui_with_layout(Vec2::new(text_width, 40.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.set_min_width(text_width);
                                ui.horizontal(|ui| { ui.label(RichText::new(&app.name).strong().color(colors.text)); if app_index == 0 { ui.label(RichText::new("Group icon").size(11.0).color(colors.muted)); } });
                                ui.add(egui::Label::new(RichText::new(&app.path).size(12.0).color(colors.muted)).truncate()).on_hover_text(&app.path);
                            });
                            if icon_button(ui, ActionIcon::Up, app_index > 0).clicked() {
                                match self.store.move_app(group.id, app.id, -1) { Ok(true) => self.save_group(group.id, group.apps.first().map(|first| first.id)), Ok(false) => {}, Err(e) => self.status = e }
                            }
                            if icon_button(ui, ActionIcon::Down, app_index + 1 < group.apps.len()).clicked() {
                                match self.store.move_app(group.id, app.id, 1) { Ok(true) => self.save_group(group.id, group.apps.first().map(|first| first.id)), Ok(false) => {}, Err(e) => self.status = e }
                            }
                            if icon_button(ui, ActionIcon::Remove, true).clicked() { self.store.groups[index].apps.retain(|item| item.id != app.id); self.save_group(group.id, group.apps.first().map(|first| first.id)); }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if icon_button(ui, ActionIcon::Add, true).clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Applications and shortcuts", &["exe", "lnk", "url", "bat", "cmd"]).pick_file() {
                    let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    match self.store.add_app(group.id, &name, &path.to_string_lossy()) { Ok(()) => self.save_group(group.id, group.apps.first().map(|first| first.id)), Err(e) => self.status = e }
                }
                if ui.add(egui::Button::new(RichText::new("Create shortcut for pinning").color(colors.accent_text)).fill(colors.accent)).clicked() {
                    self.status = match platform::create_saved_shortcut(&store_path(), group.id) {
                        Ok(path) => match platform::reveal_shortcut(&path) { Ok(()) => "Shortcut selected in Explorer. Right-click it and choose Pin to taskbar.".into(), Err(e) => format!("Shortcut created at {} but Explorer could not select it: {e}", path.display()) },
                        Err(e) => format!("Could not create shortcut: {e}"),
                    };
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_buttons_only_allow_the_available_action() {
        for (state, expected) in [
            (Some(true), (false, true)),
            (Some(false), (true, false)),
            (None, (false, false)),
        ] {
            let ctx = egui::Context::default();
            let mut enabled = (false, false);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                let (enable, disable) = startup_buttons(ui, state);
                enabled = (enable.enabled(), disable.enabled());
            });
            output.textures_delta.clear();
            assert_eq!(enabled, expected);
        }
    }

    #[test]
    fn icon_buttons_render_and_respect_enabled_state() {
        for icon in [
            ActionIcon::Up,
            ActionIcon::Down,
            ActionIcon::Remove,
            ActionIcon::Add,
        ] {
            let ctx = egui::Context::default();
            let mut enabled = false;
            let mut disabled = true;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                enabled = icon_button(ui, icon, true).enabled();
                disabled = icon_button(ui, icon, false).enabled();
            });
            output.textures_delta.clear();
            assert!(enabled && !disabled);
            assert!(!output.shapes.is_empty());
        }
    }
}
