# Drag Group App to Taskbar Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans and test-driven development.

**Goal:** Let a user drag an app from a group to the Windows taskbar, and remove the app from the group only after Windows confirms the pin.

**Architecture:** Keep egui's existing in-group reorder until the cursor reaches the taskbar, then hand a Windows Shell data object to a native drag operation. Check the resulting taskbar item identity before removing the exact saved group item under the existing store lock. Preserve the group item on cancellation, unsupported drops, failed pinning, and ambiguous identity.

**Tech Stack:** Rust 1.99, egui, Windows Shell COM and UI Automation.

**Global constraints:** Preserve existing app behavior, shortcuts, application arguments, other groups, and the first-app group icon synchronization. Never delete the source shortcut. Do not accept a drop onto an unrelated window.

**Review focus:** Package identity vs desktop shortcut identity; already pinned and running apps; drag cancellation; source or group changed while dragging; multiple taskbars and display scaling; Windows returning an accepted drop without actually creating a pin.

### Task 1: Establish taskbar drag feasibility

- [ ] Verify the Shell data object for group items, including packaged items, exposes its exact source identity.
- [ ] Implement a taskbar-only native drop operation and verify its cancellation behavior with isolated tests.

### Task 2: Make group removal conditional

- [ ] Add failing tests for locked, identity-checked removal after confirmed pinning, including stale group data and failure paths.
- [ ] Implement and run the tests.

### Task 3: Connect the flyout

- [ ] Hand off only when dragging reaches a Windows taskbar; preserve reorder and click behavior.
- [ ] Verify the resulting pin before removing the item and synchronize the group icon if the first app changed.
- [ ] Run the complete suite, formatting, lint and release build, and validate the real taskbar interaction where possible.
