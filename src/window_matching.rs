use std::{
    collections::{HashMap, HashSet},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use taskbar_groups::window_recovery::WindowKey;
use taskbar_groups::{App, Store};
use windows::{
    Win32::{
        Foundation::{
            APPMODEL_ERROR_NO_APPLICATION, APPMODEL_ERROR_NO_PACKAGE, CloseHandle, ERROR_SUCCESS,
            FILETIME, HWND, LPARAM,
        },
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Storage::Packaging::Appx::GetApplicationUserModelId,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IPersistFile, STGM_READ,
            },
            Threading::{
                GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            Shell::{
                IShellLinkW,
                PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow},
                ShellLink,
            },
            WindowsAndMessaging::{
                EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetWindow,
                GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
                IsWindow, IsWindowVisible, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{BOOL, GUID, Interface, PCWSTR, PWSTR},
};

#[derive(Clone, Debug)]
pub(crate) struct RunningWindow {
    pub(crate) hwnd: isize,
    pub(crate) pid: u32,
    pub(crate) title: String,
    pub(crate) process_started: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Identity {
    Executable(String),
    Package(String),
}

fn identity_can_own_window(identity: &Identity, explicit_id: Option<&str>) -> bool {
    match (identity, explicit_id) {
        (_, None) => true,
        (Identity::Package(expected), Some(actual)) => expected.eq_ignore_ascii_case(actual),
        (Identity::Executable(_), Some(_)) => false,
    }
}

// Absence of the property is different from failure to inspect it: an
// uninspectable window must not be assigned to a potentially unrelated app.
pub(crate) fn explicit_window_id(hwnd: HWND) -> Option<Option<String>> {
    use windows::Win32::Foundation::PROPERTYKEY;
    unsafe {
        let properties: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        let value = properties.GetValue(&key).ok()?;
        if value.Anonymous.Anonymous.vt.0 == 0 {
            return Some(None);
        }
        let id = windows::core::BSTR::try_from(&value).ok()?.to_string();
        Some((!id.is_empty()).then_some(id))
    }
}

fn managed_window_matches(
    key: &WindowKey,
    window: &RunningWindow,
    explicit_id: Option<&str>,
    app: uuid::Uuid,
    marker_present: bool,
) -> bool {
    marker_present
        && key.hwnd == window.hwnd
        && key.pid == window.pid
        && key.process_started == window.process_started
        && key.identity.as_ref().is_some_and(|change| {
            change.app == app
                && explicit_id.is_some_and(|id| id.eq_ignore_ascii_case(&change.assigned))
        })
}

fn marker_present(window: HWND, marker: &str) -> bool {
    use windows::Win32::{Foundation::HANDLE, UI::WindowsAndMessaging::GetPropW};
    let name: Vec<u16> = std::ffi::OsStr::new(marker)
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe { GetPropW(window, PCWSTR(name.as_ptr())) == HANDLE(std::ptr::dangling_mut()) }
}

// A shortcut with arguments may select a particular profile/document. It blocks
// executable matching for the entire store, even when another app has no arguments.
#[derive(Clone)]
struct AppIdentity {
    id: uuid::Uuid,
    identity: Identity,
    safe: bool,
}

fn unique_owner(identities: &[(uuid::Uuid, Identity)], window: &Identity) -> Option<uuid::Uuid> {
    let mut matches = identities
        .iter()
        .filter(|(_, identity)| identity == window)
        .map(|(id, _)| *id);
    let first = matches.next()?;
    matches.all(|id| id == first).then_some(first)
}

fn normalized_path(path: &Path) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    Some(path.to_string_lossy().replace('/', "\\").to_lowercase())
}

fn app_identity(app: &App) -> Option<AppIdentity> {
    let path = Path::new(&app.path);
    if path.extension()?.eq_ignore_ascii_case("lnk") {
        if let Some(package) = crate::platform::packaged_app_id(path) {
            return Some(AppIdentity {
                id: app.id,
                identity: Identity::Package(package.to_lowercase()),
                safe: true,
            });
        }
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let persist: IPersistFile = link.cast().ok()?;
            let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            persist.Load(PCWSTR(name.as_ptr()), STGM_READ).ok()?;
            let mut target = [0u16; 32768];
            link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
            let len = target.iter().position(|c| *c == 0)?;
            let target = crate::platform::msi_shortcut_target(path)
                .unwrap_or_else(|| PathBuf::from(String::from_utf16_lossy(&target[..len])));
            if !target
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
            {
                return None;
            }
            let mut args = [0u16; 32768];
            link.GetArguments(&mut args).ok()?;
            Some(AppIdentity {
                id: app.id,
                identity: Identity::Executable(normalized_path(&target)?),
                safe: args[0] == 0,
            })
        }
    } else if path.extension()?.eq_ignore_ascii_case("exe") {
        Some(AppIdentity {
            id: app.id,
            identity: Identity::Executable(normalized_path(path)?),
            safe: true,
        })
    } else {
        None
    }
}

struct ComApartment(bool);
impl ComApartment {
    fn new() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}

unsafe extern "system" fn collect_window(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        (&mut *(param.0 as *mut Vec<isize>)).push(hwnd.0 as isize);
    }
    BOOL(1)
}

fn creation_time(process: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
    let started = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    (started != 0).then_some(started)
}

pub(crate) fn process_started(pid: u32) -> Option<u64> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let result = creation_time(process);
    let _ = unsafe { CloseHandle(process) };
    result
}

fn process_details(pid: u32) -> Option<(u64, Identity)> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let result = (|| {
            let started = creation_time(process)?;
            let mut package = [0u16; 512];
            let mut size = package.len() as u32;
            let package_status =
                GetApplicationUserModelId(process, &mut size, Some(PWSTR(package.as_mut_ptr())));
            let identity = if package_status == ERROR_SUCCESS {
                let len = package.iter().position(|c| *c == 0)?;
                Identity::Package(String::from_utf16_lossy(&package[..len]).to_lowercase())
            } else if package_status == APPMODEL_ERROR_NO_APPLICATION
                || package_status == APPMODEL_ERROR_NO_PACKAGE
            {
                let mut path = [0u16; 32768];
                let mut size = path.len() as u32;
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(path.as_mut_ptr()),
                    &mut size,
                )
                .ok()?;
                let path = PathBuf::from(String::from_utf16_lossy(&path[..size as usize]));
                // Shared shell hosts do not establish ownership of their windows.
                let file = path.file_name()?.to_string_lossy();
                if [
                    "applicationframehost.exe",
                    "explorer.exe",
                    "shellexperiencehost.exe",
                    "runtimebroker.exe",
                    "svchost.exe",
                ]
                .iter()
                .any(|name| file.eq_ignore_ascii_case(name))
                {
                    return None;
                }
                Identity::Executable(normalized_path(&path)?)
            } else {
                return None;
            };
            Some((started, identity))
        })();
        let _ = CloseHandle(process);
        result
    }
}

fn eligible_style(style: u32, owned: bool) -> bool {
    style & WS_EX_TOOLWINDOW.0 == 0 && !owned
}

fn running_windows() -> Vec<(RunningWindow, Identity, Option<String>)> {
    let mut handles = Vec::<isize>::new();
    if unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM((&mut handles as *mut Vec<isize>) as isize),
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    let mut processes = HashMap::new();
    handles
        .into_iter()
        .filter_map(|raw| unsafe {
            let hwnd = HWND(raw as *mut _);
            if !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd {
                return None;
            }
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if !eligible_style(
                style,
                GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null()),
            ) {
                return None;
            }
            let mut cloaked = 0u32;
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
            .is_err()
                || cloaked != 0
            {
                return None;
            }
            let length = GetWindowTextLengthW(hwnd);
            if length <= 0 {
                return None;
            }
            let mut title = vec![0u16; length as usize + 1];
            let length = GetWindowTextW(hwnd, &mut title);
            if length <= 0 {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return None;
            }
            let (process_started, identity) = processes
                .entry(pid)
                .or_insert_with(|| process_details(pid))
                .clone()?;
            let explicit_id = explicit_window_id(hwnd)?;
            Some((
                RunningWindow {
                    hwnd: raw,
                    pid,
                    title: String::from_utf16_lossy(&title[..length as usize]),
                    process_started,
                },
                identity,
                explicit_id,
            ))
        })
        .collect()
}

fn allowed_identities(candidates: &[AppIdentity]) -> Vec<(uuid::Uuid, Identity)> {
    let blocked: HashSet<_> = candidates
        .iter()
        .filter(|candidate| !candidate.safe)
        .map(|candidate| candidate.identity.clone())
        .collect();
    candidates
        .iter()
        .filter(|candidate| candidate.safe && !blocked.contains(&candidate.identity))
        .map(|candidate| (candidate.id, candidate.identity.clone()))
        .collect()
}

fn matched_windows(
    apps: impl IntoIterator<Item = App>,
    journal: &[WindowKey],
) -> Vec<(uuid::Uuid, RunningWindow)> {
    let _com = ComApartment::new();
    let candidates: Vec<_> = apps
        .into_iter()
        .filter_map(|app| app_identity(&app))
        .collect();
    let allowed = allowed_identities(&candidates);
    running_windows()
        .into_iter()
        .filter_map(|(window, identity, explicit)| {
            let id = unique_owner(&allowed, &identity)?;
            let hwnd = HWND(window.hwnd as *mut _);
            (identity_can_own_window(&identity, explicit.as_deref())
                || journal.iter().any(|key| {
                    managed_window_matches(
                        key,
                        &window,
                        explicit.as_deref(),
                        id,
                        marker_present(hwnd, &key.marker),
                    )
                }))
            .then_some((id, window))
        })
        .collect()
}

pub(crate) fn all_group_windows(
    store: &Store,
    journal: &[WindowKey],
) -> Vec<(uuid::Uuid, RunningWindow)> {
    matched_windows(
        store
            .groups
            .iter()
            .flat_map(|group| group.apps.iter().cloned()),
        journal,
    )
}

pub(crate) fn activate(window: &RunningWindow) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    if !window_is_same(window) {
        return Err("That window has closed.".into());
    }
    unsafe {
        let hwnd = HWND(window.hwnd as *mut _);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            return Err("Windows could not bring that window to the foreground.".into());
        }
    }
    Ok(())
}
pub(crate) fn window_is_same(window: &RunningWindow) -> bool {
    unsafe {
        let hwnd = HWND(window.hwnd as *mut _);
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == window.pid
            && process_started(pid).is_some_and(|started| started == window.process_started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regrouped_window_is_recognized_only_with_its_record_and_marker() {
        let window = RunningWindow {
            hwnd: 21,
            pid: 22,
            process_started: 23,
            title: "Window".into(),
        };
        let app = uuid::Uuid::from_u128(1);
        let key = taskbar_groups::window_recovery::WindowKey {
            hwnd: 21,
            pid: 22,
            process_started: 23,
            marker: "marker".into(),
            identity: Some(taskbar_groups::window_recovery::IdentityChange {
                app,
                group: uuid::Uuid::from_u128(2),
                original: None,
                assigned: "group-id".into(),
            }),
        };
        assert!(managed_window_matches(
            &key,
            &window,
            Some("group-id"),
            app,
            true
        ));
        assert!(!managed_window_matches(
            &key,
            &window,
            Some("group-id"),
            app,
            false
        ));
        assert!(!managed_window_matches(
            &key,
            &window,
            Some("other-id"),
            app,
            true
        ));
        assert!(!managed_window_matches(
            &key,
            &window,
            Some("group-id"),
            uuid::Uuid::from_u128(3),
            true
        ));
    }
    #[test]
    fn a_distinct_window_app_id_does_not_inherit_executable_ownership() {
        let exe = Identity::Executable("c:/apps/browser.exe".into());
        assert!(identity_can_own_window(&exe, None));
        assert!(!identity_can_own_window(&exe, Some("browser.webapp")));
        let package = Identity::Package("family!main".into());
        assert!(identity_can_own_window(&package, Some("family!main")));
        assert!(!identity_can_own_window(&package, Some("family!other")));
    }
    #[test]
    fn argument_shortcut_blocks_same_executable_even_for_argumentless_alias() {
        let exe = Identity::Executable("c:/apps/editor.exe".into());
        let candidates = vec![
            AppIdentity {
                id: uuid::Uuid::from_u128(1),
                identity: exe.clone(),
                safe: true,
            },
            AppIdentity {
                id: uuid::Uuid::from_u128(2),
                identity: exe,
                safe: false,
            },
        ];
        assert!(allowed_identities(&candidates).is_empty());
    }
    #[test]
    fn owned_app_windows_are_not_eligible() {
        assert!(!eligible_style(
            windows::Win32::UI::WindowsAndMessaging::WS_EX_APPWINDOW.0,
            true
        ));
        assert!(eligible_style(0, false));
        assert!(!eligible_style(WS_EX_TOOLWINDOW.0, false));
    }
    #[test]
    fn exact_executable_matches_one_owner() {
        let app = uuid::Uuid::from_u128(1);
        assert_eq!(
            unique_owner(
                &[(app, Identity::Executable("c:/apps/editor.exe".into()))],
                &Identity::Executable("c:/apps/editor.exe".into())
            ),
            Some(app)
        );
    }
    #[test]
    fn shared_executable_does_not_choose_between_apps() {
        let identities = vec![
            (
                uuid::Uuid::from_u128(1),
                Identity::Executable("c:/apps/editor.exe".into()),
            ),
            (
                uuid::Uuid::from_u128(2),
                Identity::Executable("c:/apps/editor.exe".into()),
            ),
        ];
        assert_eq!(
            unique_owner(
                &identities,
                &Identity::Executable("c:/apps/editor.exe".into())
            ),
            None
        );
    }
    #[test]
    fn package_identity_must_match_entire_id() {
        let identities = vec![(
            uuid::Uuid::from_u128(1),
            Identity::Package("family!first".into()),
        )];
        assert_eq!(
            unique_owner(&identities, &Identity::Package("family!second".into())),
            None
        );
        assert_eq!(
            unique_owner(&identities, &Identity::Package("family!first".into())),
            Some(uuid::Uuid::from_u128(1))
        );
    }
}
