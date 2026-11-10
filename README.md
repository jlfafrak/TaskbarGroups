# Taskbar Groups

A Windows 11 launcher written in Rust. Create separate groups, add application files or Windows shortcuts, and pin a launcher shortcut for each group. The background watcher uses Windows UI Automation to detect a hover over a group button and opens its flyout. You can also click a pinned group to open it.

## Build and run

Install the Rust MSVC toolchain and Microsoft C++ build tools, then run:

```powershell
cargo build --release
.\target\release\taskbar-groups.exe
```

The repository pins Rust 1.99.1 in `rust-toolchain.toml`. Keep the executable in a stable location: generated shortcuts reference its current path. Moving or rebuilding into a different location requires recreating and repinning those shortcuts.

## Use

1. Create a group and select it.
2. Choose **Add application** for each `.exe`, `.lnk`, `.url`, `.bat`, or `.cmd` to include. To add a packaged application, first obtain a Windows shortcut for it and select that shortcut.
   Use **Up** and **Down** to change the order. The first application's icon is used for the group shortcut; empty groups use a folder icon.
3. Choose **Create shortcut for pinning**. Explorer selects the new shortcut. Right-click it and choose **Pin to taskbar**; on Windows 11 this may be under **Show more options**. Repeat for other groups. Windows controls the pinning operation.
4. Hover a pinned group to open its icon flyout. Drag an icon and release it where the insertion marker appears to reorder it. Drag an icon onto the taskbar to pin it there; the app stays in the group if Windows does not confirm the pin. Hover over an app icon to see its name, click it to launch, or right-click it to open the Windows shell context menu for the selected file or shortcut. This is the file's shell menu, which may differ from an app's taskbar jump list.
5. The watcher starts with the manager and continues after the manager closes. To start it after signing in, use **Enable hover after sign-in** in the manager. **Disable startup** removes that startup shortcut. Clicking a pinned group also opens its flyout without the watcher.

Group data is stored in `%LOCALAPPDATA%\TaskbarGroups\groups.json`; generated shortcuts are in `%APPDATA%\Taskbar Groups`. Deleting a group does not automatically unpin its shortcut; unpin it using Windows. Removing an app from a group never deletes the original application or shortcut.

Windows can keep showing a cached icon for an existing taskbar pin after the first app changes. If that happens, unpin the group, choose **Create shortcut for pinning**, and pin the updated shortcut again. The app preserves the group's shortcut identity and updates its existing pinned shortcut where possible, but Windows controls when its taskbar image refreshes.

## Current limitations

Hover detection relies on Windows 11's taskbar accessibility tree and each group's App User Model ID. Taskbar updates, display scaling, and multiple monitors can affect detection or flyout placement. Third-party shell extensions may behave differently. The file picker accepts filesystem applications and shortcuts; it does not enumerate installed Store apps. Shortcuts do not currently expose configurable icons or app arguments. Existing running application windows retain their normal taskbar buttons.

## Development

```powershell
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
