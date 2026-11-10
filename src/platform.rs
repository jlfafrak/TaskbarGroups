use crate::drag_ghost::DragGhost;
use std::{
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{OnceLock, mpsc},
    time::{Duration, Instant},
};
use taskbar_groups::{Group, Store, app_id, group_for_automation_id, shortcut_title};
use windows::{
    Win32::{
        Foundation::{
            COLORREF, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, ERROR_ALREADY_EXISTS,
            GetLastError, HWND, LPARAM, LRESULT, POINT, PROPERTYKEY, SIZE, WPARAM,
        },
        Graphics::Gdi::*,
        Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        System::{
            ApplicationInstallationAndServicing::{
                INSTALLSTATE_LOCAL, INSTALLSTATE_SOURCE, MsiGetComponentPathW,
                MsiGetShortcutTargetW,
            },
            Com::{StructuredStorage::PROPVARIANT, *},
            Ole::{
                DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, DoDragDrop,
                IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
            Threading::CreateMutexW,
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationTreeWalker, TreeScope_Descendants,
            },
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, VK_ESCAPE, VK_LBUTTON},
            Shell::{PropertiesSystem::IPropertyStore, *},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, GUID, Interface, PCSTR, PCWSTR, PWSTR, implement, w},
};

type MenuHandler = (isize, Option<IContextMenu3>, Option<IContextMenu2>);
thread_local! {
    static MENU_HANDLER: std::cell::RefCell<Option<MenuHandler>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn menu_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    MENU_HANDLER.with(|slot| {
        let Ok(handler) = slot.try_borrow() else {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        };
        let Some((previous, menu3, menu2)) = handler.as_ref() else {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        };
        if matches!(
            msg,
            WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
        ) {
            let mut result = LRESULT(0);
            if let Some(menu) = menu3 {
                if unsafe { menu.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
                    return result;
                }
            } else if let Some(menu) = menu2
                && unsafe { menu.HandleMenuMsg(msg, wparam, lparam) }.is_ok()
            {
                return LRESULT(0);
            }
        }
        let proc = unsafe { std::mem::transmute::<isize, WNDPROC>(*previous) };
        unsafe { CallWindowProcW(proc, hwnd, msg, wparam, lparam) }
    })
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

struct ShutdownSignal(isize);
impl ShutdownSignal {
    fn new(name: &str) -> Result<Self, String> {
        use windows::Win32::System::Threading::CreateEventW;
        let name = wide(name);
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR(name.as_ptr())) }
            .map_err(|error| error.to_string())?;
        Ok(Self(handle.0 as isize))
    }
    fn is_set(&self) -> bool {
        use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(HANDLE(self.0 as *mut _), 0) == WAIT_OBJECT_0 }
    }
    fn trigger(&self) -> Result<(), String> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Threading::SetEvent;
        unsafe { SetEvent(HANDLE(self.0 as *mut _)) }.map_err(|error| error.to_string())
    }
    fn reset(&self) -> Result<(), String> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Threading::ResetEvent;
        unsafe { ResetEvent(HANDLE(self.0 as *mut _)) }.map_err(|error| error.to_string())
    }
}
impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        let _ = unsafe { CloseHandle(HANDLE(self.0 as *mut _)) };
    }
}

fn shutdown_signal() -> Option<&'static ShutdownSignal> {
    use std::hash::{Hash, Hasher};
    static SIGNAL: OnceLock<Option<ShutdownSignal>> = OnceLock::new();
    SIGNAL
        .get_or_init(|| {
            let path = std::env::current_exe()
                .ok()?
                .to_string_lossy()
                .to_lowercase();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut hasher);
            ShutdownSignal::new(&format!(
                "Local\\TaskbarGroupsInstallerShutdown-{:016x}",
                hasher.finish()
            ))
            .ok()
        })
        .as_ref()
}

pub fn initialize_installer_shutdown() {
    let _ = shutdown_signal();
}
pub fn installer_shutdown_requested() -> bool {
    shutdown_signal().is_some_and(ShutdownSignal::is_set)
}
pub fn request_installer_shutdown() -> Result<(), String> {
    let signal = shutdown_signal().ok_or("Could not create the installer shutdown signal")?;
    signal.trigger()?;
    let result = wait_for_other_instances(
        &std::env::current_exe().map_err(|e| e.to_string())?,
        Duration::from_secs(30),
    );
    if result.is_err() {
        let _ = signal.reset();
    }
    result
}

fn wait_for_other_instances(executable: &Path, timeout: Duration) -> Result<(), String> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
    };
    let expected_name = executable
        .file_name()
        .ok_or("Executable has no filename")?
        .to_string_lossy();
    let expected_path = executable.to_string_lossy();
    let started = Instant::now();
    let mut current_session = 0;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session) }
        .map_err(|e| e.to_string())?;
    loop {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| e.to_string())?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut other = false;
        let mut result = unsafe { Process32FirstW(snapshot, &mut entry) };
        let mut failure = None;
        while result.is_ok() {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if entry.th32ProcessID != unsafe { GetCurrentProcessId() }
                && name.eq_ignore_ascii_case(&expected_name)
            {
                match unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        entry.th32ProcessID,
                    )
                } {
                    Ok(process) => {
                        let mut buffer = [0u16; 32768];
                        let mut size = buffer.len() as u32;
                        let query = unsafe {
                            QueryFullProcessImageNameW(
                                process,
                                PROCESS_NAME_WIN32,
                                PWSTR(buffer.as_mut_ptr()),
                                &mut size,
                            )
                        };
                        if query.is_ok()
                            && String::from_utf16_lossy(&buffer[..size as usize])
                                .eq_ignore_ascii_case(&expected_path)
                        {
                            other |= unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0;
                        } else if query.is_err()
                            && unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0
                        {
                            failure = Some(
                                "Could not verify a running Taskbar Groups process".to_string(),
                            );
                        }
                        let _ = unsafe { CloseHandle(process) };
                    }
                    Err(e) if unsafe { GetLastError() } != ERROR_INVALID_PARAMETER => {
                        let mut session = 0;
                        if unsafe { ProcessIdToSessionId(entry.th32ProcessID, &mut session) }
                            .is_ok()
                            && session == current_session
                        {
                            failure = Some(e.to_string());
                        }
                    }
                    _ => {}
                }
            }
            if failure.is_some() {
                break;
            }
            result = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        if failure.is_none() && result.is_err() && unsafe { GetLastError() } != ERROR_NO_MORE_FILES
        {
            failure = Some("Could not enumerate running processes".into());
        }
        let _ = unsafe { CloseHandle(snapshot) };
        if let Some(error) = failure {
            return Err(error);
        }
        if !other {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(
                "Taskbar Groups is still busy. Finish the current drag and retry the installation."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
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
            unsafe {
                CoUninitialize();
            }
        }
    }
}

pub fn enable_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn color_from_argb(value: u32) -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn color_from_colorref(value: u32) -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8)
}

fn color_to_colorref(color: eframe::egui::Color32) -> u32 {
    color.r() as u32 | (color.g() as u32) << 8 | (color.b() as u32) << 16
}

pub fn windows_theme() -> crate::theme::SystemTheme {
    use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    unsafe {
        let personalize = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let read_value = |name: PCWSTR| {
            let mut value = 0u32;
            let mut size = std::mem::size_of::<u32>() as u32;
            let result = RegGetValueW(
                HKEY_CURRENT_USER,
                personalize,
                name,
                RRF_RT_REG_DWORD,
                None,
                Some((&mut value as *mut u32).cast()),
                Some(&mut size),
            );
            (result == windows::Win32::Foundation::ERROR_SUCCESS
                && size == std::mem::size_of::<u32>() as u32)
                .then_some(value)
        };
        let dark = read_value(w!("SystemUsesLightTheme")).unwrap_or(1) == 0;
        let accent_on_taskbar = read_value(w!("ColorPrevalence")).unwrap_or(0) != 0;
        let mut argb = 0xff0078d7;
        let mut opaque = BOOL::default();
        let _ = DwmGetColorizationColor(&mut argb, &mut opaque);
        let mut high_contrast = HIGHCONTRASTW {
            cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        let contrast = if SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.cbSize,
            Some((&mut high_contrast as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && high_contrast.dwFlags.contains(HCF_HIGHCONTRASTON)
        {
            Some(crate::theme::ContrastColors {
                background: color_from_colorref(GetSysColor(COLOR_WINDOW)),
                text: color_from_colorref(GetSysColor(COLOR_WINDOWTEXT)),
                surface: color_from_colorref(GetSysColor(COLOR_BTNFACE)),
                accent: color_from_colorref(GetSysColor(COLOR_HIGHLIGHT)),
                accent_text: color_from_colorref(GetSysColor(COLOR_HIGHLIGHTTEXT)),
            })
        } else {
            None
        };
        crate::theme::SystemTheme {
            dark,
            accent: color_from_argb(argb),
            accent_on_taskbar,
            contrast,
        }
    }
}

pub fn set_titlebar_theme(window: isize, palette: crate::theme::Palette) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };
    unsafe {
        let dark = BOOL::from(palette.dark);
        let hwnd = HWND(window as *mut std::ffi::c_void);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const BOOL).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
        let caption = color_to_colorref(palette.header);
        let text = color_to_colorref(palette.text);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&caption as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            (&text as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

fn shell_item_for_path(
    path: &Path,
) -> Result<*mut windows::Win32::UI::Shell::Common::ITEMIDLIST, String> {
    if !path.is_file() {
        return Err(format!("Shortcut not found: {}", path.display()));
    }
    let path: PathBuf = path.components().collect();
    let name = wide(path.as_os_str());
    let mut pidl = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut pidl, 0, None) }
        .map_err(|e| e.to_string())?;
    Ok(pidl)
}

fn shell_drag_data_object(path: &Path) -> Result<IDataObject, String> {
    unsafe {
        let pidl = if let Some(id) = packaged_app_id(path) {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(
                PCWSTR(wide(format!("shell:AppsFolder\\{id}")).as_ptr()),
                None,
                &mut pidl,
                0,
                None,
            )
            .map_err(|e| e.to_string())?;
            pidl
        } else {
            shell_item_for_path(path)?
        };
        let result = (|| {
            let mut child = std::ptr::null_mut();
            let folder: IShellFolder =
                SHBindToParent(pidl, Some(&mut child)).map_err(|e| e.to_string())?;
            folder
                .GetUIObjectOf(HWND::default(), &[child], None)
                .map_err(|e| e.to_string())
        })();
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

fn named_drag_shortcut(source: &Path, name: &str, base: &Path) -> Result<PathBuf, String> {
    let name: String = name
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_end_matches('.').trim();
    let reserved = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let name = if name.is_empty()
        || matches!(
            reserved.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
        "Application"
    } else {
        name
    };
    let directory = base.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let destination = directory.join(format!("{name}.lnk"));
    if let Err(error) = std::fs::copy(source, &destination) {
        let _ = std::fs::remove_dir(directory);
        return Err(error.to_string());
    }
    Ok(destination)
}

fn discard_cancelled_shortcut(staged: &Path) {
    if std::fs::remove_file(staged).is_ok()
        && let Some(parent) = staged.parent()
    {
        let _ = std::fs::remove_dir(parent);
    }
}

fn add_drag_image(data: &IDataObject, path: &Path) -> Result<(), String> {
    let image = icon(&path.to_string_lossy()).ok_or("Could not load app icon")?;
    if image.size != [32, 32] {
        return Err("Unexpected app icon dimensions".into());
    }
    unsafe {
        let helper: IDragSourceHelper =
            CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.to_string())?;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 32,
                biHeight: -32,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, 32 * 32 * 4);
        for (output, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(&image.pixels) {
            let [r, g, b, a] = pixel.to_srgba_unmultiplied();
            output.copy_from_slice(&[b, g, r, a]);
        }
        let drag = SHDRAGIMAGE {
            sizeDragImage: SIZE { cx: 32, cy: 32 },
            ptOffset: POINT { x: 4, y: 4 },
            hbmpDragImage: bitmap,
            crColorKey: COLORREF(u32::MAX),
        };
        let result = helper
            .InitializeFromBitmap(&drag, data)
            .map_err(|e| e.to_string());
        if result.is_err() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        result
    }
}
fn drag_effect_is_accepted(effect: DROPEFFECT, on_taskbar: bool) -> bool {
    on_taskbar && effect.0 & 7 != 0
}

pub(crate) fn packaged_app_id(path: &Path) -> Option<String> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
    {
        return None;
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist
            .Load(PCWSTR(wide(path.as_os_str()).as_ptr()), STGM_READ)
            .ok()?;
        let pidl = link.GetIDList().ok()?;
        let item: windows::core::Result<IShellItem> = SHCreateItemFromIDList(pidl);
        CoTaskMemFree(Some(pidl.cast()));
        let name = item
            .ok()?
            .GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)
            .ok()?;
        let result = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        result.filter(|id| {
            id.split_once('!').is_some_and(|(family, app)| {
                !family.is_empty() && !app.is_empty() && !id.contains(['\\', '/', ':'])
            })
        })
    }
}

pub fn cursor_on_taskbar() -> bool {
    unsafe {
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_err() {
            return false;
        }
        let root = GetAncestor(WindowFromPoint(point), GA_ROOT);
        let mut class = [0u16; 128];
        let len = GetClassNameW(root, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd"
    }
}

#[implement(IDropSource)]
struct TaskbarDropSource;
impl IDropSource_Impl for TaskbarDropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> windows::core::HRESULT {
        taskbar_drag_decision(
            escape.as_bool(),
            keys.0 & MK_LBUTTON.0 != 0,
            cursor_on_taskbar(),
        )
    }
    fn GiveFeedback(&self, effect: DROPEFFECT) -> windows::core::HRESULT {
        unsafe {
            let cursor = if drag_effect_is_accepted(effect, cursor_on_taskbar()) {
                IDC_ARROW
            } else {
                IDC_NO
            };
            if let Ok(cursor) = LoadCursorW(None, cursor) {
                SetCursor(Some(cursor));
                windows::core::HRESULT(0)
            } else {
                DRAGDROP_S_USEDEFAULTCURSORS
            }
        }
    }
}

pub fn drag_app_to_taskbar(path: &Path, name: &str) -> Result<bool, String> {
    unsafe {
        OleInitialize(None)
            .map_err(|e| format!("Could not initialize Windows drag and drop: {e}"))?;
        let mut staged = None;
        let mut accepted = false;
        let result = (|| {
            let before = capture_pin_baseline(path);
            let drag_path = if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
                && packaged_app_id(path).is_none()
            {
                let root = PathBuf::from(
                    std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?,
                )
                .join("TaskbarGroups/exported-shortcuts");
                staged = Some(named_drag_shortcut(path, name, &root)?);
                staged.as_deref().unwrap_or(path)
            } else {
                path
            };
            let data = shell_drag_data_object(drag_path)?;
            let _ = add_drag_image(&data, path);
            let source: IDropSource = TaskbarDropSource.into();
            let mut effect = DROPEFFECT_NONE;
            let _ = ReleaseCapture();
            let outcome = DoDragDrop(
                &data,
                &source,
                DROPEFFECT_COPY | DROPEFFECT_LINK,
                &mut effect,
            );
            if outcome.is_err() {
                return Err(format!("Windows could not complete the drag: {outcome:?}"));
            }
            if outcome != DRAGDROP_S_DROP || effect == DROPEFFECT_NONE {
                return Ok(false);
            }
            accepted = true;
            if verify_taskbar_pin(path, &before) {
                Ok(true)
            } else {
                Err(
                    "Windows did not confirm that this app was pinned. It remains in the group."
                        .into(),
                )
            }
        })();
        if !accepted && let Some(staged) = staged {
            discard_cancelled_shortcut(&staged);
        }
        OleUninitialize();
        result
    }
}

fn taskbar_drag_decision(
    escape: bool,
    button_down: bool,
    on_taskbar: bool,
) -> windows::core::HRESULT {
    use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP};
    if escape || (!button_down && !on_taskbar) {
        DRAGDROP_S_CANCEL
    } else if !button_down {
        DRAGDROP_S_DROP
    } else {
        windows::core::HRESULT(0)
    }
}

fn same_launch_target(first: &Path, second: &Path) -> bool {
    if first
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        && second
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
    {
        let ids = (shortcut_app_id(first), shortcut_app_id(second));
        match ids {
            (Ok(a), Ok(b)) => {
                let a = a.filter(|id| !id.is_empty());
                let b = b.filter(|id| !id.is_empty());
                match (a, b) {
                    (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => {}
                    (None, None) => {}
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    fn target(path: &Path) -> Option<(String, String, Option<String>)> {
        unsafe {
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            {
                let target = std::fs::canonicalize(path).ok()?;
                return Some((target.to_string_lossy().to_lowercase(), String::new(), None));
            }
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let persist: IPersistFile = link.cast().ok()?;
            persist
                .Load(PCWSTR(wide(path.as_os_str()).as_ptr()), STGM_READ)
                .ok()?;
            let mut filename = [0u16; 32768];
            link.GetPath(&mut filename, std::ptr::null_mut(), 0).ok()?;
            let len = filename.iter().position(|&c| c == 0)?;
            let resolved = msi_shortcut_target(path)
                .unwrap_or_else(|| PathBuf::from(String::from_utf16_lossy(&filename[..len])));
            let target = std::fs::canonicalize(resolved).ok()?;
            let mut arguments = [0u16; 32768];
            link.GetArguments(&mut arguments).ok()?;
            let len = arguments.iter().position(|&c| c == 0)?;
            let mut working = [0u16; 32768];
            link.GetWorkingDirectory(&mut working).ok()?;
            let working_len = working.iter().position(|&c| c == 0)?;
            let working = String::from_utf16_lossy(&working[..working_len])
                .trim_end_matches(['\\', '/'])
                .to_lowercase();
            Some((
                target.to_string_lossy().to_lowercase(),
                String::from_utf16_lossy(&arguments[..len]),
                Some(working),
            ))
        }
    }
    target(first).zip(target(second)).is_some_and(|(a, b)| {
        a.0 == b.0 && a.1 == b.1 && (a.2.is_none() || b.2.is_none() || a.2 == b.2)
    })
}

fn pinned_shortcut_matches_visible_name(
    buttons: &[TaskbarButton],
    pinned: &Path,
    source: &Path,
    before: &std::collections::HashSet<PathBuf>,
    previous_identities: &std::collections::HashSet<String>,
) -> bool {
    let Ok(entries) = std::fs::read_dir(pinned) else {
        return false;
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        if before.contains(&candidate) {
            continue;
        }
        if !candidate
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        {
            continue;
        }
        let Some(name) = candidate.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let matches: std::collections::HashSet<_> = buttons
            .iter()
            .filter(|button| {
                button.name.to_ascii_lowercase().ends_with(" pinned")
                    && source_label(&button.name).eq_ignore_ascii_case(name)
            })
            .map(|button| button.identity.to_lowercase())
            .collect();
        if matches.len() != 1
            || matches
                .iter()
                .any(|identity| previous_identities.contains(identity))
        {
            continue;
        }
        if same_launch_target(source, &candidate) {
            return true;
        }
    }
    false
}

fn matching_pin_paths(
    pinned: &Path,
    source: &Path,
) -> std::io::Result<std::collections::HashSet<PathBuf>> {
    let entries = match std::fs::read_dir(pinned) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error),
    };
    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
                && same_launch_target(source, path)
        })
        .collect())
}
fn matching_new_pin_exists(
    pinned: &Path,
    source: &Path,
    before: &std::collections::HashSet<PathBuf>,
) -> bool {
    matching_pin_paths(pinned, source)
        .is_ok_and(|after| after.iter().any(|path| !before.contains(path)))
}

fn has_one_new_pinned_identity(
    before: &std::collections::HashSet<String>,
    buttons: &[TaskbarButton],
) -> bool {
    let after: std::collections::HashSet<_> = buttons
        .iter()
        .filter(|button| button.name.to_ascii_lowercase().ends_with(" pinned"))
        .map(|button| button.identity.to_lowercase())
        .filter(|identity| !before.contains(identity))
        .collect();
    after.len() == 1
}

fn new_pin_is_registered(
    pinned: &Path,
    source: &Path,
    before: &PinBaseline,
    buttons: &[TaskbarButton],
) -> bool {
    let Some(previous) = &before.matching_paths else {
        return false;
    };
    let Some(previous_identities) = &before.pinned_identities else {
        return false;
    };
    if !matching_new_pin_exists(pinned, source, previous) {
        return false;
    }
    has_one_new_pinned_identity(previous_identities, buttons)
        && pinned_shortcut_matches_visible_name(
            buttons,
            pinned,
            source,
            previous,
            previous_identities,
        )
}

fn taskbar_pin_directory() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join("Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar"),
    )
}

struct PinBaseline {
    matching_paths: Option<std::collections::HashSet<PathBuf>>,
    pinned_identities: Option<std::collections::HashSet<String>>,
}

fn taskbar_snapshots(automation: &IUIAutomation) -> Option<Vec<TaskbarSnapshot>> {
    unsafe {
        let primary = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
        let mut snapshots = vec![snapshot_taskbar(automation, primary)?];
        let mut last = None;
        while let Ok(root) = FindWindowExW(None, last, w!("Shell_SecondaryTrayWnd"), None) {
            snapshots.push(snapshot_taskbar(automation, root)?);
            last = Some(root);
        }
        Some(snapshots)
    }
}

fn capture_pin_baseline(path: &Path) -> PinBaseline {
    let matching_paths =
        taskbar_pin_directory().and_then(|dir| matching_pin_paths(&dir, path).ok());
    let pinned_identities = unsafe {
        let automation: windows::core::Result<IUIAutomation> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
        automation
            .ok()
            .and_then(|automation| taskbar_snapshots(&automation))
            .map(|snapshots| {
                snapshots
                    .into_iter()
                    .flat_map(|snapshot| snapshot.buttons)
                    .filter(|button| button.name.to_ascii_lowercase().ends_with(" pinned"))
                    .map(|button| button.identity.to_lowercase())
                    .collect()
            })
    };
    PinBaseline {
        matching_paths,
        pinned_identities,
    }
}

fn verify_taskbar_pin(path: &Path, before: &PinBaseline) -> bool {
    unsafe {
        let Ok(automation): windows::core::Result<IUIAutomation> =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        let packaged = packaged_app_id(path);
        let Some(pinned_dir) = taskbar_pin_directory() else {
            return false;
        };
        for _ in 0..10 {
            let Some(snapshots) = taskbar_snapshots(&automation) else {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            };
            if packaged.is_none() {
                let buttons: Vec<_> = snapshots
                    .iter()
                    .flat_map(|snapshot| snapshot.buttons.iter().cloned())
                    .collect();
                if new_pin_is_registered(&pinned_dir, path, before, &buttons) {
                    return true;
                }
            }
            for snapshot in snapshots {
                for button in snapshot.buttons {
                    if !button.name.to_ascii_lowercase().ends_with(" pinned") {
                        continue;
                    }
                    if let Some(id) = &packaged {
                        if id.eq_ignore_ascii_case(&button.identity) {
                            return true;
                        }
                    } else if let Ok(Some(pinned)) =
                        pinned_shortcut_for_app_id(&pinned_dir, &button.identity)
                        && same_launch_target(path, &pinned)
                    {
                        return true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        false
    }
}

pub fn reveal_shortcut(path: &Path) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let pidl = shell_item_for_path(path)?;
        let result = SHOpenFolderAndSelectItems(pidl, None, 0).map_err(|e| e.to_string());
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

pub fn acquire_manager() -> bool {
    unsafe {
        match CreateMutexW(None, false, w!("Local\\TaskbarGroupsManager")) {
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => false,
        }
    }
}

pub fn mark_manager_window(window: isize) {
    unsafe {
        let window = HWND(window as *mut std::ffi::c_void);
        let _ = SetPropW(
            window,
            w!("TaskbarGroups.Manager.v1"),
            Some(windows::Win32::Foundation::HANDLE(window.0)),
        );
    }
}

pub fn focus_manager() -> bool {
    unsafe {
        let mut previous = None;
        let window = loop {
            let Ok(window) = FindWindowExW(None, previous, None, w!("Taskbar Groups")) else {
                return false;
            };
            if GetPropW(window, w!("TaskbarGroups.Manager.v1")).0 == window.0 {
                break window;
            }
            previous = Some(window);
        };
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let Ok(process) = windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        ) else {
            return false;
        };
        let mut name = [0u16; 32768];
        let mut size = name.len() as u32;
        let result = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            process,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            PWSTR(name.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(process);
        let expected = std::env::current_exe().ok();
        let actual = PathBuf::from(String::from_utf16_lossy(&name[..size as usize]));
        if result.is_err()
            || !expected.is_some_and(|path| {
                path.to_string_lossy()
                    .eq_ignore_ascii_case(&actual.to_string_lossy())
            })
        {
            return false;
        }
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let _ = SetForegroundWindow(window);
        true
    }
}

pub fn open_manager() -> Result<(), String> {
    if focus_manager() {
        return Ok(());
    }
    Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn startup_path() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?)
            .join("Microsoft/Windows/Start Menu/Programs/Startup/Taskbar Groups.lnk"),
    )
}

fn startup_enabled_at(path: &Path, approval: Option<&[u8]>) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
        _ => {}
    }
    match approval {
        None => Ok(true),
        Some([2 | 6, ..]) => Ok(true),
        Some([3 | 7, ..]) => Ok(false),
        Some(_) => Err("Windows startup approval has an unrecognized value".into()),
    }
}

fn startup_approval() -> Result<Option<Vec<u8>>, String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
    let mut data = [0u8; 256];
    let mut size = data.len() as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder"
            ),
            w!("Taskbar Groups.lnk"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        Ok(None)
    } else if result == ERROR_SUCCESS {
        Ok(Some(data[..size as usize].to_vec()))
    } else {
        Err(format!(
            "Could not read Windows startup approval: {}",
            std::io::Error::from_raw_os_error(result.0 as i32)
        ))
    }
}

pub fn startup_enabled() -> Result<bool, String> {
    let path = startup_path()?;
    if !startup_enabled_at(&path, None)? {
        return Ok(false);
    }
    startup_enabled_at(&path, startup_approval()?.as_deref())
}

fn clear_startup_approval() -> Result<(), String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
    let result = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            w!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder"
            ),
            w!("Taskbar Groups.lnk"),
        )
    };
    if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "Could not enable Windows startup approval: {}",
            std::io::Error::from_raw_os_error(result.0 as i32)
        ))
    }
}
pub fn enable_startup() -> Result<(), String> {
    let script = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:TG_LINK); $s.TargetPath=$env:TG_EXE; $s.Arguments='--watch'; $s.Save()";
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("TG_LINK", startup_path()?)
        .env(
            "TG_EXE",
            std::env::current_exe().map_err(|e| e.to_string())?,
        )
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        clear_startup_approval()
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}
pub fn disable_startup() -> Result<(), String> {
    match std::fs::remove_file(startup_path()?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn create_shortcut(group: &Group) -> Result<PathBuf, String> {
    let dir = PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?)
        .join("Taskbar Groups");
    create_shortcut_in_dir(group, &dir)
}

pub fn create_saved_shortcut(path: &Path, id: uuid::Uuid) -> Result<PathBuf, String> {
    Store::with_lock(path, || {
        let store = Store::load(path)?;
        let group = store
            .groups
            .iter()
            .find(|group| group.id == id)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "Group no longer exists")
            })?;
        Ok(create_shortcut(group))
    })
    .map_err(|e| e.to_string())?
}

fn create_shortcut_in_dir(group: &Group, dir: &Path) -> Result<PathBuf, String> {
    let _com = ComApartment::new();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let title = shortcut_title(group).replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_");
    let path = dir.join(format!("{title}.lnk"));
    let (icon_path, icon_index) = group_icon_location(group, dir)?;
    let script = "$ErrorActionPreference='Stop'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:TG_LINK); $s.TargetPath=$env:TG_EXE; $s.Arguments=$env:TG_ARGS; $s.Description='Taskbar Groups launcher'; $s.IconLocation=\"$env:TG_ICON,$env:TG_ICONINDEX\"; $s.Save()";
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("TG_LINK", &path)
        .env(
            "TG_EXE",
            std::env::current_exe().map_err(|e| e.to_string())?,
        )
        .env("TG_ARGS", format!("--group {}", group.id))
        .env("TG_ICON", &icon_path)
        .env("TG_ICONINDEX", icon_index.to_string())
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let filename = wide(path.as_os_str());
        persist
            .Load(PCWSTR(filename.as_ptr()), STGM_READWRITE)
            .map_err(|e| e.to_string())?;
        let properties: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        let id = app_id(&group.id);
        let value = PROPVARIANT::from(id.as_str());
        properties
            .SetValue(&key, &value)
            .and_then(|_| properties.Commit())
            .and_then(|_| persist.Save(PCWSTR(filename.as_ptr()), true))
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

fn group_icon_location(group: &Group, dir: &Path) -> Result<(PathBuf, i32), String> {
    use image::ImageEncoder;
    let Some(app) = group.apps.first() else {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?;
        return Ok((PathBuf::from(root).join("System32").join("shell32.dll"), 3));
    };
    let icon_dir = dir.join("icons");
    std::fs::create_dir_all(&icon_dir).map_err(|e| e.to_string())?;
    let path = icon_dir.join(format!("{}-{}.ico", group.id, app.id));
    if !path.exists() {
        let image =
            icon(&app.path).ok_or_else(|| format!("Could not read the icon for {}", app.name))?;
        let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
        for pixel in image.pixels {
            rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
        let mut encoded = Vec::new();
        image::codecs::ico::IcoEncoder::new(&mut encoded)
            .write_image(
                &rgba,
                image.size[0] as u32,
                image.size[1] as u32,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())?;
        let temporary = path.with_extension("ico.tmp");
        std::fs::write(&temporary, encoded).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
    }
    Ok((path, 0))
}

fn update_matching_shortcut(
    path: &Path,
    group: &Group,
    icon_path: &Path,
    icon_index: i32,
) -> Result<Option<bool>, String> {
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let filename = wide(path.as_os_str());
        persist
            .Load(PCWSTR(filename.as_ptr()), STGM_READWRITE)
            .map_err(|e| e.to_string())?;
        let properties: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        let id = properties
            .GetValue(&key)
            .ok()
            .and_then(|value| windows::core::BSTR::try_from(&value).ok())
            .map(|value| value.to_string());
        if id.as_deref() != Some(app_id(&group.id).as_str()) {
            return Ok(None);
        }
        let mut current = [0u16; 1024];
        let mut old_index = 0;
        if link.GetIconLocation(&mut current, &mut old_index).is_ok() {
            let len = current
                .iter()
                .position(|&ch| ch == 0)
                .unwrap_or(current.len());
            let old_path = PathBuf::from(String::from_utf16_lossy(&current[..len]));
            if old_path == icon_path && old_index == icon_index {
                return Ok(Some(false));
            }
        }
        let icon = wide(icon_path.as_os_str());
        link.SetIconLocation(PCWSTR(icon.as_ptr()), icon_index)
            .map_err(|e| e.to_string())?;
        let id = app_id(&group.id);
        properties
            .SetValue(&key, &PROPVARIANT::from(id.as_str()))
            .and_then(|_| properties.Commit())
            .map_err(|e| e.to_string())?;
        persist
            .Save(PCWSTR(filename.as_ptr()), true)
            .map_err(|e| e.to_string())?;
        SHChangeNotify(
            SHCNE_UPDATEITEM,
            SHCNF_PATHW,
            Some(filename.as_ptr().cast()),
            None,
        );
        Ok(Some(true))
    }
}

pub fn sync_group_icon(group: &Group) -> Result<bool, String> {
    let _com = ComApartment::new();
    let appdata = PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?);
    let group_dir = appdata.join("Taskbar Groups");
    let pinned = appdata.join("Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar");
    sync_group_icon_in_dirs(group, &group_dir, &pinned)
}

pub fn sync_saved_group_icon(path: &Path, id: uuid::Uuid) -> Result<bool, String> {
    Store::with_lock(path, || {
        let store = Store::load(path)?;
        let group = store
            .groups
            .iter()
            .find(|group| group.id == id)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "Group no longer exists")
            })?;
        Ok(sync_group_icon(group))
    })
    .map_err(|e| e.to_string())?
}

fn shortcut_app_id(path: &Path) -> Result<Option<String>, String> {
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let filename = wide(path.as_os_str());
        persist
            .Load(PCWSTR(filename.as_ptr()), STGM_READ)
            .map_err(|e| e.to_string())?;
        let properties: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        Ok(properties
            .GetValue(&key)
            .ok()
            .and_then(|value| windows::core::BSTR::try_from(&value).ok())
            .map(|value| value.to_string()))
    }
}

fn pinned_shortcut_for_app_id(dir: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    let dir = dir.to_owned();
    let id = id.to_owned();
    std::thread::spawn(move || {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return Err("Could not initialize shortcut resolution".into());
        }
        pinned_shortcut_for_app_id_in_apartment(&dir, &id)
    })
    .join()
    .map_err(|_| "Shortcut resolution thread failed".to_string())?
}

fn pinned_shortcut_for_app_id_in_apartment(
    dir: &Path,
    id: &str,
) -> Result<Option<PathBuf>, String> {
    unsafe {
        let implicit_target = std::fs::canonicalize(id).ok().or_else(|| {
            let (folder, relative) = id.split_once('}')?;
            let folder = uuid::Uuid::parse_str(folder.strip_prefix('{')?).ok()?;
            let relative = relative.strip_prefix('\\')?;
            let relative = Path::new(relative);
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return None;
            }
            let guid = GUID::from_u128(folder.as_u128());
            let root = SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None).ok()?;
            let root_path = root.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(root.0.cast()));
            std::fs::canonicalize(root_path?.join(relative)).ok()
        });
        let mut found = None;
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            {
                continue;
            }
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.to_string())?;
            let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            let filename = wide(path.as_os_str());
            persist
                .Load(PCWSTR(filename.as_ptr()), STGM_READ)
                .map_err(|e| e.to_string())?;
            let properties: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            let value = properties.GetValue(&key).map_err(|e| e.to_string())?;
            let explicit = if value.Anonymous.Anonymous.vt.0 == 0 {
                None
            } else {
                Some(
                    windows::core::BSTR::try_from(&value)
                        .map_err(|e| e.to_string())?
                        .to_string(),
                )
            };
            let matches = if let Some(explicit) = explicit {
                explicit.eq_ignore_ascii_case(id)
            } else if let Some(expected) = &implicit_target {
                let mut target = [0u16; 32768];
                link.GetPath(&mut target, std::ptr::null_mut(), 0)
                    .map_err(|e| e.to_string())?;
                let len = target
                    .iter()
                    .position(|&ch| ch == 0)
                    .unwrap_or(target.len());
                let actual = msi_shortcut_target(&path)
                    .unwrap_or_else(|| PathBuf::from(String::from_utf16_lossy(&target[..len])));
                std::fs::canonicalize(actual).ok().is_some_and(|actual| {
                    actual
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&expected.to_string_lossy())
                })
            } else {
                false
            };
            if matches {
                if found.is_some() {
                    return Err("Multiple pinned shortcuts have this application identity".into());
                }
                found = Some(path);
            }
        }
        Ok(found)
    }
}

fn start_menu_shortcut_by_identity_name(
    roots: &[PathBuf],
    id: &str,
    label: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(name) = label
        .get(..label.len().saturating_sub(7))
        .filter(|_| label.to_ascii_lowercase().ends_with(" pinned"))
    else {
        return Ok(None);
    };
    let normalized = |value: &str| {
        value
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    if normalized(id) != normalized(name) || normalized(name).is_empty() {
        return Ok(None);
    }
    let mut todo: Vec<(PathBuf, usize)> = roots.iter().cloned().map(|path| (path, 0)).collect();
    let mut matching = None;
    let mut visited = 0usize;
    while let Some((dir, depth)) = todo.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            visited += 1;
            if visited > 30000 {
                return Err("Start menu contains too many items to resolve safely".into());
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() && depth < 12 {
                todo.push((entry.path(), depth + 1));
            }
            if kind.is_file() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
                    && path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
                {
                    if matching.is_some() {
                        return Ok(None);
                    }
                    matching = Some(path);
                }
            }
        }
    }
    let Some(candidate) = matching else {
        return Ok(None);
    };
    let desired_id = id.to_owned();
    std::thread::spawn(move || {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return Err("Could not initialize shortcut resolution".into());
        }
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.to_string())?;
            let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            let filename = wide(candidate.as_os_str());
            persist
                .Load(PCWSTR(filename.as_ptr()), STGM_READ)
                .map_err(|e| e.to_string())?;
            let properties: IPropertyStore = link.cast().map_err(|e| e.to_string())?;
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            let value = properties.GetValue(&key).map_err(|e| e.to_string())?;
            if value.Anonymous.Anonymous.vt.0 != 0 {
                let actual = windows::core::BSTR::try_from(&value)
                    .map_err(|e| e.to_string())?
                    .to_string();
                if !actual.is_empty() && !actual.eq_ignore_ascii_case(&desired_id) {
                    return Ok(None);
                }
            }
            Ok(Some(candidate))
        }
    })
    .join()
    .map_err(|_| "Shortcut resolution thread failed".to_string())?
}

pub(crate) fn msi_shortcut_target(path: &Path) -> Option<PathBuf> {
    unsafe {
        let mut product = [0u16; 39];
        let mut component = [0u16; 39];
        let path = wide(path.as_os_str());
        if MsiGetShortcutTargetW(
            PCWSTR(path.as_ptr()),
            Some(PWSTR(product.as_mut_ptr())),
            None,
            Some(PWSTR(component.as_mut_ptr())),
        ) != 0
        {
            return None;
        }
        let mut buffer = vec![0u16; 32768];
        let mut length = buffer.len() as u32;
        let state = MsiGetComponentPathW(
            PCWSTR(product.as_ptr()),
            PCWSTR(component.as_ptr()),
            Some(PWSTR(buffer.as_mut_ptr())),
            Some(&mut length),
        );
        if state != INSTALLSTATE_LOCAL && state != INSTALLSTATE_SOURCE {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..length as usize],
        )))
    }
}

fn create_shell_item_shortcut(parsing_name: &str, destination: &Path) -> Result<(), String> {
    let parsing_name = parsing_name.to_owned();
    let destination = destination.to_owned();
    std::thread::spawn(move || {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return Err("Could not initialize Windows shell".into());
        }
        unsafe {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(
                PCWSTR(wide(parsing_name).as_ptr()),
                None,
                &mut pidl,
                0,
                None,
            )
            .map_err(|e| e.to_string())?;
            let result = (|| {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?;
                link.SetIDList(pidl).map_err(|e| e.to_string())?;
                let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
                persist
                    .Save(PCWSTR(wide(destination.as_os_str()).as_ptr()), true)
                    .map_err(|e| e.to_string())
            })();
            CoTaskMemFree(Some(pidl.cast()));
            result
        }
    })
    .join()
    .map_err(|_| "Windows shell shortcut creation failed".to_string())?
}

fn shell_item_for_unpin(path: &Path, packaged: bool) -> Result<IShellItem, String> {
    unsafe {
        let pidl = if packaged {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.to_string())?;
            let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            persist
                .Load(PCWSTR(wide(path.as_os_str()).as_ptr()), STGM_READ)
                .map_err(|e| e.to_string())?;
            link.GetIDList().map_err(|e| e.to_string())?
        } else {
            shell_item_for_path(path)?
        };
        let result = SHCreateItemFromIDList(pidl).map_err(|e| e.to_string());
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

fn unpin_taskbar_shortcut(path: &Path, packaged: bool) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("Shortcut not found: {}", path.display()));
    }
    let path: PathBuf = path.components().collect();
    std::thread::spawn(move || {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return Err("Could not initialize Windows shell".into());
        }
        unsafe {
            let item = shell_item_for_unpin(&path, packaged)
                .map_err(|e| format!("Could not open shell item: {e}"))?;
            let pins: IStartMenuPinnedList =
                CoCreateInstance(&StartMenuPin, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("Could not access pinned list: {e}"))?;
            pins.RemoveFromList(&item)
                .map_err(|e| format!("Could not unpin shortcut: {e}"))
        }
    })
    .join()
    .map_err(|_| "Windows unpin operation failed".to_string())?
}

#[derive(Clone, Copy)]
enum PinInput {
    Down(POINT, Instant),
    Up(POINT, Instant),
    Cancel,
}
static MOUSE_EVENTS: OnceLock<mpsc::Sender<PinInput>> = OnceLock::new();

unsafe extern "system" fn mouse_event_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 && (wparam.0 == WM_LBUTTONDOWN as usize || wparam.0 == WM_LBUTTONUP as usize) {
            let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if let Some(sender) = MOUSE_EVENTS.get() {
                let now = Instant::now();
                let _ = sender.send(if wparam.0 == WM_LBUTTONDOWN as usize {
                    PinInput::Down(data.pt, now)
                } else {
                    PinInput::Up(data.pt, now)
                });
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}
unsafe extern "system" fn escape_event_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0
            && (wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize)
            && (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode == VK_ESCAPE.0 as u32
            && let Some(sender) = MOUSE_EVENTS.get()
        {
            let _ = sender.send(PinInput::Cancel);
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

fn mouse_button_events() -> mpsc::Receiver<PinInput> {
    let (sender, receiver) = mpsc::channel();
    if MOUSE_EVENTS.set(sender).is_err() {
        return receiver;
    }
    std::thread::spawn(|| unsafe {
        let Ok(_hook) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_event_hook), None, 0) else {
            return;
        };
        let _escape_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(escape_event_hook), None, 0).ok();
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    });
    receiver
}

fn sync_group_icon_in_dirs(group: &Group, group_dir: &Path, pinned: &Path) -> Result<bool, String> {
    let (icon_path, icon_index) = group_icon_location(group, group_dir)?;
    let original = group_dir.join(format!("{}.lnk", shortcut_title(group)));
    if original.is_file() {
        update_matching_shortcut(&original, group, &icon_path, icon_index)?;
    }
    let mut pinned_group = false;
    if pinned.is_dir() {
        for entry in std::fs::read_dir(pinned).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            {
                // Read identity before opening a matching pin for writing; unrelated pins are never modified.
                match shortcut_app_id(&path) {
                    Ok(Some(id)) if id.eq_ignore_ascii_case(&app_id(&group.id)) => {
                        pinned_group |=
                            update_matching_shortcut(&path, group, &icon_path, icon_index)?
                                .unwrap_or(false);
                    }
                    Err(e)
                        if path
                            .file_stem()
                            .is_some_and(|name| name == shortcut_title(group).as_str()) =>
                    {
                        return Err(e);
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(pinned_group)
}

pub fn set_app_id(id: &uuid::Uuid) {
    let value = wide(app_id(id));
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(value.as_ptr()));
    }
}

pub fn cursor_in_rect(rect: taskbar_groups::Rect) -> bool {
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).is_ok()
            && point.x >= rect.left
            && point.x < rect.right
            && point.y >= rect.top
            && point.y < rect.bottom
    }
}

pub fn launch(path: &str) -> Result<(), String> {
    let file = wide(path);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(format!(
            "Windows could not open {path} (error {}).",
            result.0 as isize
        ))
    } else {
        Ok(())
    }
}

pub fn context_menu(path: &str, window: isize) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut pidl = std::ptr::null_mut();
        let name = wide(path);
        SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut pidl, 0, None)
            .map_err(|e| e.to_string())?;
        let mut child = std::ptr::null_mut();
        let result = (|| -> Result<(), String> {
            let folder: IShellFolder =
                SHBindToParent(pidl, Some(&mut child)).map_err(|e| e.to_string())?;
            let owner = HWND(window as *mut std::ffi::c_void);
            if owner.0.is_null() {
                return Err("The group window is not active".into());
            }
            let menu: IContextMenu = folder
                .GetUIObjectOf(owner, &[child], None)
                .map_err(|e| e.to_string())?;
            let popup = CreatePopupMenu().map_err(|e| e.to_string())?;
            let query = menu.QueryContextMenu(popup, 0, 1, 0x7fff, CMF_NORMAL);
            if query.is_err() {
                let _ = DestroyMenu(popup);
                return Err(query.message());
            }
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            let previous =
                SetWindowLongPtrW(owner, GWLP_WNDPROC, menu_window_proc as *const () as isize);
            if previous == 0 {
                let _ = DestroyMenu(popup);
                return Err("Could not attach the Windows shell menu to the group window".into());
            }
            if previous != 0 {
                MENU_HANDLER.with(|slot| {
                    *slot.borrow_mut() = Some((previous, menu.cast().ok(), menu.cast().ok()))
                });
            }
            let command = TrackPopupMenu(
                popup,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                owner,
                None,
            )
            .0 as usize;
            if previous != 0 {
                let _ = SetWindowLongPtrW(owner, GWLP_WNDPROC, previous);
                MENU_HANDLER.with(|slot| *slot.borrow_mut() = None);
            }
            let _ = DestroyMenu(popup);
            if command != 0 {
                let info = CMINVOKECOMMANDINFO {
                    cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                    hwnd: owner,
                    lpVerb: PCSTR((command - 1) as *const u8),
                    nShow: SW_SHOWNORMAL.0,
                    ..Default::default()
                };
                menu.InvokeCommand(&info).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

pub fn move_window(window: isize, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(
            HWND(window as *mut std::ffi::c_void),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

fn shortcut_icon_without_overlay(path: &str) -> Option<HICON> {
    let _com = ComApartment::new();
    unsafe {
        let filename = wide(path);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist.Load(PCWSTR(filename.as_ptr()), STGM_READ).ok()?;
        let mut custom = [0u16; 32768];
        let mut icon_index = 0;
        if link.GetIconLocation(&mut custom, &mut icon_index).is_ok() && custom[0] != 0 {
            let mut icon = HICON::default();
            let _ = ExtractIconExW(
                PCWSTR(custom.as_ptr()),
                icon_index,
                Some(&mut icon),
                None,
                1,
            );
            if !icon.0.is_null() {
                return Some(icon);
            }
        }
        let target = msi_shortcut_target(Path::new(path)).or_else(|| {
            let mut target = [0u16; 32768];
            link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
            let len = target.iter().position(|&ch| ch == 0)?;
            (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&target[..len])))
        });
        if let Some(target) = target {
            let target = wide(target.as_os_str());
            let mut info = SHFILEINFOW::default();
            if SHGetFileInfoW(
                PCWSTR(target.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            ) != 0
            {
                return Some(info.hIcon);
            }
        }
        let mut pidl = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(filename.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let result = (|| {
            let mut child = std::ptr::null_mut();
            let folder: IShellFolder = SHBindToParent(pidl, Some(&mut child)).ok()?;
            let extractor: IExtractIconW =
                folder.GetUIObjectOf(HWND::default(), &[child], None).ok()?;
            let mut location = [0u16; 32768];
            let mut index = 0;
            let mut flags = 0;
            extractor
                .GetIconLocation(GIL_FORSHELL, &mut location, &mut index, &mut flags)
                .ok()?;
            let mut icon = HICON::default();
            let _ = extractor.Extract(
                PCWSTR(location.as_ptr()),
                index as u32,
                Some(&mut icon),
                None,
                32,
            );
            if icon.0.is_null() && flags & GIL_NOTFILENAME == 0 {
                let _ = ExtractIconExW(PCWSTR(location.as_ptr()), index, Some(&mut icon), None, 1);
            }
            (!icon.0.is_null()).then_some(icon)
        })();
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

pub fn icon(path: &str) -> Option<eframe::egui::ColorImage> {
    unsafe {
        let unadorned = Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            .then(|| shortcut_icon_without_overlay(path))
            .flatten();
        let icon = if let Some(icon) = unadorned {
            icon
        } else {
            let name = wide(path);
            let mut info = SHFILEINFOW::default();
            if SHGetFileInfoW(
                PCWSTR(name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            ) == 0
            {
                return None;
            }
            info.hIcon
        };
        let dc = CreateCompatibleDC(None);
        let mut pixels = std::ptr::null_mut();
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 32,
                biHeight: -32,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let bitmap =
            match CreateDIBSection(Some(dc), &bitmap_info, DIB_RGB_COLORS, &mut pixels, None, 0) {
                Ok(v) => v,
                Err(_) => {
                    let _ = DestroyIcon(icon);
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        let _ = DrawIconEx(dc, 0, 0, icon, 32, 32, 0, None, DI_NORMAL);
        let bytes = std::slice::from_raw_parts(pixels as *const u8, 32 * 32 * 4);
        let mut rgba = Vec::with_capacity(bytes.len());
        for p in bytes.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        let _ = SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        let _ = DestroyIcon(icon);
        Some(eframe::egui::ColorImage::from_rgba_premultiplied(
            [32, 32],
            &rgba,
        ))
    }
}

unsafe fn group_at_point(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    store: &Store,
    point: POINT,
) -> Option<(uuid::Uuid, windows::Win32::Foundation::RECT)> {
    let mut element = unsafe { automation.ElementFromPoint(point).ok()? };
    for _ in 0..6 {
        if let Ok(value) = unsafe { element.CurrentAutomationId() }
            && let Some(id) = group_for_automation_id(&store.groups, &value.to_string())
            && let Ok(rect) = unsafe { element.CurrentBoundingRectangle() }
        {
            return Some((id, rect));
        }
        element = unsafe { walker.GetParentElement(&element).ok()? };
    }
    None
}

#[derive(Clone)]
struct TaskbarButton {
    identity: String,
    name: String,
    bounds: windows::Win32::Foundation::RECT,
}
struct TaskbarSnapshot {
    root: HWND,
    captured: Instant,
    buttons: Vec<TaskbarButton>,
}
unsafe fn snapshot_taskbar(automation: &IUIAutomation, root: HWND) -> Option<TaskbarSnapshot> {
    unsafe {
        let items = automation
            .ElementFromHandle(root)
            .ok()?
            .FindAll(
                TreeScope_Descendants,
                &automation.CreateTrueCondition().ok()?,
            )
            .ok()?;
        let mut buttons: Vec<TaskbarButton> = Vec::new();
        for index in 0..items.Length().ok()? {
            let Ok(item) = items.GetElement(index) else {
                continue;
            };
            let Ok(id) = item.CurrentAutomationId() else {
                continue;
            };
            let id = id.to_string();
            let Some(identity) = id.strip_prefix("Appid: ") else {
                continue;
            };
            let Ok(bounds) = item.CurrentBoundingRectangle() else {
                continue;
            };
            if bounds.left >= bounds.right
                || bounds.top >= bounds.bottom
                || buttons
                    .iter()
                    .any(|button| button.identity.eq_ignore_ascii_case(identity))
            {
                continue;
            }
            buttons.push(TaskbarButton {
                identity: identity.to_owned(),
                name: item
                    .CurrentName()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
                bounds,
            });
        }
        Some(TaskbarSnapshot {
            root,
            captured: Instant::now(),
            buttons,
        })
    }
}
fn contains_point(rect: windows::Win32::Foundation::RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}
fn target_position_is_unambiguous(
    snapshot: &TaskbarSnapshot,
    point: POINT,
    source: &str,
    group: &str,
) -> bool {
    snapshot.buttons.iter().any(|button| {
        contains_point(button.bounds, point)
            && (button.identity.eq_ignore_ascii_case(source)
                || button.identity.eq_ignore_ascii_case(group))
    }) && !snapshot.buttons.iter().any(|button| {
        contains_point(button.bounds, point)
            && !button.identity.eq_ignore_ascii_case(source)
            && !button.identity.eq_ignore_ascii_case(group)
    })
}
fn process_window_at_point(point: POINT, process_id: u32) -> Option<HWND> {
    unsafe {
        let root = GetAncestor(WindowFromPoint(point), GA_ROOT);
        let mut actual = 0;
        GetWindowThreadProcessId(root, Some(&mut actual));
        (actual != 0 && actual == process_id).then_some(root)
    }
}
unsafe extern "system" fn find_visible_process_window(hwnd: HWND, arg: LPARAM) -> BOOL {
    unsafe {
        let context = &mut *(arg.0 as *mut (u32, bool));
        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if process_id == context.0 && IsWindowVisible(hwnd).as_bool() {
            context.1 = true;
            return BOOL(0);
        }
        BOOL(1)
    }
}
fn process_has_visible_window(process_id: u32) -> bool {
    let mut context = (process_id, false);
    unsafe {
        let _ = EnumWindows(
            Some(find_visible_process_window),
            LPARAM((&mut context as *mut (u32, bool)) as isize),
        );
    }
    context.1
}
pub fn cursor_on_window(window: isize) -> bool {
    unsafe {
        let mut cursor = POINT::default();
        let mut rect = windows::Win32::Foundation::RECT::default();
        GetCursorPos(&mut cursor).is_ok()
            && GetWindowRect(HWND(window as *mut std::ffi::c_void), &mut rect).is_ok()
            && contains_point(rect, cursor)
    }
}

pub fn cursor_in_client(window: isize) -> Option<(i32, i32)> {
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok()?;
        ScreenToClient(HWND(window as *mut std::ffi::c_void), &mut point)
            .as_bool()
            .then_some((point.x, point.y))
    }
}

pub fn primary_button_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 }
}

fn calculate_position(
    root: HWND,
    anchor: windows::Win32::Foundation::RECT,
    count: usize,
    icons_per_row: usize,
    extra_height: f32,
) -> Option<(i32, i32)> {
    let (width, height) = taskbar_groups::flyout_size_with_columns(count, icons_per_row);
    calculate_position_sized(root, anchor, width, height + extra_height)
}

fn calculate_position_sized(
    root: HWND,
    anchor: windows::Win32::Foundation::RECT,
    width: f32,
    height: f32,
) -> Option<(i32, i32)> {
    unsafe {
        let point = POINT {
            x: (anchor.left + anchor.right) / 2,
            y: (anchor.top + anchor.bottom) / 2,
        };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut bar = windows::Win32::Foundation::RECT::default();
        if !GetMonitorInfoW(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST), &mut info).as_bool()
            || GetWindowRect(root, &mut bar).is_err()
        {
            return None;
        }
        let rect = |r: windows::Win32::Foundation::RECT| taskbar_groups::Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        };
        Some(taskbar_groups::position_flyout(
            rect(info.rcWork),
            rect(bar),
            rect(anchor),
            width,
            height,
            GetDpiForWindow(root) as f32 / 96.0,
        ))
    }
}

pub fn position_at_anchor(
    anchor: taskbar_groups::Rect,
    count: usize,
    icons_per_row: usize,
    extra_height: f32,
) -> Option<(i32, i32)> {
    let (width, height) = taskbar_groups::flyout_size_with_columns(count, icons_per_row);
    position_at_anchor_sized(anchor, width, height + extra_height)
}

pub fn position_at_anchor_sized(
    anchor: taskbar_groups::Rect,
    width: f32,
    height: f32,
) -> Option<(i32, i32)> {
    unsafe {
        let point = POINT {
            x: (anchor.left + anchor.right) / 2,
            y: (anchor.top + anchor.bottom) / 2,
        };
        let root = GetAncestor(WindowFromPoint(point), GA_ROOT);
        let mut class = [0u16; 128];
        let len = GetClassNameW(root, &mut class).max(0) as usize;
        let name = String::from_utf16_lossy(&class[..len]);
        if name != "Shell_TrayWnd" && name != "Shell_SecondaryTrayWnd" {
            return None;
        }
        calculate_position_sized(
            root,
            windows::Win32::Foundation::RECT {
                left: anchor.left,
                top: anchor.top,
                right: anchor.right,
                bottom: anchor.bottom,
            },
            width,
            height,
        )
    }
}

pub fn taskbar_position_for_group(
    group: &Group,
    icons_per_row: usize,
) -> Option<((i32, i32), taskbar_groups::Rect)> {
    unsafe {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return None;
        }
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_ok() {
            let root = GetAncestor(WindowFromPoint(cursor), GA_ROOT);
            let mut class = [0u16; 128];
            let len = GetClassNameW(root, &mut class).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..len]);
            if (class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd")
                && let Ok(walker) = automation.ControlViewWalker()
                && let Some((id, anchor)) = group_at_point(
                    &automation,
                    &walker,
                    &Store {
                        groups: vec![group.clone()],
                        ..Store::default()
                    },
                    cursor,
                )
                && id == group.id
                && let Some(position) =
                    calculate_position(root, anchor, group.apps.len(), icons_per_row, 0.0)
            {
                return Some((
                    position,
                    taskbar_groups::Rect {
                        left: anchor.left,
                        top: anchor.top,
                        right: anchor.right,
                        bottom: anchor.bottom,
                    },
                ));
            }
        }
        let condition = automation.CreateTrueCondition().ok()?;
        let mut roots = Vec::new();
        if let Ok(root) = FindWindowW(w!("Shell_TrayWnd"), None) {
            roots.push(root);
        }
        let mut last = None;
        while let Ok(root) = FindWindowExW(None, last, w!("Shell_SecondaryTrayWnd"), None) {
            roots.push(root);
            last = Some(root);
        }
        let desired = format!("Appid: {}", app_id(&group.id));
        for root in roots {
            let Ok(element) = automation.ElementFromHandle(root) else {
                continue;
            };
            let Ok(items) = element.FindAll(TreeScope_Descendants, &condition) else {
                continue;
            };
            for index in 0..items.Length().unwrap_or(0) {
                let Ok(item) = items.GetElement(index) else {
                    continue;
                };
                if item
                    .CurrentAutomationId()
                    .ok()
                    .is_some_and(|id| id.to_string().eq_ignore_ascii_case(&desired))
                    && let Ok(rect) = item.CurrentBoundingRectangle()
                    && let Some(position) =
                        calculate_position(root, rect, group.apps.len(), icons_per_row, 0.0)
                {
                    return Some((
                        position,
                        taskbar_groups::Rect {
                            left: rect.left,
                            top: rect.top,
                            right: rect.right,
                            bottom: rect.bottom,
                        },
                    ));
                }
            }
        }
        None
    }
}

pub(crate) fn group_has_accessible_pin(group: &Group, icons_per_row: usize) -> bool {
    let Some(dir) = taskbar_pin_directory() else {
        return false;
    };
    if pinned_shortcut_for_app_id_in_apartment(&dir, &app_id(&group.id))
        .ok()
        .flatten()
        .is_none()
    {
        return false;
    }
    taskbar_position_for_group(group, icons_per_row).is_some()
}

struct ActivePinDrag {
    source: PathBuf,
    temporary_source: bool,
    source_identity: String,
    name: String,
    state: taskbar_groups::PinDrag,
    targets: Vec<taskbar_groups::DragTarget>,
    root: HWND,
    began: Instant,
    popup_ready: Option<Instant>,
    image_requested: bool,
    image_receiver: Option<mpsc::Receiver<Option<eframe::egui::ColorImage>>>,
    ghost: Option<DragGhost>,
    layout_path: PathBuf,
}
impl ActivePinDrag {
    fn request_drag_image(&mut self) {
        if self.image_requested {
            return;
        }
        self.image_requested = true;
        let source = self.source.clone();
        let (sender, receiver) = mpsc::channel();
        self.image_receiver = Some(receiver);
        std::thread::spawn(move || {
            let apartment = ComApartment::new();
            let image = apartment
                .0
                .then(|| icon(&source.to_string_lossy()))
                .flatten();
            let _ = sender.send(image);
        });
    }

    fn update_drag_image(&mut self, point: POINT, on_taskbar: bool) {
        if let Some(receiver) = &self.image_receiver {
            match receiver.try_recv() {
                Ok(image) => {
                    self.ghost = image.and_then(|image| {
                        let dpi = unsafe { GetDpiForWindow(self.root) };
                        DragGhost::new(&image, dpi).ok()
                    });
                    self.image_receiver = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.image_receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(ghost) = &mut self.ghost {
            if should_show_pin_drag_image(self.popup_ready.is_some(), on_taskbar) {
                ghost.show_at(point);
            } else {
                ghost.hide();
            }
        }
    }
}
impl Drop for ActivePinDrag {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.layout_path);
        let _ = std::fs::remove_file(self.layout_path.with_extension("tmp"));
        if self.temporary_source {
            let _ = std::fs::remove_file(&self.source);
        }
    }
}

fn snapshot_precedes_press(captured: Instant, press: Instant) -> bool {
    press
        .checked_duration_since(captured)
        .is_some_and(|age| age <= Duration::from_millis(500))
}
fn should_show_pin_drag_image(flyout_visible: bool, cursor_on_taskbar: bool) -> bool {
    flyout_visible && !cursor_on_taskbar
}
fn release_after_popup_ready(ready: Option<Instant>, release: Instant) -> bool {
    ready.is_some_and(|ready| release >= ready)
}

fn group_targets(snapshot: &TaskbarSnapshot, store: &Store) -> Vec<taskbar_groups::DragTarget> {
    snapshot
        .buttons
        .iter()
        .filter_map(|button| {
            let group = store
                .groups
                .iter()
                .find(|group| app_id(&group.id).eq_ignore_ascii_case(&button.identity))?;
            Some(taskbar_groups::DragTarget {
                group: group.id,
                bounds: taskbar_groups::Rect {
                    left: button.bounds.left,
                    top: button.bounds.top,
                    right: button.bounds.right,
                    bottom: button.bounds.bottom,
                },
            })
        })
        .collect()
}

fn source_identity_verified(
    expected: &str,
    at_press: Option<&str>,
    at_cursor: Option<&str>,
) -> bool {
    match at_press {
        Some(id) => id.eq_ignore_ascii_case(expected),
        None => at_cursor.is_some_and(|id| id.eq_ignore_ascii_case(expected)),
    }
}
fn source_label(label: &str) -> String {
    let label = label.strip_suffix(" pinned").unwrap_or(label);
    if let Some((name, suffix)) = label.rsplit_once(" - ")
        && let Some((count, kind)) = suffix.split_once(' ')
        && count.parse::<usize>().is_ok()
        && (kind == "running window" || kind == "running windows")
    {
        return name.to_owned();
    }
    label.to_owned()
}
fn shortcut_target_description(path: &Path) -> Option<String> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist
            .Load(PCWSTR(wide(path.as_os_str()).as_ptr()), STGM_READ)
            .ok()?;
        let mut buffer = [0u16; 32768];
        link.GetPath(&mut buffer, std::ptr::null_mut(), 0).ok()?;
        let len = buffer.iter().position(|&c| c == 0)?;
        let target = msi_shortcut_target(path)
            .unwrap_or_else(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])));
        let item: IShellItem2 =
            SHCreateItemFromParsingName(PCWSTR(wide(target.as_os_str()).as_ptr()), None).ok()?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x0cef7d53_fa64_11d1_a203_0000f81fedee),
            pid: 3,
        };
        let value = item.GetString(&key).ok()?;
        let name = value.to_string().ok();
        CoTaskMemFree(Some(value.0.cast()));
        name.filter(|value| !value.trim().is_empty())
    }
}
fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("Microsoft/Windows/Start Menu/Programs"));
    }
    if let Some(common) = std::env::var_os("ProgramData") {
        roots.push(PathBuf::from(common).join("Microsoft/Windows/Start Menu/Programs"));
    }
    roots
}
fn friendly_name_from_shortcuts(source: &Path, roots: &[PathBuf]) -> Option<String> {
    let source = source.to_owned();
    let roots = roots.to_vec();
    std::thread::spawn(move || {
        let apartment = ComApartment::new();
        if !apartment.0 {
            return None;
        }
        let mut todo: Vec<(PathBuf, usize)> = roots.into_iter().map(|path| (path, 0)).collect();
        let mut found: Option<String> = None;
        let mut visited = 0usize;
        while let Some((dir, depth)) = todo.pop() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > 30000 {
                    return None;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && depth < 12 {
                    todo.push((entry.path(), depth + 1));
                }
                if kind.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
                    && same_launch_target(&source, &entry.path())
                    && let Some(name) = entry
                        .path()
                        .file_stem()
                        .and_then(|v| v.to_str())
                        .map(str::to_owned)
                    && uuid::Uuid::parse_str(&name).is_err()
                {
                    if found
                        .as_ref()
                        .is_some_and(|other| !other.eq_ignore_ascii_case(&name))
                    {
                        return None;
                    }
                    found = Some(name);
                }
            }
        }
        found
    })
    .join()
    .ok()
    .flatten()
}
fn dragged_app_display_name(label: &str, source: &Path) -> String {
    let label = source_label(label);
    if !label.trim().is_empty() && uuid::Uuid::parse_str(label.trim()).is_err() {
        return label;
    }
    if let Some(name) = shortcut_target_description(source) {
        return name;
    }
    source
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && uuid::Uuid::parse_str(name).is_err())
        .unwrap_or("Application")
        .to_owned()
}
fn repair_generated_names_in_roots(path: &Path, roots: &[PathBuf]) -> std::io::Result<usize> {
    let store = Store::load(path)?;
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("shortcuts");
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return Ok(0);
    };
    let mut replacements = Vec::new();
    for group in &store.groups {
        for app in &group.apps {
            let source = Path::new(&app.path);
            if uuid::Uuid::parse_str(&app.name).is_err()
                || source
                    .parent()
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .as_ref()
                    != Some(&dir)
            {
                continue;
            }
            if let Some(name) = friendly_name_from_shortcuts(source, roots) {
                replacements.push((group.id, app.clone(), name));
            }
        }
    }
    if replacements.is_empty() {
        return Ok(0);
    }
    Store::with_lock(path, || {
        let mut latest = Store::load(path)?;
        let mut updated = 0;
        for (group_id, original, name) in &replacements {
            if let Some(app) = latest
                .groups
                .iter_mut()
                .find(|g| g.id == *group_id)
                .and_then(|g| g.apps.iter_mut().find(|app| **app == *original))
            {
                app.name = name.clone();
                updated += 1;
            }
        }
        if updated > 0 {
            latest.save(path)?;
        }
        Ok(updated)
    })
}
pub fn repair_generated_names(path: &Path) -> std::io::Result<usize> {
    repair_generated_names_in_roots(path, &start_menu_roots())
}
unsafe fn taskbar_button_identity_at(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    point: POINT,
) -> Option<String> {
    let mut element = unsafe { automation.ElementFromPoint(point).ok()? };
    for _ in 0..6 {
        if let Ok(id) = unsafe { element.CurrentAutomationId() } {
            let id = id.to_string();
            if let Some(id) = id.strip_prefix("Appid: ") {
                return Some(id.to_owned());
            }
        }
        element = unsafe { walker.GetParentElement(&element).ok()? };
    }
    None
}

unsafe fn start_pin_drag(
    path: &Path,
    snapshot: &TaskbarSnapshot,
    point: POINT,
    press: Instant,
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
) -> Option<ActivePinDrag> {
    let source = snapshot
        .buttons
        .iter()
        .find(|button| contains_point(button.bounds, point))?;
    if !snapshot_precedes_press(snapshot.captured, press) {
        return None;
    }
    if unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) } != snapshot.root {
        return None;
    }
    let at_press = unsafe { taskbar_button_identity_at(automation, walker, point) };
    let mut cursor = POINT::default();
    let at_cursor = if unsafe { GetCursorPos(&mut cursor) }.is_ok() {
        unsafe { taskbar_button_identity_at(automation, walker, cursor) }
    } else {
        None
    };
    if !source_identity_verified(&source.identity, at_press.as_deref(), at_cursor.as_deref()) {
        return None;
    }
    let store = Store::load(path).ok()?;
    if store
        .groups
        .iter()
        .any(|group| app_id(&group.id).eq_ignore_ascii_case(&source.identity))
    {
        return None;
    }
    let targets = group_targets(snapshot, &store);
    if targets.is_empty() {
        return None;
    }
    let pinned = PathBuf::from(std::env::var_os("APPDATA")?)
        .join("Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar");
    let source_identity = source.identity.clone();
    let source_name = source.name.clone();
    let roots = start_menu_roots();
    let (source, temporary_source) = match pinned_shortcut_for_app_id(&pinned, &source_identity) {
        Ok(Some(source)) => (source, false),
        Ok(None) => {
            match start_menu_shortcut_by_identity_name(&roots, &source_identity, &source.name) {
                Ok(Some(source)) => (source, false),
                Ok(None) => {
                    if !source_identity.contains('!') {
                        return None;
                    }
                    let temporary = std::env::temp_dir()
                        .join(format!("TaskbarGroups-{}.lnk", uuid::Uuid::new_v4()));
                    create_shell_item_shortcut(
                        &format!("shell:AppsFolder\\{source_identity}"),
                        &temporary,
                    )
                    .ok()?;
                    (temporary, true)
                }
                Err(_) => {
                    return None;
                }
            }
        }
        Err(_) => {
            return None;
        }
    };
    let name = dragged_app_display_name(&source_name, &source);
    Some(ActivePinDrag {
        source,
        temporary_source,
        source_identity,
        name,
        state: taskbar_groups::PinDrag::new(targets.clone()),
        targets,
        root: snapshot.root,
        began: Instant::now(),
        popup_ready: None,
        image_requested: false,
        image_receiver: None,
        ghost: None,
        layout_path: std::env::temp_dir()
            .join(format!("TaskbarGroups-drop-{}.json", uuid::Uuid::new_v4())),
    })
}

unsafe fn spawn_group_popup(
    path: &Path,
    root: HWND,
    id: uuid::Uuid,
    anchor: windows::Win32::Foundation::RECT,
    source_name: &str,
    source_path: &Path,
    layout_path: &Path,
) -> Option<std::process::Child> {
    let store = Store::load(path).ok()?;
    let count = store
        .groups
        .into_iter()
        .find(|group| group.id == id)?
        .apps
        .len();
    let position = calculate_position(root, anchor, count, store.icons_per_row, 36.0)?;
    Command::new(std::env::current_exe().ok()?)
        .args([
            "--group",
            &id.to_string(),
            &position.0.to_string(),
            &position.1.to_string(),
            &anchor.left.to_string(),
            &anchor.top.to_string(),
            &anchor.right.to_string(),
            &anchor.bottom.to_string(),
        ])
        .arg("--drag-source")
        .arg(source_name)
        .arg(source_path)
        .arg("--drag-layout")
        .arg(layout_path)
        .spawn()
        .ok()
}

pub fn watch(path: &Path) {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = match CreateMutexW(None, false, w!("Local\\TaskbarGroupsWatcher.v2")) {
            Ok(m) => m,
            Err(_) => return,
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let repair_path = path.to_owned();
        std::thread::spawn(move || {
            let _ = repair_generated_names(&repair_path);
        });
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _window_monitor = crate::managed_windows::start(path.to_owned());
        let automation: IUIAutomation =
            match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                Ok(a) => a,
                Err(_) => return,
            };
        let walker = match automation.ControlViewWalker() {
            Ok(w) => w,
            Err(_) => return,
        };
        let mut last = None;
        let mut since = Instant::now();
        let mut opened = false;
        let mut popup: Option<std::process::Child> = None;
        let mut popup_group = None;
        let mouse_events = mouse_button_events();
        let mut snapshots: Vec<TaskbarSnapshot> = Vec::new();
        let mut previous_snapshots: Vec<TaskbarSnapshot> = Vec::new();
        let mut last_snapshot = Instant::now() - Duration::from_secs(1);
        let mut button_down = false;
        let mut drag: Option<ActivePinDrag> = None;
        loop {
            std::thread::sleep(Duration::from_millis(30));
            if installer_shutdown_requested() && drag.is_none() {
                break;
            }
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_err() {
                continue;
            }
            let root = GetAncestor(WindowFromPoint(point), GA_ROOT);
            let mut class = [0u16; 128];
            let len = GetClassNameW(root, &mut class).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..len]);
            if let Some(active) = drag.as_mut()
                && active.popup_ready.is_none()
                && let Some(child) = popup.as_ref()
                && process_has_visible_window(child.id())
            {
                active.popup_ready = Some(Instant::now());
            }
            for event in mouse_events.try_iter() {
                match event {
                    PinInput::Down(down_point, pressed_at) => {
                        button_down = true;
                        drag = snapshots.iter().chain(previous_snapshots.iter()).find_map(
                            |snapshot| {
                                start_pin_drag(
                                    path,
                                    snapshot,
                                    down_point,
                                    pressed_at,
                                    &automation,
                                    &walker,
                                )
                            },
                        );
                    }
                    PinInput::Cancel => {
                        if drag.take().is_some() && popup_group.is_some() {
                            if let Some(mut child) = popup.take() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            popup_group = None;
                        }
                    }
                    PinInput::Up(up_point, released_at) => {
                        button_down = false;
                        if let Some(mut active) = drag.take() {
                            active.ghost.take();
                            let over = popup.as_ref().and_then(|child| {
                                if !release_after_popup_ready(active.popup_ready, released_at) {
                                    return None;
                                }
                                Some((popup_group?, process_window_at_point(up_point, child.id())?))
                            });
                            if let Some(group) = active.state.release(over.map(|(group, _)| group))
                            {
                                let position = (|| -> std::io::Result<Option<(uuid::Uuid, taskbar_groups::DropSide)>> {
                                    let layout = crate::flyout_ui::DropLayout::read(&active.layout_path)?;
                                    if layout.group != group {
                                        return Err(std::io::Error::other("The group changed while dragging"));
                                    }
                                    let (_, window) = over.ok_or_else(|| std::io::Error::other("The group window closed"))?;
                                    let mut client = up_point;
                                    if !ScreenToClient(window, &mut client).as_bool() {
                                        return Err(std::io::Error::other("Could not locate the drop"));
                                    }
                                    Ok(layout.marker_at_client(client.x, client.y).map(|marker| (marker.target, marker.side)))
                                })();
                                let position = match position {
                                    Ok(position) => position,
                                    Err(error) => {
                                        let _ = rfd::MessageDialog::new()
                                            .set_title("Could not add pinned app")
                                            .set_description(format!("Could not determine the insertion position: {error}. Please try again."))
                                            .show();
                                        continue;
                                    }
                                };
                                match Store::move_pinned_shortcut_at(
                                    path,
                                    group,
                                    &active.name,
                                    &active.source,
                                    position,
                                    |source| {
                                        unpin_taskbar_shortcut(source, active.temporary_source)
                                    },
                                ) {
                                    Ok((_store, unpin, first_changed)) => {
                                        if let Err(error) = unpin {
                                            let _ = rfd::MessageDialog::new()
                                                .set_title("App added to group")
                                                .set_description(format!("The app was added, but Windows could not remove its original taskbar pin: {error}"))
                                                .show();
                                        }
                                        if first_changed
                                            && let Err(error) = sync_saved_group_icon(path, group)
                                        {
                                            let _ = rfd::MessageDialog::new()
                                                .set_title("Could not update group icon")
                                                .set_description(error)
                                                .show();
                                        }
                                    }
                                    Err(error) => {
                                        let _ = rfd::MessageDialog::new()
                                            .set_title("Could not add pinned app")
                                            .set_description(error.to_string())
                                            .show();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if last_snapshot.elapsed() >= Duration::from_millis(150) {
                let mut roots = Vec::new();
                if let Ok(primary) = FindWindowW(w!("Shell_TrayWnd"), None) {
                    roots.push(primary);
                }
                let mut last_secondary = None;
                while let Ok(secondary) =
                    FindWindowExW(None, last_secondary, w!("Shell_SecondaryTrayWnd"), None)
                {
                    roots.push(secondary);
                    last_secondary = Some(secondary);
                }
                let current: Vec<_> = roots
                    .into_iter()
                    .filter_map(|root| snapshot_taskbar(&automation, root))
                    .collect();
                if let Some(active) = drag.as_mut() {
                    let visible = current
                        .iter()
                        .find(|snapshot| snapshot.root == active.root)
                        .and_then(|snapshot| {
                            Store::load(path)
                                .ok()
                                .map(|store| group_targets(snapshot, &store))
                        });
                    if let Some(visible) =
                        visible.filter(|items| items.len() == active.targets.len())
                    {
                        active.state.set_visible_targets(visible);
                    } else {
                        drag = None;
                    }
                }
                if !button_down {
                    previous_snapshots = std::mem::replace(&mut snapshots, current);
                }
                last_snapshot = Instant::now();
            }
            if popup
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_some())
            {
                popup = None;
                popup_group = None;
                if let Some(active) = drag.as_mut() {
                    active.state.popup_closed();
                    active.popup_ready = None;
                }
            }
            if let Some(active) = drag.as_mut() {
                active.update_drag_image(
                    point,
                    class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd",
                );
            }
            if let Some(active) = drag.as_mut() {
                let target =
                    active
                        .state
                        .hover(point.x, point.y, active.began.elapsed().as_millis());
                if popup_group.is_some() && active.state.opened_group() != popup_group {
                    if let Some(mut child) = popup.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    popup_group = None;
                    active.popup_ready = None;
                }
                if let Some(id) = target {
                    let visible = snapshot_taskbar(&automation, active.root).and_then(|snapshot| {
                        if !target_position_is_unambiguous(
                            &snapshot,
                            point,
                            &active.source_identity,
                            &app_id(&id),
                        ) {
                            return None;
                        }
                        Store::load(path)
                            .ok()
                            .map(|store| group_targets(&snapshot, &store))
                    });
                    let Some(visible) = visible.filter(|items| items.len() == active.targets.len())
                    else {
                        active.state.popup_closed();
                        continue;
                    };
                    let Some(anchor) = visible
                        .iter()
                        .find(|item| item.group == id)
                        .map(|item| item.bounds)
                    else {
                        active.state.popup_closed();
                        continue;
                    };
                    active.state.set_visible_targets(visible);
                    active
                        .state
                        .hover(point.x, point.y, active.began.elapsed().as_millis());
                    if active.state.opened_group() != Some(id) {
                        continue;
                    }
                    if let Some(mut child) = popup.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    let b = anchor;
                    let _ = std::fs::remove_file(&active.layout_path);
                    popup = spawn_group_popup(
                        path,
                        active.root,
                        id,
                        windows::Win32::Foundation::RECT {
                            left: b.left,
                            top: b.top,
                            right: b.right,
                            bottom: b.bottom,
                        },
                        &active.name,
                        &active.source,
                        &active.layout_path,
                    );
                    popup_group = popup.as_ref().map(|_| id);
                    active.popup_ready = None;
                    if popup.is_none() {
                        active.state.popup_closed();
                    } else {
                        active.request_drag_image();
                    }
                }
                continue;
            }
            let mut current = None;
            let mut anchor = None;
            if (class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd")
                && let Ok(store) = Store::load(path)
                && let Some((id, rect)) = group_at_point(&automation, &walker, &store, point)
            {
                current = Some(id);
                anchor = Some(rect);
            }
            if current != last {
                last = current;
                since = Instant::now();
                opened = false;
            }
            if let Some(id) = current
                && !opened
                && since.elapsed() >= Duration::from_millis(550)
                && popup_group != Some(id)
            {
                if let Some(mut child) = popup.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let (count, icons_per_row) = Store::load(path)
                    .ok()
                    .map(|s| {
                        (
                            s.groups
                                .into_iter()
                                .find(|g| g.id == id)
                                .map(|g| g.apps.len())
                                .unwrap_or(0),
                            s.icons_per_row,
                        )
                    })
                    .unwrap_or((0, taskbar_groups::DEFAULT_ICONS_PER_ROW));
                let anchor = anchor.unwrap_or(windows::Win32::Foundation::RECT {
                    left: point.x - 1,
                    top: point.y - 1,
                    right: point.x + 1,
                    bottom: point.y + 1,
                });
                let position = calculate_position(root, anchor, count, icons_per_row, 0.0)
                    .unwrap_or((point.x, point.y - 100));
                popup = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--group",
                        &id.to_string(),
                        &position.0.to_string(),
                        &position.1.to_string(),
                        &anchor.left.to_string(),
                        &anchor.top.to_string(),
                        &anchor.right.to_string(),
                        &anchor.bottom.to_string(),
                    ])
                    .spawn()
                    .ok();
                popup_group = popup.as_ref().map(|_| id);
                opened = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installer_shutdown_signal_reaches_other_process_handles() {
        let name = format!("Local\\TaskbarGroupsShutdownTest-{}", uuid::Uuid::new_v4());
        let listener = ShutdownSignal::new(&name).unwrap();
        let sender = ShutdownSignal::new(&name).unwrap();
        assert!(!listener.is_set());
        sender.trigger().unwrap();
        assert!(listener.is_set());
        sender.reset().unwrap();
        assert!(!listener.is_set());
    }
    #[test]
    fn installer_waits_for_running_instances_before_replacing_the_executable() {
        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(&executable)
            .args([
                "--exact",
                "platform::tests::installer_wait_child",
                "--ignored",
            ])
            .spawn()
            .unwrap();
        let result = wait_for_other_instances(&executable, Duration::from_millis(100));
        assert!(
            result.is_err(),
            "a running process should prevent shutdown from succeeding"
        );
        child.wait().unwrap();
        assert!(wait_for_other_instances(&executable, Duration::from_secs(2)).is_ok());
    }
    #[test]
    #[ignore = "child process used by the installer shutdown test"]
    fn installer_wait_child() {
        std::thread::sleep(Duration::from_millis(800));
    }
    #[test]
    fn startup_status_respects_the_shortcut_and_windows_approval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Taskbar Groups.lnk");
        assert!(!startup_enabled_at(&path, None).unwrap());
        std::fs::write(&path, b"shortcut").unwrap();
        assert!(startup_enabled_at(&path, None).unwrap());
        assert!(startup_enabled_at(&path, Some(&[2, 0, 0, 0])).unwrap());
        assert!(startup_enabled_at(&path, Some(&[6, 0, 0, 0])).unwrap());
        assert!(!startup_enabled_at(&path, Some(&[3, 0, 0, 0])).unwrap());
        assert!(!startup_enabled_at(&path, Some(&[7, 0, 0, 0])).unwrap());
        assert!(startup_enabled_at(&path, Some(&[])).is_err());
    }
    #[test]
    fn windows_theme_color_formats_are_decoded_correctly() {
        assert_eq!(
            color_from_argb(0xaa112233),
            eframe::egui::Color32::from_rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(
            color_from_colorref(0x00332211),
            eframe::egui::Color32::from_rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(
            color_to_colorref(eframe::egui::Color32::from_rgb(0x11, 0x22, 0x33)),
            0x00332211
        );
    }
    #[test]
    fn unpin_rejects_a_missing_shortcut() {
        let path = tempfile::tempdir().unwrap().path().join("missing.lnk");
        assert!(unpin_taskbar_shortcut(&path, false).is_err());
    }
    #[test]
    fn shortcut_icon_is_the_application_icon_without_the_shortcut_arrow() {
        let _com = ComApartment::new();
        let dir = tempfile::tempdir().unwrap();
        let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
        let path = dir.path().join("Notepad.lnk");
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(path.as_os_str()).as_ptr()), true)
                .unwrap();
        }
        let shortcut_icon = icon(&path.to_string_lossy()).unwrap();
        let application_icon = icon(&exe.to_string_lossy()).unwrap();
        assert_eq!(shortcut_icon.pixels, application_icon.pixels);
    }
    #[test]
    fn shortcut_keeps_its_custom_icon_without_the_shortcut_arrow() {
        use image::ImageEncoder;
        let _com = ComApartment::new();
        let dir = tempfile::tempdir().unwrap();
        let icon_file = dir.path().join("custom.ico");
        let mut encoded = Vec::new();
        let rgba: Vec<u8> = [240, 20, 30, 255].repeat(32 * 32);
        image::codecs::ico::IcoEncoder::new(&mut encoded)
            .write_image(&rgba, 32, 32, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&icon_file, encoded).unwrap();
        let shortcut = dir.path().join("custom.lnk");
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            link.SetIconLocation(PCWSTR(wide(icon_file.as_os_str()).as_ptr()), 0)
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(shortcut.as_os_str()).as_ptr()), true)
                .unwrap();
        }
        let image = icon(&shortcut.to_string_lossy()).unwrap();
        let center = image.pixels[16 * 32 + 16];
        assert!(center.r() > 200 && center.g() < 50 && center.b() < 60);
    }
    #[test]
    fn start_menu_fallback_rejects_ambiguous_or_mismatched_names() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("Example App.lnk"), b"not a link").unwrap();
        std::fs::write(second.path().join("Example App.lnk"), b"not a link").unwrap();
        let dirs = vec![first.path().to_owned(), second.path().to_owned()];
        assert_eq!(
            start_menu_shortcut_by_identity_name(&dirs, "example.app", "Example App pinned")
                .unwrap(),
            None
        );
        assert_eq!(
            start_menu_shortcut_by_identity_name(&dirs[..1], "other.app", "Example App pinned")
                .unwrap(),
            None
        );
        assert_eq!(
            start_menu_shortcut_by_identity_name(&dirs[..1], "example.app", "Example App").unwrap(),
            None
        );
    }
    #[test]
    fn guid_drag_name_can_use_the_shortcut_target_description_without_scanning() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("3f4de768-7086-4705-a1c9-58c9a6c02568.lnk");
            let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(path.as_os_str()).as_ptr()), true)
                .unwrap();
            assert!(shortcut_target_description(&path).is_some_and(|name| !name.is_empty()));
            assert_ne!(
                dragged_app_display_name("3f4de768-7086-4705-a1c9-58c9a6c02568 pinned", &path),
                "Application"
            );
        }
    }
    #[test]
    fn repairs_a_generated_group_name_using_the_unique_matching_shortcut() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let shortcuts = dir.path().join("shortcuts");
            let menu = dir.path().join("start-menu");
            std::fs::create_dir(&shortcuts).unwrap();
            std::fs::create_dir(&menu).unwrap();
            let original = menu.join("Tera Term.lnk");
            let exe = std::env::current_exe().unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(original.as_os_str()).as_ptr()), true)
                .unwrap();
            let imported = shortcuts.join("b89e34b1-669b-4623-a3da-3f722309a0a1.lnk");
            std::fs::copy(original, &imported).unwrap();
            let path = dir.path().join("groups.json");
            let mut store = Store::default();
            let group = store.add_group("Work").unwrap();
            store
                .add_app(
                    group,
                    "b89e34b1-669b-4623-a3da-3f722309a0a1",
                    &imported.to_string_lossy(),
                )
                .unwrap();
            store.save(&path).unwrap();
            assert_eq!(repair_generated_names_in_roots(&path, &[menu]).unwrap(), 1);
            assert_eq!(
                Store::load(&path).unwrap().groups[0].apps[0].name,
                "Tera Term"
            );
        }
    }
    #[test]
    fn incoming_drag_prefers_the_taskbar_application_name_over_generated_filename() {
        let source = Path::new("C:\\shortcuts\\3f4de768-7086-4705-a1c9-58c9a6c02568.lnk");
        assert_eq!(
            dragged_app_display_name("Tera Term pinned", source),
            "Tera Term"
        );
        assert_eq!(
            dragged_app_display_name("Terminal - 1 running window pinned", source),
            "Terminal"
        );
    }
    #[test]
    fn guid_shortcut_recovers_the_unique_matching_start_menu_name() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let source = dir.path().join("3f4de768-7086-4705-a1c9-58c9a6c02568.lnk");
            let menu = dir.path().join("menu");
            std::fs::create_dir(&menu).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(source.as_os_str()).as_ptr()), true)
                .unwrap();
            std::fs::copy(&source, menu.join("Tera Term.lnk")).unwrap();
            assert_eq!(
                friendly_name_from_shortcuts(&source, std::slice::from_ref(&menu)).as_deref(),
                Some("Tera Term")
            );
            std::fs::copy(&source, menu.join("Other name.lnk")).unwrap();
            assert_eq!(friendly_name_from_shortcuts(&source, &[menu]), None);
        }
    }
    #[test]
    fn taskbar_accessibility_suffix_is_not_part_of_the_application_name() {
        assert_eq!(
            source_label("Terminal - 1 running window pinned"),
            "Terminal"
        );
        assert_eq!(source_label("Kindle pinned"), "Kindle");
    }
    #[test]
    fn dragged_pin_can_be_verified_at_its_moved_pointer_location() {
        assert!(source_identity_verified("Editor", Some("Editor"), None));
        assert!(!source_identity_verified(
            "Editor",
            Some("Other"),
            Some("Editor")
        ));
        assert!(source_identity_verified("Editor", None, Some("Editor")));
        assert!(!source_identity_verified(
            "Editor",
            Some("Other"),
            Some("Different")
        ));
        assert!(!source_identity_verified("Editor", None, None));
    }
    #[test]
    fn a_regular_app_in_the_old_group_position_is_not_a_group_target() {
        let rect = windows::Win32::Foundation::RECT {
            left: 10,
            top: 20,
            right: 50,
            bottom: 60,
        };
        let point = POINT { x: 20, y: 30 };
        let snapshot = |identity: &str| TaskbarSnapshot {
            root: HWND::default(),
            captured: Instant::now(),
            buttons: vec![TaskbarButton {
                identity: identity.into(),
                name: String::new(),
                bounds: rect,
            }],
        };
        assert!(target_position_is_unambiguous(
            &snapshot("Source.App"),
            point,
            "Source.App",
            "Group.App"
        ));
        assert!(target_position_is_unambiguous(
            &snapshot("Group.App"),
            point,
            "Source.App",
            "Group.App"
        ));
        assert!(!target_position_is_unambiguous(
            &snapshot("Other.App"),
            point,
            "Source.App",
            "Group.App"
        ));
    }
    #[test]
    fn a_snapshot_taken_after_the_mouse_press_is_never_used() {
        let captured = Instant::now();
        assert!(snapshot_precedes_press(
            captured,
            captured + Duration::from_millis(200)
        ));
        assert!(!snapshot_precedes_press(
            captured,
            captured - Duration::from_millis(1)
        ));
        assert!(snapshot_precedes_press(
            captured,
            captured + Duration::from_millis(400)
        ));
        assert!(!snapshot_precedes_press(
            captured,
            captured + Duration::from_millis(600)
        ));
    }
    #[test]
    fn incoming_drag_overlay_tracks_visibility_and_is_destroyed_with_the_drag() {
        unsafe {
            let (sender, receiver) = mpsc::channel();
            sender
                .send(Some(eframe::egui::ColorImage::filled(
                    [32, 32],
                    eframe::egui::Color32::WHITE,
                )))
                .unwrap();
            let mut drag = ActivePinDrag {
                source: PathBuf::from("unused.lnk"),
                temporary_source: false,
                source_identity: "example".into(),
                name: "Example".into(),
                state: taskbar_groups::PinDrag::new(Vec::new()),
                targets: Vec::new(),
                root: HWND::default(),
                began: Instant::now(),
                popup_ready: None,
                image_requested: true,
                image_receiver: Some(receiver),
                ghost: None,
                layout_path: std::env::temp_dir().join(format!(
                    "TaskbarGroups-test-drop-{}.json",
                    uuid::Uuid::new_v4()
                )),
            };
            let point = POINT {
                x: -32000,
                y: -32000,
            };
            drag.update_drag_image(point, false);
            let hwnd = drag.ghost.as_ref().unwrap().window_handle_for_test();
            assert!(!IsWindowVisible(hwnd).as_bool());
            drag.popup_ready = Some(Instant::now());
            drag.update_drag_image(point, false);
            assert!(IsWindowVisible(hwnd).as_bool());
            drag.update_drag_image(point, true);
            assert!(!IsWindowVisible(hwnd).as_bool());
            drag.update_drag_image(point, false);
            assert!(IsWindowVisible(hwnd).as_bool());
            drop(drag);
            assert!(!IsWindow(Some(hwnd)).as_bool());
        }
    }
    #[test]
    fn incoming_drag_image_appears_only_outside_taskbar_after_flyout_opens() {
        assert!(!should_show_pin_drag_image(false, true));
        assert!(!should_show_pin_drag_image(false, false));
        assert!(!should_show_pin_drag_image(true, true));
        assert!(should_show_pin_drag_image(true, false));
    }
    #[test]
    fn release_is_valid_only_after_the_flyout_was_observed_visible() {
        let seen = Instant::now();
        assert!(!release_after_popup_ready(None, seen));
        assert!(!release_after_popup_ready(
            Some(seen),
            seen - Duration::from_millis(1)
        ));
        assert!(release_after_popup_ready(Some(seen), seen));
    }
    #[test]
    fn resolves_implicit_taskbar_id_to_the_unique_matching_shortcut() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("notepad.exe");
            let make_link = |name: &str| {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                let exe = wide(exe.as_os_str());
                link.SetPath(PCWSTR(exe.as_ptr())).unwrap();
                let persist: IPersistFile = link.cast().unwrap();
                let dest = dir.path().join(name);
                let file = wide(dest.as_os_str());
                persist.Save(PCWSTR(file.as_ptr()), true).unwrap();
                dest
            };
            let link = make_link("Editor.lnk");
            let id = "{F38BF404-1D43-42F2-9305-67DE0B28FC23}\\System32\\notepad.exe";
            assert_eq!(
                pinned_shortcut_for_app_id(dir.path(), id).unwrap(),
                Some(link)
            );
            make_link("Editor alternate.lnk");
            assert!(pinned_shortcut_for_app_id(dir.path(), id).is_err());
        }
    }
    #[test]
    fn created_shortcut_alone_does_not_confirm_a_pin() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let source = dir.path().join("original.lnk");
            let pinned = dir.path().join("pins");
            std::fs::create_dir(&pinned).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(source.as_os_str()).as_ptr()), true)
                .unwrap();
            let previous = PinBaseline {
                matching_paths: Some(Default::default()),
                pinned_identities: Some(Default::default()),
            };
            let button = TaskbarButton {
                identity: "runtime.app.id".into(),
                name: "Different title pinned".into(),
                bounds: Default::default(),
            };
            assert!(!new_pin_is_registered(
                &pinned,
                &source,
                &previous,
                std::slice::from_ref(&button)
            ));
            std::fs::copy(&source, pinned.join("App.lnk")).unwrap();
            assert!(!new_pin_is_registered(&pinned, &source, &previous, &[]));
            assert!(!new_pin_is_registered(
                &pinned,
                &source,
                &previous,
                std::slice::from_ref(&button)
            ));
            let button = TaskbarButton {
                name: "App pinned".into(),
                ..button
            };
            assert!(new_pin_is_registered(
                &pinned,
                &source,
                &previous,
                &[button]
            ));
        }
    }
    #[test]
    fn a_new_pin_is_confirmed_only_with_one_new_visible_pinned_identity() {
        let before = std::collections::HashSet::from(["existing.app".to_string()]);
        let button = |id: &str, name: &str| TaskbarButton {
            identity: id.into(),
            name: name.into(),
            bounds: Default::default(),
        };
        assert!(has_one_new_pinned_identity(
            &before,
            &[
                button("Existing.App", "Existing pinned"),
                button("new.app", "Different display name pinned")
            ]
        ));
        assert!(!has_one_new_pinned_identity(
            &before,
            &[button("existing.app", "Existing pinned")]
        ));
        assert!(!has_one_new_pinned_identity(
            &before,
            &[
                button("new.app", "New pinned"),
                button("other.app", "Other pinned")
            ]
        ));
        assert!(!has_one_new_pinned_identity(
            &before,
            &[button("new.app", "New running window")]
        ));
    }
    #[test]
    fn newly_created_matching_shortcut_is_detected_without_taskbar_labels() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let source = dir.path().join("original.lnk");
            let pinned = dir.path().join("pins");
            std::fs::create_dir(&pinned).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(source.as_os_str()).as_ptr()), true)
                .unwrap();
            let pin = pinned.join("Unexpected taskbar label.lnk");
            std::fs::copy(&source, &pin).unwrap();
            assert!(matching_new_pin_exists(
                &pinned,
                &source,
                &Default::default()
            ));
            assert!(!matching_new_pin_exists(
                &pinned,
                &source,
                &std::collections::HashSet::from([pin])
            ));
            let other = dir.path().join("other.exe");
            std::fs::write(&other, b"different").unwrap();
            assert!(!matching_new_pin_exists(
                &pinned,
                &other,
                &Default::default()
            ));
        }
    }
    #[test]
    fn verifies_pin_when_application_identity_differs_from_shortcut_identity() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let source = dir.path().join("original.lnk");
            let pinned = dir.path().join("pins");
            std::fs::create_dir(&pinned).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(source.as_os_str()).as_ptr()), true)
                .unwrap();
            std::fs::copy(&source, pinned.join("Example Studio.lnk")).unwrap();
            let button = |name: &str| TaskbarButton {
                identity: "custom.runtime.id".into(),
                name: name.into(),
                bounds: Default::default(),
            };
            assert!(pinned_shortcut_matches_visible_name(
                &[button("Example Studio pinned")],
                &pinned,
                &source,
                &Default::default(),
                &Default::default()
            ));
            assert!(!pinned_shortcut_matches_visible_name(
                &[button("Example Studio")],
                &pinned,
                &source,
                &Default::default(),
                &Default::default()
            ));
            assert!(pinned_shortcut_matches_visible_name(
                &[
                    button("Example Studio pinned"),
                    button("Example Studio pinned")
                ],
                &pinned,
                &source,
                &Default::default(),
                &Default::default()
            ));
            let different = TaskbarButton {
                identity: "another.identity".into(),
                ..button("Example Studio pinned")
            };
            assert!(!pinned_shortcut_matches_visible_name(
                &[button("Example Studio pinned"), different],
                &pinned,
                &source,
                &Default::default(),
                &Default::default()
            ));
            let other = dir.path().join("other.exe");
            std::fs::write(&other, b"different").unwrap();
            assert!(!pinned_shortcut_matches_visible_name(
                &[button("Example Studio pinned")],
                &pinned,
                &other,
                &Default::default(),
                &Default::default()
            ));
            assert!(!pinned_shortcut_matches_visible_name(
                &[button("Example Studio pinned")],
                &pinned,
                &source,
                &std::collections::HashSet::from([pinned.join("Example Studio.lnk")]),
                &Default::default()
            ));
            assert!(!pinned_shortcut_matches_visible_name(
                &[button("Example Studio pinned")],
                &pinned,
                &source,
                &Default::default(),
                &std::collections::HashSet::from(["custom.runtime.id".to_string()])
            ));
        }
    }
    #[test]
    fn pin_verification_compares_the_shortcut_target_and_arguments() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let make_link = |name: &str, args: &str| {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                    .unwrap();
                link.SetArguments(PCWSTR(wide(args).as_ptr())).unwrap();
                let persist: IPersistFile = link.cast().unwrap();
                let path = dir.path().join(name);
                persist
                    .Save(PCWSTR(wide(path.as_os_str()).as_ptr()), true)
                    .unwrap();
                path
            };
            let first = make_link("First.lnk", "--profile one");
            let same = make_link("Same.lnk", "--profile one");
            let different = make_link("Other.lnk", "--profile two");
            assert!(same_launch_target(&first, &same));
            assert!(!same_launch_target(&first, &different));
            assert!(!same_launch_target(&first, &exe));
        }
    }
    #[test]
    fn pin_verification_rejects_different_explicit_application_ids() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let make_link = |name: &str, identity: &str| {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                    .unwrap();
                let props: IPropertyStore = link.cast().unwrap();
                let key = PROPERTYKEY {
                    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                    pid: 5,
                };
                props.SetValue(&key, &PROPVARIANT::from(identity)).unwrap();
                props.Commit().unwrap();
                let persist: IPersistFile = link.cast().unwrap();
                let path = dir.path().join(name);
                persist
                    .Save(PCWSTR(wide(path.as_os_str()).as_ptr()), true)
                    .unwrap();
                path
            };
            assert!(!same_launch_target(
                &make_link("One.lnk", "App.One"),
                &make_link("Two.lnk", "App.Two")
            ));
        }
    }
    #[test]
    fn matching_shortcuts_must_preserve_working_directory_and_explicit_identity() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let create = |name: &str, work: &Path, identity: Option<&str>| {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                    .unwrap();
                link.SetWorkingDirectory(PCWSTR(wide(work.as_os_str()).as_ptr()))
                    .unwrap();
                if let Some(identity) = identity {
                    let props: IPropertyStore = link.cast().unwrap();
                    let key = PROPERTYKEY {
                        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                        pid: 5,
                    };
                    props.SetValue(&key, &PROPVARIANT::from(identity)).unwrap();
                    props.Commit().unwrap();
                }
                let persist: IPersistFile = link.cast().unwrap();
                let path = dir.path().join(name);
                persist
                    .Save(PCWSTR(wide(path.as_os_str()).as_ptr()), true)
                    .unwrap();
                path
            };
            let plain = create("Plain.lnk", dir.path(), None);
            let other_work = create("Other.lnk", dir.path().parent().unwrap(), None);
            let identified = create("Identified.lnk", dir.path(), Some("App.One"));
            assert!(!same_launch_target(&plain, &other_work));
            assert!(!same_launch_target(&plain, &identified));
        }
    }
    #[test]
    fn native_drag_cancels_unless_released_on_the_taskbar() {
        use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP};
        assert_eq!(
            taskbar_drag_decision(false, true, false),
            windows::core::HRESULT(0)
        );
        assert_eq!(taskbar_drag_decision(false, false, true), DRAGDROP_S_DROP);
        assert_eq!(
            taskbar_drag_decision(false, false, false),
            DRAGDROP_S_CANCEL
        );
        assert_eq!(taskbar_drag_decision(true, true, true), DRAGDROP_S_CANCEL);
    }
    #[test]
    fn cancelling_an_export_removes_only_the_staged_shortcut() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.lnk");
        std::fs::write(&source, b"shortcut").unwrap();
        let exported = named_drag_shortcut(&source, "Editor", &dir.path().join("exports")).unwrap();
        let folder = exported.parent().unwrap().to_owned();
        discard_cancelled_shortcut(&exported);
        assert!(!exported.exists());
        assert!(!folder.exists());
        assert!(source.exists());
    }
    #[test]
    fn named_export_with_mixed_path_separators_is_a_valid_shell_drag_source() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
            let source = dir.path().join("source.lnk");
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(source.as_os_str()).as_ptr()), true)
                .unwrap();
            let named =
                named_drag_shortcut(&source, "Explorer", &dir.path().join("nested/exported"))
                    .unwrap();
            assert!(named.is_file());
            shell_drag_data_object(&named).expect("Shell must accept the staged shortcut path");
        }
    }
    #[test]
    fn outgoing_shortcut_uses_the_app_name_and_preserves_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("b2cbddc8-1a72-4844-a5e7-dbf08f330ab5.lnk");
        std::fs::write(&source, b"shortcut with launch arguments").unwrap();
        let exported =
            named_drag_shortcut(&source, "Tera Term", &dir.path().join("exports")).unwrap();
        assert_eq!(exported.file_name().unwrap(), "Tera Term.lnk");
        assert_eq!(
            std::fs::read(exported).unwrap(),
            std::fs::read(source).unwrap()
        );
    }
    #[test]
    fn outgoing_shortcut_name_cannot_escape_the_export_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("input.lnk");
        std::fs::write(&source, b"shortcut").unwrap();
        let root = dir.path().join("exports");
        let output = named_drag_shortcut(&source, "..\\Other/App", &root).unwrap();
        assert!(output.starts_with(root));
        assert!(output.is_file());
    }
    #[test]
    fn drag_feedback_distinguishes_an_accepted_drop_from_no_drop() {
        assert!(!drag_effect_is_accepted(DROPEFFECT_NONE, true));
        assert!(!drag_effect_is_accepted(DROPEFFECT_LINK, false));
        assert!(drag_effect_is_accepted(DROPEFFECT_LINK, true));
        assert!(drag_effect_is_accepted(DROPEFFECT_COPY, true));
    }
    #[test]
    fn shell_drag_data_can_use_the_application_icon_as_its_image() {
        let _com = ComApartment::new();
        let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("explorer.exe");
        let data = shell_drag_data_object(&exe).unwrap();
        add_drag_image(&data, &exe).unwrap();
    }
    #[test]
    fn shell_drag_object_contains_the_original_application_file() {
        unsafe {
            let _com = ComApartment::new();
            let exe = std::env::current_exe().unwrap();
            let object = shell_drag_data_object(&exe).unwrap();
            let format = FORMATETC {
                cfFormat: windows::Win32::System::Ole::CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            assert!(object.QueryGetData(&format).is_ok());
        }
    }
    #[test]
    fn packaged_unpin_targets_the_original_shell_item_not_the_copied_shortcut() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let shortcut = dir.path().join("Apps.lnk");
            create_shell_item_shortcut("shell:AppsFolder", &shortcut).unwrap();
            let item = shell_item_for_unpin(&shortcut, true).unwrap();
            let name = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).unwrap();
            let actual = name.to_string().unwrap();
            CoTaskMemFree(Some(name.0.cast()));
            assert!(
                actual
                    .to_ascii_lowercase()
                    .contains("4234d49b-0245-4df3-b780-3893943456e1"),
                "{actual}"
            );
        }
    }
    #[test]
    fn desktop_shortcut_unpin_targets_the_shortcut_itself() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let shortcut = dir.path().join("Apps.lnk");
            create_shell_item_shortcut("shell:AppsFolder", &shortcut).unwrap();
            let item = shell_item_for_unpin(&shortcut, false).unwrap();
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).unwrap();
            let actual = PathBuf::from(name.to_string().unwrap());
            CoTaskMemFree(Some(name.0.cast()));
            assert_eq!(
                std::fs::canonicalize(actual).unwrap(),
                std::fs::canonicalize(shortcut).unwrap()
            );
        }
    }
    #[test]
    fn shortcut_can_preserve_a_non_filesystem_shell_item() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let shortcut = dir.path().join("Apps.lnk");
            create_shell_item_shortcut("shell:AppsFolder", &shortcut).unwrap();
            let store_path = dir.path().join("groups.json");
            let mut store = Store::default();
            let group = store.add_group("Apps").unwrap();
            store.save(&store_path).unwrap();
            let saved =
                Store::import_pinned_shortcut(&store_path, group, "Apps", &shortcut).unwrap();
            let imported = PathBuf::from(&saved.groups[0].apps[0].path);
            std::fs::remove_file(shortcut).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Load(PCWSTR(wide(imported.as_os_str()).as_ptr()), STGM_READ)
                .unwrap();
            let pidl = link.GetIDList().unwrap();
            let item: IShellItem = SHCreateItemFromIDList(pidl).unwrap();
            CoTaskMemFree(Some(pidl.cast()));
            let actual = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).unwrap();
            let value = actual.to_string().unwrap();
            CoTaskMemFree(Some(actual.0.cast()));
            assert!(
                value
                    .to_ascii_lowercase()
                    .contains("4234d49b-0245-4df3-b780-3893943456e1"),
                "{value}"
            );
        }
    }
    #[test]
    fn resolves_absolute_executable_identity_to_matching_pinned_shortcut() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let exe =
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/notepad.exe");
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            let shortcut = dir.path().join("Editor.lnk");
            persist
                .Save(PCWSTR(wide(shortcut.as_os_str()).as_ptr()), true)
                .unwrap();
            assert_eq!(
                pinned_shortcut_for_app_id(dir.path(), &exe.to_string_lossy()).unwrap(),
                Some(shortcut)
            );
        }
    }
    #[test]
    fn implicit_identity_does_not_match_a_link_with_a_different_explicit_id() {
        unsafe {
            let _com = ComApartment::new();
            let dir = tempfile::tempdir().unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let exe =
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/notepad.exe");
            link.SetPath(PCWSTR(wide(exe.as_os_str()).as_ptr()))
                .unwrap();
            let properties: IPropertyStore = link.cast().unwrap();
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            properties
                .SetValue(&key, &PROPVARIANT::from("Other.Application"))
                .unwrap();
            properties.Commit().unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(dir.path().join("Editor.lnk")).as_ptr()), true)
                .unwrap();
            let id = "{F38BF404-1D43-42F2-9305-67DE0B28FC23}\\System32\\notepad.exe";
            assert_eq!(pinned_shortcut_for_app_id(dir.path(), id).unwrap(), None);
        }
    }
    #[test]
    fn resolves_explicit_taskbar_identity_without_guessing_by_name() {
        let _com = ComApartment::new();
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        let group = store.add_group("Work").unwrap();
        let link = create_shortcut_in_dir(&store.groups[0], dir.path()).unwrap();
        let identity = app_id(&group);
        assert_eq!(
            pinned_shortcut_for_app_id(dir.path(), &identity).unwrap(),
            Some(link.clone())
        );
        assert_eq!(
            pinned_shortcut_for_app_id(dir.path(), "Different.Identity").unwrap(),
            None
        );
        std::fs::copy(link, dir.path().join("duplicate.lnk")).unwrap();
        assert!(pinned_shortcut_for_app_id(dir.path(), &identity).is_err());
    }
    #[test]
    fn shortcut_keeps_its_unique_group_identity_and_launcher_arguments() {
        unsafe {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::default();
            store.add_group("Work").unwrap();
            let group = &store.groups[0];
            let path = create_shortcut_in_dir(group, dir.path()).unwrap();
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            let filename = wide(path.as_os_str());
            persist.Load(PCWSTR(filename.as_ptr()), STGM_READ).unwrap();
            let mut args = [0u16; 256];
            link.GetArguments(&mut args).unwrap();
            let arg = String::from_utf16_lossy(&args[..args.iter().position(|&x| x == 0).unwrap()]);
            assert_eq!(arg, format!("--group {}", group.id));
            let properties: IPropertyStore = link.cast().unwrap();
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            let value = properties.GetValue(&key).unwrap();
            assert_eq!(
                windows::core::BSTR::try_from(&value).unwrap().to_string(),
                app_id(&group.id)
            );
        }
    }

    #[test]
    fn shell_item_resolves_a_shortcut_in_a_path_with_spaces() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dir = tempfile::tempdir().unwrap();
            let folder = dir.path().join("Taskbar Groups");
            std::fs::create_dir(&folder).unwrap();
            let path = folder.join("Design Tools.lnk");
            std::fs::write(&path, b"test").unwrap();
            let pidl = shell_item_for_path(&path).unwrap();
            let mut result = [0u16; 260];
            assert!(SHGetPathFromIDListW(pidl, &mut result).as_bool());
            let len = result.iter().position(|&c| c == 0).unwrap();
            let actual = std::path::PathBuf::from(String::from_utf16_lossy(&result[..len]));
            assert_eq!(
                std::fs::canonicalize(actual).unwrap(),
                std::fs::canonicalize(path).unwrap()
            );
            CoTaskMemFree(Some(pidl.cast()));
        }
    }

    #[test]
    fn shell_item_rejects_a_missing_shortcut() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dir = tempfile::tempdir().unwrap();
            assert!(shell_item_for_path(&dir.path().join("missing shortcut.lnk")).is_err());
        }
    }

    #[test]
    #[ignore = "opens Explorer; set TASKBAR_GROUPS_REVEAL_TEST_PATH to an existing file"]
    fn reveal_existing_shortcut_in_explorer() {
        let path = std::env::var_os("TASKBAR_GROUPS_REVEAL_TEST_PATH").expect("set test path");
        reveal_shortcut(Path::new(&path)).unwrap();
    }

    #[test]
    #[ignore = "reads the user's current taskbar and group configuration"]
    fn matches_a_live_pinned_group_at_its_taskbar_button() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let path = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
                .join("TaskbarGroups/groups.json");
            let store = Store::load(&path).unwrap();
            let group = store.groups.first().expect("create and pin a group first");
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).unwrap();
            let root = FindWindowW(w!("Shell_TrayWnd"), None).unwrap();
            let items = automation
                .ElementFromHandle(root)
                .unwrap()
                .FindAll(
                    TreeScope_Descendants,
                    &automation.CreateTrueCondition().unwrap(),
                )
                .unwrap();
            let expected = format!("Appid: {}", app_id(&group.id));
            let item = (0..items.Length().unwrap())
                .filter_map(|i| items.GetElement(i).ok())
                .find(|item| {
                    item.CurrentAutomationId()
                        .ok()
                        .is_some_and(|id| id.to_string().eq_ignore_ascii_case(&expected))
                })
                .expect("group is not pinned to the primary taskbar");
            let rect = item.CurrentBoundingRectangle().unwrap();
            println!(
                "Pinned group bounds: {}, {}, {}, {}",
                rect.left, rect.top, rect.right, rect.bottom
            );
            let point = POINT {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
            };
            if let Ok(under) = automation.ElementFromPoint(point) {
                println!(
                    "Element at button center: name={:?} class={:?} automation_id={:?}",
                    under.CurrentName(),
                    under.CurrentClassName(),
                    under.CurrentAutomationId()
                );
            }
            let walker = automation.ControlViewWalker().unwrap();
            assert_eq!(
                group_at_point(&automation, &walker, &store, point).map(|v| v.0),
                Some(group.id)
            );
            assert!(
                taskbar_position_for_group(group, taskbar_groups::DEFAULT_ICONS_PER_ROW).is_some()
            );
        }
    }

    #[test]
    #[ignore = "reads the user's configured application icons"]
    fn configured_application_icons_have_visible_pixels() {
        let path = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
            .join("TaskbarGroups/groups.json");
        let store = Store::load(&path).unwrap();
        for app in store.groups.iter().flat_map(|g| &g.apps) {
            let image =
                icon(&app.path).unwrap_or_else(|| panic!("Could not load icon for {}", app.name));
            assert!(
                image.pixels.iter().any(|pixel| pixel.a() != 0),
                "Icon for {} is transparent",
                app.name
            );
        }
    }

    #[test]
    fn group_shortcut_uses_first_app_icon_and_preserves_identity() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::default();
            store.add_group("Work").unwrap();
            let id = store.groups[0].id;
            let explorer = PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("explorer.exe");
            store
                .add_app(id, "Explorer", &explorer.to_string_lossy())
                .unwrap();
            let shortcut = create_shortcut_in_dir(&store.groups[0], dir.path()).unwrap();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Load(PCWSTR(wide(shortcut.as_os_str()).as_ptr()), STGM_READ)
                .unwrap();
            let mut path = [0u16; 1024];
            let mut index = 0;
            link.GetIconLocation(&mut path, &mut index).unwrap();
            let end = path.iter().position(|&c| c == 0).unwrap();
            let icon_file = PathBuf::from(String::from_utf16_lossy(&path[..end]));
            assert_eq!(icon_file.extension().unwrap(), "ico");
            let decoded = image::open(icon_file).unwrap().into_rgba8();
            assert!(decoded.pixels().any(|p| p[3] > 0));
            let properties: IPropertyStore = link.cast().unwrap();
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            assert_eq!(
                windows::core::BSTR::try_from(&properties.GetValue(&key).unwrap())
                    .unwrap()
                    .to_string(),
                app_id(&id)
            );
        }
    }

    #[test]
    fn updating_a_pinned_shortcut_changes_only_the_matching_group() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::default();
            let first = store.add_group("First").unwrap();
            store.add_group("Other").unwrap();
            let matching = create_shortcut_in_dir(&store.groups[0], dir.path()).unwrap();
            let other = create_shortcut_in_dir(&store.groups[1], dir.path()).unwrap();
            let explorer = PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("explorer.exe");
            store
                .add_app(first, "Explorer", &explorer.to_string_lossy())
                .unwrap();
            let (icon, index) = group_icon_location(&store.groups[0], dir.path()).unwrap();
            assert_eq!(
                update_matching_shortcut(&matching, &store.groups[0], &icon, index).unwrap(),
                Some(true)
            );
            assert_eq!(
                update_matching_shortcut(&other, &store.groups[0], &icon, index).unwrap(),
                None
            );
            let read_icon = |file: &Path| {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                let persist: IPersistFile = link.cast().unwrap();
                persist
                    .Load(PCWSTR(wide(file.as_os_str()).as_ptr()), STGM_READ)
                    .unwrap();
                let mut path = [0u16; 1024];
                let mut index = 0;
                link.GetIconLocation(&mut path, &mut index).unwrap();
                PathBuf::from(String::from_utf16_lossy(
                    &path[..path.iter().position(|&c| c == 0).unwrap()],
                ))
            };
            assert_eq!(read_icon(&matching), icon);
            assert_ne!(read_icon(&other), icon);
        }
    }

    #[test]
    fn synchronizes_a_pin_even_when_its_shortcut_was_renamed() {
        let _com = ComApartment::new();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let pins = dir.path().join("pins");
        std::fs::create_dir_all(&pins).unwrap();
        let mut store = Store::default();
        let id = store.add_group("Work").unwrap();
        let original = create_shortcut_in_dir(&store.groups[0], &source).unwrap();
        let pin = pins.join("Renamed shortcut.lnk");
        std::fs::copy(original, &pin).unwrap();
        let explorer = PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("explorer.exe");
        store
            .add_app(id, "Explorer", &explorer.to_string_lossy())
            .unwrap();
        assert!(sync_group_icon_in_dirs(&store.groups[0], &source, &pins).unwrap());
        assert!(!sync_group_icon_in_dirs(&store.groups[0], &source, &pins).unwrap());
        assert_eq!(
            shortcut_app_id(&pin).unwrap().as_deref(),
            Some(app_id(&id).as_str())
        );
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Load(PCWSTR(wide(pin.as_os_str()).as_ptr()), STGM_READ)
                .unwrap();
            let mut icon = [0u16; 1024];
            let mut index = 0;
            link.GetIconLocation(&mut icon, &mut index).unwrap();
            let len = icon.iter().position(|&value| value == 0).unwrap();
            assert_eq!(
                PathBuf::from(String::from_utf16_lossy(&icon[..len])),
                group_icon_location(&store.groups[0], &source).unwrap().0
            );
        }
    }
}
