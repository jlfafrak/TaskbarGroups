# Running Windows in Groups Implementation Plan

**Goal:** Keep grouped running applications accessible through the flyout without separate running taskbar buttons by default, with an opt-out per application.

**Architecture:** Identify windows conservatively from application and shortcut identity. The watcher removes matching taskbar buttons only when a separate recovery process can restore them; a durable journal covers interrupted cleanup. The flyout lists matching windows and activates a selected window, while still offering a way to launch a new one.

**Tech stack:** Rust, egui, Windows Shell and window APIs.

## Tasks

1. Add a backwards-compatible per-app preference and manager control; test old data, defaults, and persistence.
2. Resolve shortcut and packaged application identities and enumerate matching windows without matching unrelated applications; test uncertain and reused identities.
3. Add a durable recovery journal and independent recovery process; test interruptions, stale handles, restoration, and failures before hiding windows.
4. Reconcile managed windows as groups and preferences change; restore windows when monitoring stops or matching becomes uncertain.
5. Show running status and a window chooser in the flyout, with an option to launch a new window; test the UI and actions.
6. Run the Rust and installer checks and test with disposable windows, including multi-window and recovery scenarios. Preserve the existing uncommitted installer work.
