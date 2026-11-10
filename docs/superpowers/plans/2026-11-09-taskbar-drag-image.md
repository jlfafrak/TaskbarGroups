# Taskbar Drag Image Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans and test-driven development.

**Goal:** Keep the dragged application icon visible from the taskbar into a group flyout until the drag ends.

**Architecture:** The existing taskbar watcher owns a temporary, no-activate, click-through, per-pixel transparent Windows overlay. Load the source application's real icon on an STA worker, show it beside the cursor once the group flyout is visible and the cursor has left the taskbar, and destroy it on drop or cancellation. Existing drag detection, drop targeting, and pin transfer remain unchanged.

**Tech Stack:** Rust 1.99, Windows Shell and layered windows, egui icon extraction.

**Constraints:** Do not intercept pointer events, take focus, add a taskbar button, or mutate pins or groups when the drag is canceled. Support secondary taskbars, scaling, slow icon loading, and missing icons without breaking drop.

### Tasks

- [ ] Test the image visibility rules for taskbar, flyout, and cancellation.
- [ ] Create and test the transparent native overlay and its cleanup without displaying it on the desktop.
- [ ] Load icons without blocking the watcher, connect the overlay to drag lifecycle, and keep pointer hit tests unchanged.
- [ ] Run formatting, unit tests, lint and release build; install through the normal desktop session and verify live behavior if possible.
