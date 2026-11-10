# Windows Installer Implementation Plan

**Goal:** Build a per-user Windows MSI that installs Taskbar Groups and supports future major upgrades.

**Architecture:** WiX authors a stable x64 MSI installation in Local AppData, with a Start menu shortcut and a fixed upgrade code. The build script derives the version from Cargo and builds a separate release binary, while user data stays outside the MSI. Windows Installer detects earlier versions and blocks downgrades.

**Tech stack:** Rust, PowerShell, WiX Toolset, Windows Installer.

## Tasks

1. Add WiX authoring and a repeatable MSI build script with validated versions and stable upgrade/component identities.
2. Ensure installed application processes can stop cleanly for an upgrade and that existing configurations remain intact.
3. Document installation, upgrades, build prerequisites, and the transition from development builds.
4. Build the MSI and verify its metadata, isolated install, upgrade, downgrade protection, uninstall, and the Rust test suite.
