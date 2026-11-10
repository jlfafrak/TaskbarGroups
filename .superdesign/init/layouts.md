# Existing egui UI in src/main.rs

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;
use eframe::egui::{self, Color32, RichText, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{path::PathBuf, process::Command};
use taskbar_groups::{Store, shortcut_title};
use uuid::Uuid;

fn store_path() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join("TaskbarGroups")
        .join("groups.json")
}
fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--watch") {
        platform::watch(&store_path());
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--group") {
        let id = args.get(2).and_then(|v| Uuid::parse_str(v).ok());
        if let Some(id) = id {
            platform::set_app_id(&id);
        }
        return run_popup(id, &args);
    }
    if !platform::acquire_manager() {
        let _ = rfd::MessageDialog::new()
            .set_title("Taskbar Groups")
            .set_description("The group manager is already open, or could not be started.")
            .show();
        return Ok(());
    }
    let _ = Command::new(std::env::current_exe().unwrap())
        .arg("--watch")
        .spawn();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 520.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Taskbar Groups",
        options,
        Box::new(|_cc| Ok(Box::new(Manager::new()))),
    )
}

struct Manager {
    store: Store,
    selected: Option<Uuid>,
    name: String,
    status: String,
    load_failed: bool,
}
impl Manager {
    fn new() -> Self {
        match Store::load(&store_path()) {
            Ok(store) => {
                let selected = store.groups.first().map(|g| g.id);
                Self {
                    store,
                    selected,
                    name: String::new(),
                    status: String::new(),
                    load_failed: false,
                }
            }
            Err(e) => Self {
                store: Store::default(),
                selected: None,
                name: String::new(),
                status: format!("Could not load groups: {e}. Your file was not changed."),
                load_failed: true,
            },
        }
    }
    fn save(&mut self) {
        if let Err(e) = self.store.save(&store_path()) {
            self.status = format!("Could not save groups: {e}");
        }
    }
}
impl eframe::App for Manager {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("Taskbar Groups");
        if self.load_failed {
            ui.colored_label(Color32::LIGHT_RED, &self.status);
            return;
        }
        ui.label("Create groups of shortcuts and pin each group to your taskbar.");
        ui.horizontal(|ui| {
            if ui.button("Enable hover after sign-in").clicked() {
                self.status = match platform::enable_startup() {
                    Ok(()) => "Hover will start when you sign in.".into(),
                    Err(e) => format!("Could not enable startup: {e}"),
                };
            }
            if ui.button("Disable startup").clicked() {
                self.status = match platform::disable_startup() {
                    Ok(()) => "Startup disabled.".into(),
                    Err(e) => format!("Could not disable startup: {e}"),
                };
            }
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(220.0);
                ui.strong("GROUPS");
                for group in &self.store.groups {
                    if ui.selectable_label(self.selected == Some(group.id), format!("{}  ·  {}", group.name, group.apps.len())).clicked() { self.selected = Some(group.id); }
                }
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("New group name"));
                if ui.button("+ Create group").clicked() {
                    match self.store.add_group(&self.name) {
                        Ok(id) => { self.selected = Some(id); self.name.clear(); self.save(); },
                        Err(e) => self.status = e,
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                if let Some(index) = self.store.groups.iter().position(|g| Some(g.id) == self.selected) {
                    let group = &self.store.groups[index];
                    ui.heading(&group.name);
                    ui.label("Click an item to open it. Right-click it in the group flyout for its Windows menu.");
                    ui.add_space(8.0);
                    let mut remove = None;
                    for app in &group.apps {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&app.name).strong());
                            ui.label(RichText::new(&app.path).small().color(Color32::GRAY));
                            if ui.small_button("Remove").clicked() { remove = Some(app.id); }
                        });
                    }
                    if let Some(id) = remove { self.store.groups[index].apps.retain(|a| a.id != id); self.save(); }
                    ui.add_space(12.0);
                    if ui.button("+ Add application or shortcut").clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("Applications and shortcuts", &["exe", "lnk", "url", "bat", "cmd"]).pick_file() {
                            let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                            let id = self.store.groups[index].id;
                            match self.store.add_app(id, &name, &path.to_string_lossy()) { Ok(()) => self.save(), Err(e) => self.status = e }
                        }
                    ui.horizontal(|ui| {
                        if ui.button("Create shortcut for pinning").clicked() {
                            match platform::create_shortcut(&self.store.groups[index]) {
                                Ok(path) => { self.status = match platform::reveal_shortcut(&path) { Ok(()) => "Shortcut selected in Explorer. Right-click it and choose Pin to taskbar (under Show more options if needed).".into(), Err(e) => format!("Shortcut created at {} but Explorer could not select it: {e}", path.display()) }; },
                                Err(e) => self.status = format!("Could not create shortcut: {e}"),
                            }
                        }
                        if ui.button("Preview group").clicked() { let _ = Command::new(std::env::current_exe().unwrap()).arg("--group").arg(self.store.groups[index].id.to_string()).spawn(); }
                    });
                    if ui.button("Delete group").clicked() { self.store.remove_group(self.store.groups[index].id); self.selected = self.store.groups.first().map(|g| g.id); self.save(); }
                } else { ui.label("Create a group to get started."); }
            });
        });
        ui.add_space(16.0);
        if !self.status.is_empty() {
            ui.separator();
            ui.label(&self.status);
        }
        });
    }
}

struct Popup {
    group: taskbar_groups::Group,
    textures: std::collections::HashMap<Uuid, egui::TextureHandle>,
    focused_once: bool,
    last_hover: std::time::Instant,
    anchor: Option<taskbar_groups::Rect>,
    position: Option<(i32, i32)>,
}
impl eframe::App for Popup {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let window = frame.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        });
        if let (Some((x, y)), Some(window)) = (self.position.take(), window) {
            platform::move_window(window, x, y);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        if let Some(anchor) = self.anchor {
            let on_popup = ui.ctx().input(|i| i.pointer.hover_pos().is_some());
            let on_anchor = platform::cursor_in_rect(anchor);
            if on_popup || on_anchor {
                self.last_hover = std::time::Instant::now();
            }
            if taskbar_groups::should_dismiss_hover(
                on_anchor,
                on_popup,
                self.last_hover.elapsed().as_millis(),
            ) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(focused) = ui.ctx().input(|i| i.viewport().focused) {
            if focused {
                self.focused_once = true;
            } else if self.focused_once {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ui.horizontal(|ui| {
            ui.heading(&self.group.name);
            if ui.button("×").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.separator();
        if self.group.apps.is_empty() {
            ui.label("No apps in this group yet.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for app in &self.group.apps {
                let tex = self.textures.entry(app.id).or_insert_with(|| {
                    let image = platform::icon(&app.path).unwrap_or_else(|| {
                        egui::ColorImage::filled([32, 32], Color32::from_rgb(70, 100, 150))
                    });
                    ui.ctx()
                        .load_texture(app.id.to_string(), image, Default::default())
                });
                let row = ui.horizontal(|ui| {
                    let icon = ui.add(
                        egui::Image::new((tex.id(), Vec2::splat(32.0))).sense(egui::Sense::click()),
                    );
                    let button =
                        ui.add(egui::Button::new(&app.name).min_size(Vec2::new(210.0, 36.0)));
                    icon.union(button)
                });
                let response = row.inner;
                if response.clicked() {
                    if let Err(e) = platform::launch(&app.path) {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Unable to launch")
                            .set_description(e)
                            .show();
                    } else {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                if response.secondary_clicked()
                    && let Err(e) = window
                        .ok_or_else(|| "Could not access the group window".to_string())
                        .and_then(|window| platform::context_menu(&app.path, window))
                {
                    let _ = rfd::MessageDialog::new()
                        .set_title("Unable to open menu")
                        .set_description(e)
                        .show();
                }
            }
        });
    }
}
fn run_popup(id: Option<Uuid>, args: &[String]) -> eframe::Result {
    let store = match Store::load(&store_path()) {
        Ok(s) => s,
        Err(e) => {
            let _ = rfd::MessageDialog::new()
                .set_title("Taskbar Groups")
                .set_description(e.to_string())
                .show();
            return Ok(());
        }
    };
    let Some(group) = store.groups.into_iter().find(|g| Some(g.id) == id) else {
        let _ = rfd::MessageDialog::new()
            .set_title("Taskbar Groups")
            .set_description("This group no longer exists. Unpin its shortcut from the taskbar.")
            .show();
        return Ok(());
    };
    let height = (85.0 + group.apps.len() as f32 * 46.0).clamp(150.0, 500.0);
    let anchor = match (args.get(5), args.get(6), args.get(7), args.get(8)) {
        (Some(l), Some(t), Some(r), Some(b)) => {
            match (l.parse(), t.parse(), r.parse(), b.parse()) {
                (Ok(left), Ok(top), Ok(right), Ok(bottom)) => Some(taskbar_groups::Rect {
                    left,
                    top,
                    right,
                    bottom,
                }),
                _ => None,
            }
        }
        _ => None,
    };
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([320.0, height])
        .with_always_on_top()
        .with_title(shortcut_title(&group));
    let position = args
        .get(3)
        .and_then(|s| s.parse::<f32>().ok())
        .zip(args.get(4).and_then(|s| s.parse::<f32>().ok()))
        .map(|(x, y)| (x as i32, y as i32));
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Taskbar Group",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(Popup {
                group,
                textures: Default::default(),
                focused_once: false,
                last_hover: std::time::Instant::now(),
                anchor,
                position,
            }))
        }),
    )
}

```
