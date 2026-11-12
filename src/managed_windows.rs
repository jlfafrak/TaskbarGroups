//! Manage taskbar buttons with a separate process ready to restore them.
use crate::window_matching::{self, RunningWindow};
use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc,
    thread,
    time::Duration,
};
use taskbar_groups::{
    Store,
    window_recovery::{IdentityChange, RecoveryJournal, WindowKey},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, PROPERTYKEY, WAIT_OBJECT_0},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, StructuredStorage::PROPVARIANT,
            },
            Threading::{
                CreateEventW, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                ITaskbarList,
                PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow},
                TaskbarList,
            },
            WindowsAndMessaging::{
                GetPropW, GetWindowThreadProcessId, IsWindow, RemovePropW, SetPropW,
            },
        },
    },
    core::{GUID, PCWSTR},
};

fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(Some(0))
        .collect()
}
fn hwnd(key: &WindowKey) -> HWND {
    HWND(key.hwnd as *mut _)
}
fn marker_value() -> HANDLE {
    HANDLE(std::ptr::dangling_mut())
}
fn shell() -> io::Result<ITaskbarList> {
    unsafe {
        let shell: ITaskbarList =
            CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).map_err(io::Error::other)?;
        shell.HrInit().map_err(io::Error::other)?;
        Ok(shell)
    }
}
fn should_restore_identity(current: Option<&str>, assigned: &str) -> bool {
    current.is_some_and(|id| id.eq_ignore_ascii_case(assigned))
}
fn identity_changed(current: Option<&str>, assigned: &str) -> bool {
    !should_restore_identity(current, assigned)
}
fn set_window_identity(window: HWND, identity: Option<&str>) -> io::Result<()> {
    unsafe {
        let properties: IPropertyStore =
            SHGetPropertyStoreForWindow(window).map_err(io::Error::other)?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        let value = identity.map(PROPVARIANT::from).unwrap_or_default();
        properties.SetValue(&key, &value).map_err(io::Error::other)
    }
}
fn restore(shell: &ITaskbarList, key: &WindowKey) -> io::Result<()> {
    let window = hwnd(key);
    unsafe {
        if !IsWindow(Some(window)).as_bool() {
            return Ok(());
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pid != key.pid {
            return Ok(());
        }
        let marker = wide(&key.marker);
        if GetPropW(window, PCWSTR(marker.as_ptr())) != marker_value() {
            return Ok(());
        }
        let current = RunningWindow {
            hwnd: key.hwnd,
            pid: key.pid,
            process_started: key.process_started,
            title: String::new(),
        };
        if !window_matching::window_is_same(&current) {
            return Err(io::Error::other(
                "Cannot verify the window for taskbar recovery",
            ));
        }
        if let Some(change) = &key.identity {
            let current = window_matching::explicit_window_id(window)
                .ok_or_else(|| io::Error::other("Cannot inspect window identity for recovery"))?;
            if should_restore_identity(current.as_deref(), &change.assigned) {
                set_window_identity(window, change.original.as_deref())?;
            }
        } else {
            shell.AddTab(window).map_err(io::Error::other)?;
        }
        let _ = RemovePropW(window, PCWSTR(marker.as_ptr()));
        Ok(())
    }
}
pub(crate) fn journal_path(store_path: &Path) -> PathBuf {
    store_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("hidden-windows.json")
}
fn recover(path: &Path, session: Option<&str>) -> io::Result<()> {
    Store::with_lock(path, || {
        let mut journal = RecoveryJournal::load(path)?;
        let shell = shell()?;
        if let Some(session) = session {
            let keys = journal
                .entries()
                .iter()
                .filter(|key| key.marker.starts_with(session))
                .cloned()
                .collect::<Vec<_>>();
            let mut failure = None;
            for key in keys {
                if let Err(error) = journal.restore_one(&key, |key| restore(&shell, key)) {
                    failure = Some(error);
                }
            }
            failure.map_or(Ok(()), Err)
        } else {
            journal.restore_all(|key| restore(&shell, key))
        }
    })
}

fn spawn_guardian(path: &Path, session: &str) -> io::Result<Child> {
    unsafe {
        let name = format!("Local\\TaskbarGroupsRecoveryReady-{}", uuid::Uuid::new_v4());
        let name_wide = wide(&name);
        let event = CreateEventW(None, true, false, PCWSTR(name_wide.as_ptr()))
            .map_err(io::Error::other)?;
        let child = Command::new(std::env::current_exe()?)
            .arg("--window-guardian")
            .arg(std::process::id().to_string())
            .arg(path)
            .arg(session)
            .arg(name)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = CloseHandle(event);
                return Err(error);
            }
        };
        let ready = WaitForSingleObject(event, 5000) == WAIT_OBJECT_0;
        let _ = CloseHandle(event);
        if !ready {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("Window recovery process did not start"));
        }
        Ok(child)
    }
}

fn retry_recovery(
    mut recover: impl FnMut() -> io::Result<()>,
    mut wait: impl FnMut(Duration),
    mut report: impl FnMut(&io::Error),
) {
    let mut attempts = 0u32;
    loop {
        match recover() {
            Ok(()) => return,
            Err(error) => {
                attempts = attempts.saturating_add(1);
                if attempts == 3 {
                    report(&error);
                }
                wait(Duration::from_secs(u64::from(attempts.min(30))));
            }
        }
    }
}

pub fn guardian(parent_id: u32, path: &Path, session: &str, ready_name: &str) -> io::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(io::Error::other)?;
        let parent =
            OpenProcess(PROCESS_SYNCHRONIZE, false, parent_id).map_err(io::Error::other)?;
        let name = wide(ready_name);
        let ready =
            CreateEventW(None, true, false, PCWSTR(name.as_ptr())).map_err(io::Error::other)?;
        SetEvent(ready).map_err(io::Error::other)?;
        let _ = CloseHandle(ready);
        let exit = WaitForSingleObject(parent, u32::MAX);
        let _ = CloseHandle(parent);
        let result = if exit == WAIT_OBJECT_0 {
            retry_recovery(
                || recover(path, Some(session)),
                thread::sleep,
                |error| {
                    let description = format!(
                        "Could not restore all taskbar buttons yet: {error}. Taskbar Groups will keep trying."
                    );
                    thread::spawn(move || {
                        let _ = rfd::MessageDialog::new()
                            .set_title("Taskbar Groups recovery")
                            .set_description(description)
                            .show();
                    });
                },
            );
            Ok(())
        } else {
            Err(io::Error::other("Could not monitor the watcher"))
        };
        CoUninitialize();
        result
    }
}

struct Monitor {
    path: PathBuf,
    session: String,
    guardian: Child,
    externally_changed: Vec<WindowKey>,
}
fn eligible_app_ids(
    store: &Store,
    accessible_groups: &std::collections::HashSet<uuid::Uuid>,
) -> std::collections::HashSet<uuid::Uuid> {
    store
        .groups
        .iter()
        .filter(|group| accessible_groups.contains(&group.id))
        .flat_map(|group| &group.apps)
        .filter(|app| app.hide_taskbar_button)
        .map(|app| app.id)
        .collect()
}
impl Monitor {
    fn new(store: &Path) -> io::Result<Self> {
        let path = journal_path(store);
        recover(&path, None)?;
        let session = format!("TaskbarGroups.Hidden.{}.", uuid::Uuid::new_v4());
        let guardian = spawn_guardian(&path, &session)?;
        Ok(Self {
            path,
            session,
            guardian,
            externally_changed: Default::default(),
        })
    }
    fn tick(&mut self, store_path: &Path) -> io::Result<()> {
        if self.guardian.try_wait()?.is_some() {
            recover(&self.path, Some(&self.session))?;
            self.guardian = spawn_guardian(&self.path, &self.session)?;
        }
        let store = match Store::load(store_path) {
            Ok(store) => store,
            Err(error) => {
                recover(&self.path, Some(&self.session))?;
                return Err(error);
            }
        };
        let accessible_groups = store
            .groups
            .iter()
            .filter(|group| crate::platform::group_has_accessible_pin(group, store.icons_per_row))
            .map(|group| group.id)
            .collect();
        let allowed = eligible_app_ids(&store, &accessible_groups);
        Store::with_lock(&self.path, || {
            let mut journal = RecoveryJournal::load(&self.path)?;
            self.externally_changed.retain(|key| {
                let current = RunningWindow {
                    hwnd: key.hwnd,
                    pid: key.pid,
                    process_started: key.process_started,
                    title: String::new(),
                };
                let marked = window_matching::window_is_same(&current)
                    && unsafe { GetPropW(hwnd(key), PCWSTR(wide(&key.marker).as_ptr())) }
                        == marker_value();
                let still_enabled = key.identity.as_ref().is_some_and(|change| {
                    allowed.contains(&change.app) && accessible_groups.contains(&change.group)
                });
                if marked && !still_enabled {
                    let _ = unsafe { RemovePropW(hwnd(key), PCWSTR(wide(&key.marker).as_ptr())) };
                }
                marked && still_enabled
            });
            let wanted = window_matching::all_group_windows(&store, journal.entries())
                .into_iter()
                .filter_map(|(id, window)| {
                    if !allowed.contains(&id) {
                        return None;
                    }
                    let group = store
                        .groups
                        .iter()
                        .find(|group| group.apps.iter().any(|app| app.id == id))?;
                    Some((id, group.id, window))
                })
                .collect::<Vec<_>>();
            let shell = shell()?;
            let old = journal
                .entries()
                .iter()
                .filter(|key| key.marker.starts_with(&self.session))
                .cloned()
                .collect::<Vec<_>>();
            for key in old {
                let current = RunningWindow {
                    hwnd: key.hwnd,
                    pid: key.pid,
                    process_started: key.process_started,
                    title: String::new(),
                };
                if let Some(change) = &key.identity
                    && window_matching::window_is_same(&current)
                    && unsafe { GetPropW(hwnd(&key), PCWSTR(wide(&key.marker).as_ptr())) }
                        == marker_value()
                    && let Some(current_identity) = window_matching::explicit_window_id(hwnd(&key))
                    && identity_changed(current_identity.as_deref(), &change.assigned)
                {
                    journal.restore_one(&key, |_| Ok(()))?;
                    if window_matching::window_is_same(&current)
                        && unsafe { GetPropW(hwnd(&key), PCWSTR(wide(&key.marker).as_ptr())) }
                            == marker_value()
                    {
                        self.externally_changed.push(key);
                    }
                    continue;
                }
                if !wanted.iter().any(|(app, group, window)| {
                    window.hwnd == key.hwnd
                        && window.pid == key.pid
                        && window.process_started == key.process_started
                        && key
                            .identity
                            .as_ref()
                            .is_some_and(|change| change.app == *app && change.group == *group)
                }) {
                    journal.restore_one(&key, |key| restore(&shell, key))?;
                }
            }
            for (app, group, window) in &wanted {
                if self.externally_changed.iter().any(|key| {
                    key.hwnd == window.hwnd
                        && key.pid == window.pid
                        && key.process_started == window.process_started
                }) {
                    continue;
                }
                if let Some(key) = journal
                    .entries()
                    .iter()
                    .find(|key| {
                        key.hwnd == window.hwnd
                            && key.pid == window.pid
                            && key.process_started == window.process_started
                    })
                    .cloned()
                {
                    let marker = wide(&key.marker);
                    if !window_matching::window_is_same(window)
                        || unsafe { GetPropW(hwnd(&key), PCWSTR(marker.as_ptr())) }
                            != marker_value()
                    {
                        journal.restore_one(&key, |key| restore(&shell, key))?;
                    }
                    continue;
                }
                if self.guardian.try_wait()?.is_some() {
                    return Err(io::Error::other("Window recovery process stopped"));
                }
                let actual_window = HWND(window.hwnd as *mut _);
                let Some(original) = window_matching::explicit_window_id(actual_window) else {
                    continue;
                };
                let assigned = taskbar_groups::app_id(group);
                let key = WindowKey {
                    hwnd: window.hwnd,
                    pid: window.pid,
                    process_started: window.process_started,
                    marker: format!("{}{}", self.session, uuid::Uuid::new_v4()),
                    identity: Some(IdentityChange {
                        app: *app,
                        group: *group,
                        original: original.clone(),
                        assigned: assigned.clone(),
                    }),
                };
                if !window_matching::window_is_same(window) {
                    continue;
                }
                let marker = wide(&key.marker);
                if unsafe { SetPropW(hwnd(&key), PCWSTR(marker.as_ptr()), Some(marker_value())) }
                    .is_err()
                {
                    continue;
                }
                if let Err(error) = journal.hide(&key, || unsafe {
                    if !window_matching::window_is_same(window)
                        || GetPropW(hwnd(&key), PCWSTR(marker.as_ptr())) != marker_value()
                        || window_matching::explicit_window_id(hwnd(&key)) != Some(original)
                    {
                        return Err(io::Error::other(
                            "Window changed before its taskbar button could be hidden",
                        ));
                    }
                    set_window_identity(hwnd(&key), Some(&assigned))
                }) {
                    if journal.entries().contains(&key) {
                        let _ = journal.restore_one(&key, |key| restore(&shell, key));
                    } else {
                        let _ = unsafe { RemovePropW(hwnd(&key), PCWSTR(marker.as_ptr())) };
                    }
                    return Err(error);
                }
            }
            Ok(())
        })
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = recover(&self.path, Some(&self.session));
        for key in &self.externally_changed {
            let current = RunningWindow {
                hwnd: key.hwnd,
                pid: key.pid,
                process_started: key.process_started,
                title: String::new(),
            };
            let marker = wide(&key.marker);
            if window_matching::window_is_same(&current)
                && unsafe { GetPropW(hwnd(key), PCWSTR(marker.as_ptr())) } == marker_value()
            {
                let _ = unsafe { RemovePropW(hwnd(key), PCWSTR(marker.as_ptr())) };
            }
        }
    }
}

pub struct MonitorThread {
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for MonitorThread {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn start(store_path: PathBuf) -> MonitorThread {
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
            return;
        }
        match Monitor::new(&store_path) {
            Ok(mut monitor) => loop {
                let _ = monitor.tick(&store_path);
                if receiver.recv_timeout(Duration::from_secs(1)).is_ok() {
                    break;
                }
            },
            Err(error) => {
                thread::spawn(move || {
                    let _ = rfd::MessageDialog::new()
                        .set_title("Taskbar Groups recovery")
                        .set_description(format!(
                            "Could not safely manage running windows: {error}"
                        ))
                        .show();
                });
            }
        }
        CoUninitialize();
    });
    MonitorThread {
        stop: sender,
        thread: Some(worker),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_restores_original_identity_without_overwriting_later_changes() {
        use windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
            },
            core::w,
        };
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Taskbar Groups identity test"),
                WINDOW_STYLE::default(),
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let marker = wide("TaskbarGroups.Test.IdentityRecovery");
            SetPropW(window, PCWSTR(marker.as_ptr()), Some(marker_value())).unwrap();
            let key = WindowKey {
                hwnd: window.0 as isize,
                pid: std::process::id(),
                process_started: window_matching::process_started(std::process::id()).unwrap(),
                marker: "TaskbarGroups.Test.IdentityRecovery".into(),
                identity: Some(IdentityChange {
                    app: uuid::Uuid::new_v4(),
                    group: uuid::Uuid::new_v4(),
                    original: Some("original.test".into()),
                    assigned: "group.test".into(),
                }),
            };
            let shell = shell().unwrap();
            set_window_identity(window, Some("group.test")).unwrap();
            restore(&shell, &key).unwrap();
            assert_eq!(
                window_matching::explicit_window_id(window),
                Some(Some("original.test".into()))
            );
            SetPropW(window, PCWSTR(marker.as_ptr()), Some(marker_value())).unwrap();
            set_window_identity(window, Some("changed.by.app")).unwrap();
            restore(&shell, &key).unwrap();
            assert_eq!(
                window_matching::explicit_window_id(window),
                Some(Some("changed.by.app".into()))
            );
            set_window_identity(window, None).unwrap();
            drop(shell);
            DestroyWindow(window).unwrap();
            CoUninitialize();
        }
    }

    #[test]
    fn restoration_does_not_overwrite_an_identity_changed_by_the_app() {
        assert!(should_restore_identity(Some("group"), "group"));
        assert!(!should_restore_identity(Some("app-changed-it"), "group"));
        assert!(!should_restore_identity(None, "group"));
    }

    #[test]
    fn clearing_a_group_identity_is_treated_as_an_external_change() {
        assert!(identity_changed(None, "group"));
        assert!(identity_changed(Some("external"), "group"));
        assert!(!identity_changed(Some("group"), "group"));
    }

    #[test]
    fn recovery_retries_a_transient_failure() {
        let mut attempts = 0;
        retry_recovery(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::other("temporary"))
                } else {
                    Ok(())
                }
            },
            |_| {},
            |_| {},
        );
        assert_eq!(attempts, 3);
    }

    #[test]
    fn apps_are_managed_only_while_their_group_is_accessible_on_taskbar() {
        let mut store = Store::default();
        let group = store.add_group("test").unwrap();
        store.add_app(group, "app", "app.exe").unwrap();
        let app = store.groups[0].apps[0].id;
        assert!(!eligible_app_ids(&store, &Default::default()).contains(&app));
        assert!(eligible_app_ids(&store, &std::collections::HashSet::from([group])).contains(&app));
        store.groups[0].apps[0].hide_taskbar_button = false;
        assert!(
            !eligible_app_ids(&store, &std::collections::HashSet::from([group])).contains(&app)
        );
    }
}
