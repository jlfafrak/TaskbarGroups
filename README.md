# Taskbar Groups

A Windows 11 launcher written in Rust. Create separate groups, add application files or Windows shortcuts, and pin a launcher shortcut for each group. The background watcher uses Windows UI Automation to detect a hover over a group button and opens its flyout. You can also click a pinned group to open it.

## Install and upgrade

Run `TaskbarGroups-<version>-x64.msi` on Windows 11. It installs for the current user to `%LOCALAPPDATA%\Programs\Taskbar Groups` and adds a Start menu shortcut. Open **Taskbar Groups** from the Start menu after installation. The MSI is unsigned, so Windows may display a publisher warning.

To upgrade, run a newer MSI. It replaces the installed application at the same path, closes running installed instances, and restarts the hover watcher. Finish any drag in progress before upgrading; if the app cannot close within 30 seconds, the installer stops without replacing it. If an installation fails after closing the app, open Taskbar Groups again to restart the watcher. Groups and settings in `%LOCALAPPDATA%\TaskbarGroups` are preserved, as are the app's startup preference and pinned group shortcuts. Windows Installer prevents installing an older version over a newer one. Before uninstalling through Windows Settings, disable startup in the manager if it was enabled; your groups and settings are retained.

If you previously ran the app from a development build, your existing pins and startup shortcut still refer to that build's path. After installing, close the development manager and its `--watch` process, open the installed app, and recreate and repin each group shortcut from the manager. If startup was enabled, disable and re-enable it in the installed manager so it points to the installed executable.

## Build the MSI

Install the Rust MSVC toolchain and Microsoft C++ build tools, plus a .NET runtime 6 or newer. From the project directory run:

```powershell
.\installer\build-msi.ps1
```

The script downloads and verifies WiX 5.0.2 from NuGet on its first run, builds the release executable separately, and writes the MSI to `target\installer`. WiX 5 is out of support; the pinned version avoids requiring acceptance of the newer WiX maintenance-fee EULA. Each release must increase the three-part version in `Cargo.toml`; Windows Installer uses those three parts for upgrades (maximum 255.255.65535). Keep the installer's upgrade and component IDs unchanged between releases.

## Build and run from source

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
4. Hover a pinned group to open its icon flyout. Drag an icon and release it where the insertion marker appears to reorder it. Drag an icon onto the taskbar to pin it there; the app stays in the group if Windows does not confirm the pin. Hover over an app icon to see its name. Click to launch it or bring its existing window forward; if it has several windows, choose one from the list. A dot marks a running app. Shift-click an icon to launch another instance, or use **Open another window** in the window list. Right-click to open the Windows shell context menu for the selected file or shortcut. This is the file's shell menu, which may differ from an app's taskbar jump list.
5. The watcher starts with the manager and continues after the manager closes. To start it after signing in, use **Enable hover after sign-in** in the manager. **Disable startup** removes that startup shortcut. Clicking a pinned group also opens its flyout without the watcher.

Group data is stored in `%LOCALAPPDATA%\TaskbarGroups\groups.json`; generated shortcuts are in `%APPDATA%\Taskbar Groups`. Deleting a group does not automatically unpin its shortcut; unpin it using Windows. Removing an app from a group never deletes the original application or shortcut.

When the watcher can identify an app's running windows and its group is pinned and accessible, it hides their separate taskbar buttons by default. Clear **Hide taskbar button** for an individual app to keep its normal buttons. Removing the app, unpinning the group, turning the setting off, or stopping the watcher restores the buttons. A separate recovery process also restores them if the watcher exits unexpectedly.

Windows can keep showing a cached icon for an existing taskbar pin after the first app changes. If that happens, unpin the group, choose **Create shortcut for pinning**, and pin the updated shortcut again. The app preserves the group's shortcut identity and updates its existing pinned shortcut where possible, but Windows controls when its taskbar image refreshes.

## Current limitations

Hover detection relies on Windows 11's taskbar accessibility tree and each group's App User Model ID. Taskbar updates, display scaling, and multiple monitors can affect detection or flyout placement. Third-party shell extensions may behave differently. The file picker accepts filesystem applications and shortcuts; it does not enumerate installed Store apps. Shortcuts do not currently expose configurable icons or app arguments.

Running-window matching is conservative: shortcuts with command-line arguments, applications that launch a different executable, shared executables used by more than one grouped app, desktop windows with their own explicit app identity, some hosted Store apps, and windows that Windows does not permit the watcher to inspect may retain their normal taskbar buttons and will launch normally from the group. Window lists cover identifiable, visible top-level windows on the current desktop. Windows can refuse foreground activation, and launching another window depends on the application's own behavior. Windows does not expose a reliable public query for whether an app independently removed a particular window's taskbar button; restoring a matched window can therefore create a button that app had intentionally hidden.

## Development

```powershell
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
