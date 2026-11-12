#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod drag_ghost;
mod flyout_ui;
mod managed_windows;
mod manager_ui;
mod platform;
mod theme;
mod window_matching;
use eframe::egui::{self, Color32};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use taskbar_groups::{Store, shortcut_title};
use uuid::Uuid;

fn store_path() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join("TaskbarGroups")
        .join("groups.json")
}
fn main() -> eframe::Result {
    platform::enable_dpi_awareness();
    let args: Vec<String> = std::env::args().collect();
    platform::initialize_installer_shutdown();
    if args.get(1).is_some_and(|s| s == "--window-guardian") {
        if let (Some(parent), Some(path), Some(session), Some(ready)) = (
            args.get(2).and_then(|s| s.parse().ok()),
            args.get(3),
            args.get(4),
            args.get(5),
        ) && managed_windows::guardian(parent, Path::new(path), session, ready).is_ok()
        {
            return Ok(());
        }
        std::process::exit(1);
    }
    if args.get(1).is_some_and(|s| s == "--installer-shutdown") {
        if platform::request_installer_shutdown().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
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
        let mut focused = false;
        for _ in 0..10 {
            if platform::focus_manager() {
                focused = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !focused {
            let _ = rfd::MessageDialog::new()
                .set_title("Taskbar Groups")
                .set_description("The group manager is already open, or could not be started.")
                .show();
        }
        return Ok(());
    }
    let _ = Command::new(std::env::current_exe().unwrap())
        .arg("--watch")
        .spawn();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 650.0])
            .with_min_inner_size([760.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Taskbar Groups",
        options,
        Box::new(|cc| {
            if let Ok(handle) = cc.window_handle()
                && let RawWindowHandle::Win32(handle) = handle.as_raw()
            {
                platform::mark_manager_window(handle.hwnd.get());
            }
            Ok(Box::new(Manager::new(&cc.egui_ctx)))
        }),
    )
}

struct Manager {
    store: Store,
    baseline: Store,
    selected: Option<Uuid>,
    name: String,
    status: String,
    startup_state: Result<bool, String>,
    load_failed: bool,
    textures: std::collections::HashMap<Uuid, egui::TextureHandle>,
    last_refresh: std::time::Instant,
    theme: theme::ThemeTracker,
}
impl Manager {
    fn new(ctx: &egui::Context) -> Self {
        let theme = theme::ThemeTracker::new(ctx);
        match Store::load(&store_path()) {
            Ok(store) => {
                let selected = store.groups.first().map(|g| g.id);
                let mut messages = Vec::new();
                for group in &store.groups {
                    match platform::sync_saved_group_icon(&store_path(), group.id) {
                            Ok(true) => messages.push(format!(
                                "{}: if Windows still shows the old icon, unpin this group and pin its updated shortcut again.",
                                group.name
                            )),
                            Ok(false) => {},
                            Err(e) => messages.push(format!("Could not update icon for {}: {e}", group.name)),
                    }
                }
                let status = messages.join("\n");
                Self {
                    baseline: store.clone(),
                    store,
                    selected,
                    name: String::new(),
                    status,
                    startup_state: platform::startup_enabled(),
                    load_failed: false,
                    textures: Default::default(),
                    last_refresh: std::time::Instant::now(),
                    theme,
                }
            }
            Err(e) => Self {
                store: Store::default(),
                baseline: Store::default(),
                selected: None,
                name: String::new(),
                status: format!("Could not load groups: {e}. Your file was not changed."),
                startup_state: platform::startup_enabled(),
                load_failed: true,
                textures: Default::default(),
                last_refresh: std::time::Instant::now(),
                theme,
            },
        }
    }
    fn refresh(&mut self) {
        if self.last_refresh.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.last_refresh = std::time::Instant::now();
        self.startup_state = platform::startup_enabled();
        match Store::load(&store_path()) {
            Ok(current) if current != self.baseline => {
                self.store = current.clone();
                self.baseline = current;
                if !self
                    .store
                    .groups
                    .iter()
                    .any(|group| Some(group.id) == self.selected)
                {
                    self.selected = self.store.groups.first().map(|group| group.id);
                }
            }
            Err(e) => self.status = format!("Could not reload groups: {e}"),
            _ => {}
        }
    }
    fn save(&mut self) -> bool {
        match self.store.save_if_unchanged(&store_path(), &self.baseline) {
            Ok(()) => {
                self.baseline = self.store.clone();
                true
            }
            Err(e) => {
                self.store = Store::load(&store_path()).unwrap_or_else(|_| self.baseline.clone());
                self.baseline = self.store.clone();
                if !self
                    .store
                    .groups
                    .iter()
                    .any(|group| Some(group.id) == self.selected)
                {
                    self.selected = self.store.groups.first().map(|group| group.id);
                }
                self.status = if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "Groups changed in another window. Please try that action again.".into()
                } else {
                    format!("Could not save groups: {e}")
                };
                false
            }
        }
    }
    fn save_group(&mut self, id: Uuid, previous_first: Option<Uuid>) {
        if !self.save() {
            return;
        }
        if let Some(group) = self.store.groups.iter().find(|group| group.id == id)
            && group.apps.first().map(|app| app.id) != previous_first
        {
            self.status = match platform::sync_saved_group_icon(&store_path(), group.id) {
                Ok(true) => "Group icon updated. If Windows still shows the old icon, unpin this group and pin its updated shortcut again.".into(),
                Ok(false) => String::new(),
                Err(e) => format!("Could not update group icon: {e}"),
            };
        }
    }
}

struct Popup {
    group: taskbar_groups::Group,
    running: std::collections::HashMap<Uuid, Vec<window_matching::RunningWindow>>,
    choosing: Option<Uuid>,
    icons_per_row: usize,
    textures: std::collections::HashMap<Uuid, egui::TextureHandle>,
    focused_once: bool,
    last_hover: std::time::Instant,
    hover_entered: bool,
    anchor: Option<taskbar_groups::Rect>,
    position: Option<(i32, i32)>,
    last_refresh: std::time::Instant,
    drag_source: Option<(String, PathBuf)>,
    drag_texture: Option<egui::TextureHandle>,
    drag_layout_path: Option<PathBuf>,
    last_drag_layout: Option<flyout_ui::DropLayout>,
    drag_finished: bool,
    theme: theme::ThemeTracker,
}
impl Popup {
    fn resize_for_chooser(&mut self, ctx: &egui::Context, choosing: bool) {
        let (width, height) = if choosing {
            (360.0, 300.0)
        } else {
            let (width, height) =
                taskbar_groups::flyout_size_with_columns(self.group.apps.len(), self.icons_per_row);
            (
                width,
                height
                    + if self.drag_source.is_some() {
                        36.0
                    } else {
                        0.0
                    },
            )
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));
        self.position = self
            .anchor
            .and_then(|anchor| platform::position_at_anchor_sized(anchor, width, height));
    }

    fn replace_group(
        &mut self,
        ctx: &egui::Context,
        group: taskbar_groups::Group,
        icons_per_row: usize,
    ) {
        if self.choosing.is_none()
            && (self.group.apps.len() != group.apps.len() || self.icons_per_row != icons_per_row)
        {
            let (width, height) =
                taskbar_groups::flyout_size_with_columns(group.apps.len(), icons_per_row);
            let extra_height = if self.drag_source.is_some() {
                36.0
            } else {
                0.0
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                width,
                height + extra_height,
            )));
            self.position = self.anchor.and_then(|anchor| {
                platform::position_at_anchor(anchor, group.apps.len(), icons_per_row, extra_height)
            });
        }
        self.group = group;
        self.icons_per_row = icons_per_row;
    }

    fn reorder(
        &mut self,
        ctx: &egui::Context,
        dragged: Uuid,
        target: Uuid,
        side: taskbar_groups::DropSide,
    ) {
        match Store::insert_saved(&store_path(), self.group.id, dragged, target, side) {
            Ok((store, first_changed)) => {
                if let Some(group) = store
                    .groups
                    .into_iter()
                    .find(|group| group.id == self.group.id)
                {
                    self.replace_group(ctx, group, store.icons_per_row);
                }
                if first_changed {
                    match platform::sync_saved_group_icon(&store_path(), self.group.id) {
                        Ok(_) => {}
                        Err(e) => {
                            let _ = rfd::MessageDialog::new()
                                .set_title("Could not update group icon")
                                .set_description(e)
                                .show();
                        }
                    }
                }
            }
            Err(e) => {
                let _ = rfd::MessageDialog::new()
                    .set_title("Could not reorder apps")
                    .set_description(e.to_string())
                    .show();
                if let Ok(store) = Store::load(&store_path())
                    && let Some(group) = store
                        .groups
                        .into_iter()
                        .find(|group| group.id == self.group.id)
                {
                    self.replace_group(ctx, group, store.icons_per_row);
                }
            }
        }
    }
}
impl eframe::App for Popup {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        ui.spacing_mut().button_padding = egui::vec2(0.0, 0.0);
        let window = frame.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        });
        let colors = self.theme.update(ui.ctx(), window);
        if let (Some((x, y)), Some(window)) = (self.position.take(), window) {
            platform::move_window(window, x, y);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        if self.drag_source.is_some() && !platform::primary_button_down() {
            self.drag_finished = true;
        }
        let external_drag_active = self.drag_source.is_some() && !self.drag_finished;
        if external_drag_active {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        let dragging = egui::DragAndDrop::has_payload_of_type::<Uuid>(ui.ctx());
        if !dragging && !external_drag_active && platform::installer_shutdown_requested() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !dragging && self.last_refresh.elapsed() >= std::time::Duration::from_millis(500) {
            self.last_refresh = std::time::Instant::now();
            if let Ok(store) = Store::load(&store_path()) {
                self.running = running_by_app(&store);
                if let Some(group) = store
                    .groups
                    .into_iter()
                    .find(|group| group.id == self.group.id)
                {
                    self.replace_group(ui.ctx(), group, store.icons_per_row);
                } else {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let on_popup = ui.ctx().input(|i| i.pointer.hover_pos().is_some())
            || dragging
            || window.is_some_and(platform::cursor_on_window);
        let on_anchor = self.anchor.is_some_and(platform::cursor_in_rect);
        if on_popup || on_anchor {
            self.last_hover = std::time::Instant::now();
            self.hover_entered = true;
        }
        let elapsed = self.last_hover.elapsed().as_millis();
        let should_close = (self.hover_entered
            && taskbar_groups::should_dismiss_hover(on_anchor, on_popup, elapsed))
            || (!self.hover_entered && elapsed >= 3000)
            || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
        if should_close {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if let Some(focused) = ui.ctx().input(|i| i.viewport().focused) {
            if focused {
                self.focused_once = true;
            } else if self.focused_once && !dragging {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let mut reorder = None;
        let mut action = None;
        egui::Frame::new()
            .fill(colors.flyout)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if let Some(id) = self.choosing {
                    if let Some(app) = self.group.apps.iter().find(|app| app.id == id) {
                        ui.horizontal(|ui| {
                            if ui.button("Back").clicked() {
                                action = Some(PopupAction::Back);
                            }
                            ui.strong(&app.name);
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height((ui.available_height() - 42.0).max(0.0))
                            .show(ui, |ui| {
                                if let Some(windows) = self.running.get(&id) {
                                    for window in windows {
                                        if ui
                                            .add_sized(
                                                [ui.available_width(), 32.0],
                                                egui::Button::new(&window.title),
                                            )
                                            .clicked()
                                        {
                                            action = Some(PopupAction::Activate(window.clone()));
                                        }
                                    }
                                }
                            });
                        if ui.button("Open another window").clicked() {
                            action = Some(PopupAction::Launch(app.path.clone()));
                        }
                    } else {
                        action = Some(PopupAction::Back);
                    }
                    return;
                }
                if let Some((name, path)) = &self.drag_source {
                    let texture = self.drag_texture.get_or_insert_with(|| {
                        let image = platform::icon(&path.to_string_lossy()).unwrap_or_else(|| {
                            egui::ColorImage::filled([32, 32], Color32::from_rgb(70, 100, 150))
                        });
                        ui.ctx()
                            .load_texture("drag-source", image, Default::default())
                    });
                    ui.horizontal(|ui| {
                        ui.add(egui::Image::new((texture.id(), egui::vec2(24.0, 24.0))));
                        ui.label(
                            egui::RichText::new(format!("Drop {name} to add")).color(colors.text),
                        )
                        .on_hover_text(path.display().to_string());
                    });
                }
                if self.group.apps.is_empty() {
                    ui.label("No apps in this group yet.");
                }
                let mut items = Vec::new();
                let scroll = egui::ScrollArea::vertical()
                    .min_scrolled_height(0.0)
                    .max_height((ui.available_height() - 28.0).max(0.0))
                    .show(ui, |ui| {
                        flyout_ui::icon_rows(
                            ui,
                            &self.group.apps,
                            self.icons_per_row,
                            |ui, app| {
                                let tex = self.textures.entry(app.id).or_insert_with(|| {
                                    let image = platform::icon(&app.path).unwrap_or_else(|| {
                                        egui::ColorImage::filled(
                                            [32, 32],
                                            Color32::from_rgb(70, 100, 150),
                                        )
                                    });
                                    ui.ctx().load_texture(
                                        app.id.to_string(),
                                        image,
                                        Default::default(),
                                    )
                                });
                                let response =
                                    flyout_ui::render_icon(ui, app.id, &app.name, tex.id());
                                let windows = self
                                    .running
                                    .get(&app.id)
                                    .map(Vec::as_slice)
                                    .unwrap_or_default();
                                if !windows.is_empty() {
                                    ui.painter().circle_filled(
                                        egui::pos2(
                                            response.rect.center().x,
                                            response.rect.bottom() - 4.0,
                                        ),
                                        2.0,
                                        colors.text,
                                    );
                                }
                                items.push((app.id, response.rect));
                                if response.clicked() {
                                    action = Some(
                                        match flyout_ui::icon_action(
                                            windows.len(),
                                            ui.input(|i| i.modifiers.shift),
                                        ) {
                                            flyout_ui::IconAction::Launch => {
                                                PopupAction::Launch(app.path.clone())
                                            }
                                            flyout_ui::IconAction::Activate => {
                                                PopupAction::Activate(windows[0].clone())
                                            }
                                            flyout_ui::IconAction::Choose => {
                                                PopupAction::Choose(app.id)
                                            }
                                        },
                                    );
                                }
                                if response.secondary_clicked()
                                    && let Err(e) = window
                                        .ok_or_else(|| {
                                            "Could not access the group window".to_string()
                                        })
                                        .and_then(|window| {
                                            platform::context_menu(&app.path, window)
                                        })
                                {
                                    let _ = rfd::MessageDialog::new()
                                        .set_title("Unable to open menu")
                                        .set_description(e)
                                        .show();
                                }
                            },
                        );
                    });
                let clip = flyout_ui::drop_hit_area(scroll.inner_rect);
                if let Some(path) = &self.drag_layout_path
                    && external_drag_active
                {
                    let scale = ui.ctx().pixels_per_point();
                    let layout = flyout_ui::DropLayout::new(self.group.id, scale, &items, clip);
                    if self.last_drag_layout.as_ref() != Some(&layout) {
                        if layout.write_atomic(path).is_ok() {
                            self.last_drag_layout = Some(layout.clone());
                        } else {
                            let _ = std::fs::remove_file(path);
                            self.last_drag_layout = None;
                        }
                    }
                    if self.last_drag_layout.as_ref() == Some(&layout) {
                        let pointer = window
                            .and_then(platform::cursor_in_client)
                            .map(|(x, y)| egui::pos2(x as f32 / scale, y as f32 / scale));
                        flyout_ui::show_external_insertion(ui, &items, clip, pointer);
                    }
                }
                reorder = flyout_ui::finish_drag(ui, &items, clip);
                if flyout_ui::settings_button(ui).clicked() {
                    match platform::open_manager() {
                        Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(error) => {
                            let _ = rfd::MessageDialog::new()
                                .set_title("Unable to open settings")
                                .set_description(error)
                                .show();
                        }
                    }
                }
            });
        if let Some(action) = action {
            let result = match action {
                PopupAction::Back => {
                    self.choosing = None;
                    self.resize_for_chooser(ui.ctx(), false);
                    None
                }
                PopupAction::Choose(id) => {
                    self.choosing = Some(id);
                    self.resize_for_chooser(ui.ctx(), true);
                    None
                }
                PopupAction::Launch(path) => Some(platform::launch(&path)),
                PopupAction::Activate(window) => Some(window_matching::activate(&window)),
            };
            if let Some(result) = result {
                match result {
                    Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Unable to open window")
                            .set_description(error)
                            .show();
                    }
                }
            }
        }
        if let Some((dragged, target, side)) = reorder {
            self.reorder(ui.ctx(), dragged, target, side);
        }
        if dragging {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
            if ui.ctx().input(|input| input.pointer.primary_down())
                && platform::cursor_on_taskbar()
                && let Some(id) = egui::DragAndDrop::take_payload::<Uuid>(ui.ctx())
                && let Some(app) = self.group.apps.iter().find(|app| app.id == *id).cloned()
            {
                match platform::drag_app_to_taskbar(Path::new(&app.path), &app.name) {
                    Ok(true) => {
                        match Store::remove_saved_if_matches(&store_path(), self.group.id, &app) {
                            Ok((store, first_changed)) => {
                                if let Some(group) = store
                                    .groups
                                    .into_iter()
                                    .find(|group| group.id == self.group.id)
                                {
                                    self.replace_group(ui.ctx(), group, store.icons_per_row);
                                }
                                if first_changed
                                    && let Err(error) = platform::sync_saved_group_icon(
                                        &store_path(),
                                        self.group.id,
                                    )
                                {
                                    let _ = rfd::MessageDialog::new()
                                        .set_title("Could not update group icon")
                                        .set_description(error)
                                        .show();
                                }
                            }
                            Err(error) => {
                                let _ = rfd::MessageDialog::new().set_title("App pinned to taskbar")
                                .set_description(format!("The app was pinned but could not be removed from the group: {error}"))
                                .show();
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Could not pin app")
                            .set_description(error)
                            .show();
                    }
                }
            }
        }
    }
}

enum PopupAction {
    Back,
    Choose(Uuid),
    Launch(String),
    Activate(window_matching::RunningWindow),
}

fn running_by_app(
    store: &Store,
) -> std::collections::HashMap<Uuid, Vec<window_matching::RunningWindow>> {
    let mut running = std::collections::HashMap::<Uuid, Vec<_>>::new();
    let path = managed_windows::journal_path(&store_path());
    let journal = Store::with_lock(&path, || {
        Ok(
            taskbar_groups::window_recovery::RecoveryJournal::load(&path)?
                .entries()
                .to_vec(),
        )
    })
    .unwrap_or_default();
    for (id, window) in window_matching::all_group_windows(store, &journal) {
        running.entry(id).or_default().push(window);
    }
    running
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
    let running = running_by_app(&store);
    let icons_per_row = store.icons_per_row;
    let Some(group) = store.groups.into_iter().find(|g| Some(g.id) == id) else {
        let _ = rfd::MessageDialog::new()
            .set_title("Taskbar Groups")
            .set_description("This group no longer exists. Unpin its shortcut from the taskbar.")
            .show();
        return Ok(());
    };
    let drag_source = if args.get(9).is_some_and(|arg| arg == "--drag-source") {
        args.get(10)
            .zip(args.get(11))
            .map(|(name, path)| (name.clone(), PathBuf::from(path)))
    } else {
        None
    };
    let drag_layout_path =
        if drag_source.is_some() && args.get(12).is_some_and(|arg| arg == "--drag-layout") {
            args.get(13).map(PathBuf::from)
        } else {
            None
        };
    let (width, height) = taskbar_groups::flyout_size_with_columns(group.apps.len(), icons_per_row);
    let height = height + if drag_source.is_some() { 36.0 } else { 0.0 };
    let mut anchor = match (args.get(5), args.get(6), args.get(7), args.get(8)) {
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
        .with_inner_size([width, height])
        .with_decorations(false)
        .with_taskbar(false)
        .with_active(false)
        .with_always_on_top()
        .with_title(shortcut_title(&group));
    let mut position = args
        .get(3)
        .and_then(|s| s.parse::<f32>().ok())
        .zip(args.get(4).and_then(|s| s.parse::<f32>().ok()))
        .map(|(x, y)| (x as i32, y as i32));
    if position.is_none()
        && let Some((taskbar_position, taskbar_anchor)) =
            platform::taskbar_position_for_group(&group, icons_per_row)
    {
        position = Some(taskbar_position);
        anchor = Some(taskbar_anchor);
    }
    if let Some(actual_position) = anchor.and_then(|anchor| {
        platform::position_at_anchor(
            anchor,
            group.apps.len(),
            icons_per_row,
            if drag_source.is_some() { 36.0 } else { 0.0 },
        )
    }) {
        position = Some(actual_position);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Taskbar Group",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Popup {
                group,
                running,
                choosing: None,
                icons_per_row,
                textures: Default::default(),
                focused_once: false,
                last_hover: std::time::Instant::now(),
                hover_entered: false,
                anchor,
                position,
                last_refresh: std::time::Instant::now(),
                drag_source,
                drag_texture: None,
                drag_layout_path,
                last_drag_layout: None,
                drag_finished: false,
                theme: theme::ThemeTracker::new(&cc.egui_ctx),
            }))
        }),
    )
}
