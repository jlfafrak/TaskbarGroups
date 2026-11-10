# Taskbar Drag Handoff Implementation Plan

> Use superpowers:executing-plans and test each stage before enabling the handoff.

**Goal:** Remove a taskbar pin when its drag enters a group flyout, add it to the group on drop, and restore it to the taskbar on cancellation or a drop back onto the taskbar.

**Architecture:** Save a durable, named copy and identity of the source before unpinning. Hand the still-held gesture to a Windows drag source where possible, and retain the recovery record until either the group has been saved or Windows has confirmed the pin was restored. Never enable early unpinning until automatic restoration is demonstrated against a disposable shell target and Explorer.

**Tech Stack:** Rust, Windows COM/OLE drag and drop, existing taskbar watcher and group store.

**Review focus:** packaged apps, shortcuts with arguments, failure or crash between unpin and save, Escape while held, release outside any target, pinning failure, multiple taskbars, and preservation of the original pin position.

## Tasks

1. Characterize `DoDragDrop` without a physical button and Windows taskbar acceptance using disposable objects. Confirm whether the taskbar accepts automatic restoration, and whether the requested drop location is preserved.
2. Test and implement a persistent recovery record which retains the source shortcut and identity before any unpin, survives restart, and is removed only after a verified outcome.
3. Test and implement the handoff state transitions for flyout entry, unpin failure, flyout drop, taskbar drop, cancellation, and recovery failure. Preserve current behavior when the handoff cannot start safely.
4. Integrate with the watcher and existing drag source. Test cancellation, window closure, stale snapshots, concurrent group edits, and shortcuts for packaged and unpackaged apps.
5. Run formatting, unit/integration tests, clippy and a release build. Verify both transfer directions and cancellation on Windows with a disposable pin before deploying.

## Investigation status

- A disposable in-process OLE target accepted a drop without a held mouse button after the source sent a private wake-up message following accepted feedback.
- The same approach, run at Explorer's integrity level with the pointer over a taskbar button and a disposable executable as the data source, returned no accepted drop effect and created no matching taskbar pin. This result characterizes that approach, not every possible way to restore a pin.
- A real pointer movement on the taskbar also failed to produce an accepted drop effect without a held button. A Windows Shell pin-list interface returned success for an unpinned shortcut but did not create a pin on Windows 11 25H2. An extended Shell command was advertised by name but was not present in the menu, and invoking it for a test executable returned `0x80070483`.
- Computer-use automation could read Explorer's window but could not activate it, even after the user confirmed the desktop was unlocked and the session was reset. Avoid assuming that a graphical Explorer operation has been tested.
- `src/pin_recovery.rs` stages a recovery copy and metadata, with filesystem tests, but is not connected to the watcher or enabled in the running build. No early unpin should be enabled until restoration and crash recovery are verified on Windows.
