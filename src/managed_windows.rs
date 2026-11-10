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
    window_recovery::{RecoveryJournal, WindowKey},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Threading::{
                CreateEventW, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{ITaskbarList, TaskbarList},
            WindowsAndMessaging::{
                GetPropW, GetWindowThreadProcessId, IsWindow, RemovePropW, SetPropW,
            },
        },
    },
    core::PCWSTR,
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
        shell.AddTab(window).map_err(io::Error::other)?;
        let _ = RemovePropW(window, PCWSTR(marker.as_ptr()));
        Ok(())
    }
}
fn journal_path(store_path: &Path) -> PathBuf {
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
        let wanted = window_matching::all_group_windows(&store)
            .into_iter()
            .filter_map(|(id, window)| allowed.contains(&id).then_some(window))
            .collect::<Vec<_>>();
        Store::with_lock(&self.path, || {
            let mut journal = RecoveryJournal::load(&self.path)?;
            let shell = shell()?;
            let old = journal
                .entries()
                .iter()
                .filter(|key| key.marker.starts_with(&self.session))
                .cloned()
                .collect::<Vec<_>>();
            for key in old {
                if !wanted.iter().any(|window| {
                    window.hwnd == key.hwnd
                        && window.pid == key.pid
                        && window.process_started == key.process_started
                }) {
                    journal.restore_one(&key, |key| restore(&shell, key))?;
                }
            }
            for window in &wanted {
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
                    if window_matching::window_is_same(window)
                        && unsafe { GetPropW(hwnd(&key), PCWSTR(marker.as_ptr())) }
                            == marker_value()
                    {
                        unsafe {
                            shell.DeleteTab(hwnd(&key)).map_err(io::Error::other)?;
                        }
                    } else {
                        journal.restore_one(&key, |key| restore(&shell, key))?;
                    }
                    continue;
                }
                if self.guardian.try_wait()?.is_some() {
                    return Err(io::Error::other("Window recovery process stopped"));
                }
                let key = WindowKey {
                    hwnd: window.hwnd,
                    pid: window.pid,
                    process_started: window.process_started,
                    marker: format!("{}{}", self.session, uuid::Uuid::new_v4()),
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
                    {
                        return Err(io::Error::other(
                            "Window changed before its taskbar button could be hidden",
                        ));
                    }
                    shell.DeleteTab(hwnd(&key)).map_err(io::Error::other)
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
