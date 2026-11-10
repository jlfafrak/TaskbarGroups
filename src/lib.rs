use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};
use uuid::Uuid;

pub mod pin_recovery;
pub mod window_recovery;

pub const DEFAULT_ICONS_PER_ROW: usize = 4;
pub const MAX_ICONS_PER_ROW: usize = 12;

fn default_icons_per_row() -> usize {
    DEFAULT_ICONS_PER_ROW
}

fn default_hide_taskbar_button() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub groups: Vec<Group>,
    #[serde(default = "default_icons_per_row")]
    pub icons_per_row: usize,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            icons_per_row: DEFAULT_ICONS_PER_ROW,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub apps: Vec<App>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    #[serde(default = "default_hide_taskbar_button")]
    pub hide_taskbar_button: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropSide {
    Before,
    After,
}

impl Store {
    pub fn set_icons_per_row(&mut self, value: usize) -> Result<(), String> {
        if !(1..=MAX_ICONS_PER_ROW).contains(&value) {
            return Err(format!(
                "Icons per row must be between 1 and {MAX_ICONS_PER_ROW}"
            ));
        }
        self.icons_per_row = value;
        Ok(())
    }
    pub fn add_group(&mut self, name: &str) -> Result<Uuid, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Enter a group name".into());
        }
        if self
            .groups
            .iter()
            .any(|g| g.name.eq_ignore_ascii_case(name))
        {
            return Err("A group already has that name".into());
        }
        let id = Uuid::new_v4();
        self.groups.push(Group {
            id,
            name: name.into(),
            apps: Vec::new(),
        });
        Ok(id)
    }
    pub fn add_app(&mut self, group: Uuid, name: &str, path: &str) -> Result<(), String> {
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == group)
            .ok_or("Group not found")?;
        if name.trim().is_empty() || path.trim().is_empty() {
            return Err("App name and path are required".into());
        }
        group.apps.push(App {
            id: Uuid::new_v4(),
            name: name.trim().into(),
            path: path.into(),
            hide_taskbar_button: true,
        });
        Ok(())
    }
    pub fn remove_group(&mut self, id: Uuid) {
        self.groups.retain(|g| g.id != id);
    }
    pub fn move_app(
        &mut self,
        group_id: Uuid,
        app_id: Uuid,
        offset: isize,
    ) -> Result<bool, String> {
        let group = self
            .groups
            .iter_mut()
            .find(|group| group.id == group_id)
            .ok_or("Group not found")?;
        let index = group
            .apps
            .iter()
            .position(|app| app.id == app_id)
            .ok_or("App not found")?;
        let Some(destination) = index.checked_add_signed(offset) else {
            return Ok(false);
        };
        if destination >= group.apps.len() || index == destination {
            return Ok(false);
        }
        let app = group.apps.remove(index);
        group.apps.insert(destination, app);
        Ok(true)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(data) => {
                let mut store: Self = serde_json::from_slice(&data).map_err(io::Error::other)?;
                store.icons_per_row = store.icons_per_row.clamp(1, MAX_ICONS_PER_ROW);
                Ok(store)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, data)?;
        fs::rename(temporary, path)
    }

    pub fn with_lock<T>(path: &Path, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_extension("lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.lock()?;
        action()
    }

    pub fn save_if_unchanged(&self, path: &Path, expected: &Self) -> io::Result<()> {
        Self::with_lock(path, || {
            if &Self::load(path)? != expected {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Groups changed in another window",
                ));
            }
            self.save(path)
        })
    }

    pub fn import_pinned_shortcut(
        path: &Path,
        group_id: Uuid,
        name: &str,
        source: &Path,
    ) -> io::Result<Self> {
        Self::import_pinned_shortcut_at(path, group_id, name, source, None).map(|(store, _)| store)
    }

    pub fn import_pinned_shortcut_at(
        path: &Path,
        group_id: Uuid,
        name: &str,
        source: &Path,
        position: Option<(Uuid, DropSide)>,
    ) -> io::Result<(Self, bool)> {
        Self::with_lock(path, || {
            let mut store = Self::load(path)?;
            let group = store
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Group no longer exists"))?;
            let insert_at = match position {
                Some((target, side)) => {
                    let index = group
                        .apps
                        .iter()
                        .position(|app| app.id == target)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "The insertion target no longer exists",
                            )
                        })?;
                    index + usize::from(side == DropSide::After)
                }
                None => group.apps.len(),
            };
            let first_changed = insert_at == 0;
            if !source
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Expected a pinned shortcut",
                ));
            }
            let dir = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("shortcuts");
            fs::create_dir_all(&dir)?;
            let destination = dir.join(format!("{}.lnk", Uuid::new_v4()));
            fs::copy(source, &destination)?;
            let result = store
                .add_app(group_id, name, &destination.to_string_lossy())
                .map_err(io::Error::other)
                .and_then(|_| {
                    let group = store
                        .groups
                        .iter_mut()
                        .find(|group| group.id == group_id)
                        .unwrap();
                    let new_app = group.apps.pop().unwrap();
                    group.apps.insert(insert_at, new_app);
                    store.save(path)
                });
            if let Err(error) = result {
                let _ = fs::remove_file(destination);
                return Err(error);
            }
            Ok((store, first_changed))
        })
    }

    pub fn move_pinned_shortcut(
        path: &Path,
        group_id: Uuid,
        name: &str,
        source: &Path,
        unpin: impl FnOnce(&Path) -> Result<(), String>,
    ) -> io::Result<(Self, Result<(), String>)> {
        let (saved, unpin, _) =
            Self::move_pinned_shortcut_at(path, group_id, name, source, None, unpin)?;
        Ok((saved, unpin))
    }

    pub fn move_pinned_shortcut_at(
        path: &Path,
        group_id: Uuid,
        name: &str,
        source: &Path,
        position: Option<(Uuid, DropSide)>,
        unpin: impl FnOnce(&Path) -> Result<(), String>,
    ) -> io::Result<(Self, Result<(), String>, bool)> {
        let (saved, first_changed) =
            Self::import_pinned_shortcut_at(path, group_id, name, source, position)?;
        let result = unpin(source);
        Ok((saved, result, first_changed))
    }

    pub fn remove_saved_if_matches(
        path: &Path,
        group_id: Uuid,
        app: &App,
    ) -> io::Result<(Self, bool)> {
        Self::with_lock(path, || {
            let mut store = Self::load(path)?;
            let group = store
                .groups
                .iter_mut()
                .find(|group| group.id == group_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Group no longer exists"))?;
            let index = group
                .apps
                .iter()
                .position(|current| current == app)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "The app was changed or removed")
                })?;
            let first_changed = index == 0;
            group.apps.remove(index);
            store.save(path)?;
            Ok((store, first_changed))
        })
    }

    pub fn insert_saved(
        path: &Path,
        group_id: Uuid,
        dragged: Uuid,
        target: Uuid,
        side: DropSide,
    ) -> io::Result<(Self, bool)> {
        Self::with_lock(path, || {
            let mut store = Self::load(path)?;
            let group = store
                .groups
                .iter_mut()
                .find(|group| group.id == group_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Group no longer exists"))?;
            let before = group.apps.first().map(|app| app.id);
            let from = group
                .apps
                .iter()
                .position(|app| app.id == dragged)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "Dragged app no longer exists")
                })?;
            let target_index = group
                .apps
                .iter()
                .position(|app| app.id == target)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "Target app no longer exists")
                })?;
            if dragged == target {
                return Ok((store, false));
            }
            let adjusted_target = target_index - usize::from(from < target_index);
            let destination = adjusted_target + usize::from(side == DropSide::After);
            if from == destination {
                return Ok((store, false));
            }
            let app = group.apps.remove(from);
            group.apps.insert(destination, app);
            let first_changed = group.apps.first().map(|app| app.id) != before;
            store.save(path)?;
            Ok((store, first_changed))
        })
    }
}

pub fn shortcut_title(group: &Group) -> String {
    format!(
        "{} - Taskbar Groups ({})",
        group.name,
        &group.id.to_string()[..8]
    )
    .replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_")
}

pub fn app_id(id: &Uuid) -> String {
    format!("TaskbarGroups.Group.{}", id.simple())
}

pub fn group_for_taskbar_name(groups: &[Group], name: &str) -> Option<Uuid> {
    groups
        .iter()
        .find(|g| shortcut_title(g) == name)
        .map(|g| g.id)
}

pub fn group_for_automation_id(groups: &[Group], automation_id: &str) -> Option<Uuid> {
    let id = automation_id.strip_prefix("Appid: ")?;
    groups
        .iter()
        .find(|group| app_id(&group.id).eq_ignore_ascii_case(id))
        .map(|group| group.id)
}

pub fn flyout_grid(count: usize) -> (usize, usize) {
    flyout_grid_with_columns(count, DEFAULT_ICONS_PER_ROW)
}

pub fn flyout_grid_with_columns(count: usize, icons_per_row: usize) -> (usize, usize) {
    let columns = count.clamp(1, icons_per_row.clamp(1, MAX_ICONS_PER_ROW));
    (columns, count.max(1).div_ceil(columns))
}

#[derive(Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy)]
pub struct DragTarget {
    pub group: Uuid,
    pub bounds: Rect,
}

pub struct PinDrag {
    targets: Vec<DragTarget>,
    visible: Vec<DragTarget>,
    hovering: Option<(Uuid, u128)>,
    opened: Option<Uuid>,
}
impl PinDrag {
    pub fn new(targets: Vec<DragTarget>) -> Self {
        Self {
            visible: targets.clone(),
            targets,
            hovering: None,
            opened: None,
        }
    }
    pub fn set_visible_targets(&mut self, targets: Vec<DragTarget>) {
        self.visible = targets;
    }
    pub fn hover(&mut self, x: i32, y: i32, now_ms: u128) -> Option<Uuid> {
        let contains = |bounds: Rect| {
            x >= bounds.left && x < bounds.right && y >= bounds.top && y < bounds.bottom
        };
        let mut candidates = self
            .targets
            .iter()
            .filter(|item| {
                contains(item.bounds)
                    || self
                        .visible
                        .iter()
                        .any(|visible| visible.group == item.group && contains(visible.bounds))
            })
            .map(|item| item.group);
        let target = candidates.next();
        if candidates.next().is_some() {
            self.popup_closed();
            return None;
        }
        if target.is_some_and(|group| {
            self.visible.iter().any(|item| {
                item.group != group
                    && x >= item.bounds.left
                    && x < item.bounds.right
                    && y >= item.bounds.top
                    && y < item.bounds.bottom
            })
        }) {
            self.popup_closed();
            return None;
        }
        if let Some(opened) = self.opened {
            if target.is_none() || target == Some(opened) {
                return None;
            }
            self.opened = None;
        }
        match (target, self.hovering) {
            (Some(group), Some((prior, since)))
                if group == prior && now_ms.saturating_sub(since) >= 120 =>
            {
                self.opened = Some(group);
                Some(group)
            }
            (Some(group), Some((prior, _))) if group == prior => None,
            (Some(group), _) => {
                self.hovering = Some((group, now_ms));
                None
            }
            (None, _) => {
                self.hovering = None;
                None
            }
        }
    }
    pub fn popup_closed(&mut self) {
        self.opened = None;
        self.hovering = None;
    }
    pub fn opened_group(&self) -> Option<Uuid> {
        self.opened
    }
    pub fn release(&mut self, flyout: Option<Uuid>) -> Option<Uuid> {
        let opened = self.opened.take();
        (opened == flyout).then_some(opened).flatten()
    }
}

pub fn should_dismiss_hover(on_anchor: bool, on_popup: bool, elapsed_ms: u128) -> bool {
    !on_anchor && !on_popup && elapsed_ms >= 1500
}

pub fn flyout_size(count: usize) -> (f32, f32) {
    flyout_size_with_columns(count, DEFAULT_ICONS_PER_ROW)
}

pub fn flyout_size_with_columns(count: usize, icons_per_row: usize) -> (f32, f32) {
    let (columns, rows) = flyout_grid_with_columns(count, icons_per_row);
    (
        (columns as f32 * 60.0 + 32.0).max(120.0),
        (rows as f32 * 60.0 + 24.0).min(384.0) + 28.0,
    )
}

pub fn position_flyout(
    work: Rect,
    taskbar: Rect,
    anchor: Rect,
    width: f32,
    height: f32,
    scale: f32,
) -> (i32, i32) {
    let width = (width * scale).round() as i32;
    let height = (height * scale).round() as i32;
    let center_x = (anchor.left + anchor.right) / 2;
    let center_y = (anchor.top + anchor.bottom) / 2;
    let x = if taskbar.right <= work.left {
        work.left
    } else if taskbar.left >= work.right {
        work.right - width
    } else {
        center_x - width / 2
    };
    let y = if taskbar.right <= work.left || taskbar.left >= work.right {
        center_y - height / 2
    } else if taskbar.bottom <= work.top {
        work.top
    } else {
        work.bottom - height
    };
    (
        x.clamp(work.left, (work.right - width).max(work.left)),
        y.clamp(work.top, (work.bottom - height).max(work.top)),
    )
}

pub fn popup_position(
    work: Rect,
    taskbar: Rect,
    cursor_x: i32,
    cursor_y: i32,
    height: f32,
    scale: f32,
) -> (f32, f32) {
    let scale = scale.max(0.5);
    let width_px = 320.0 * scale;
    let height_px = height * scale;
    let mut x = (cursor_x as f32).clamp(
        work.left as f32,
        (work.right as f32 - width_px).max(work.left as f32),
    );
    let y = if taskbar.right <= work.left || taskbar.left >= work.right {
        x = if taskbar.right <= work.left {
            work.left as f32
        } else {
            (work.right as f32 - width_px).max(work.left as f32)
        };
        (cursor_y as f32).clamp(
            work.top as f32,
            (work.bottom as f32 - height_px).max(work.top as f32),
        )
    } else if taskbar.bottom <= work.top {
        work.top as f32
    } else {
        (work.bottom as f32 - height_px).max(work.top as f32)
    };
    (x, y)
}
