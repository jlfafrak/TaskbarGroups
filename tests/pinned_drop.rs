use taskbar_groups::Store;

#[test]
fn taskbar_drag_opens_a_group_only_after_dwelling_over_its_original_position() {
    use taskbar_groups::{DragTarget, PinDrag, Rect};
    let group = uuid::Uuid::new_v4();
    let target = DragTarget {
        group,
        bounds: Rect {
            left: 100,
            top: 200,
            right: 150,
            bottom: 240,
        },
    };
    let mut drag = PinDrag::new(vec![target]);
    assert_eq!(drag.hover(120, 220, 0), None);
    assert_eq!(drag.hover(120, 220, 80), None);
    assert_eq!(drag.hover(120, 220, 120), Some(group));
    assert_eq!(drag.hover(120, 220, 900), None);
    assert_eq!(drag.release(Some(group)), Some(group));
    assert_eq!(drag.release(Some(group)), None);

    let mut cancelled = PinDrag::new(vec![target]);
    assert_eq!(cancelled.hover(120, 220, 0), None);
    assert_eq!(cancelled.hover(170, 220, 600), None);
    assert_eq!(cancelled.release(Some(group)), None);
}

#[test]
fn drag_can_reopen_a_closed_flyout_or_choose_another_group() {
    use taskbar_groups::{DragTarget, PinDrag, Rect};
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let targets = vec![
        DragTarget {
            group: a,
            bounds: Rect {
                left: 0,
                top: 0,
                right: 40,
                bottom: 40,
            },
        },
        DragTarget {
            group: b,
            bounds: Rect {
                left: 40,
                top: 0,
                right: 80,
                bottom: 40,
            },
        },
    ];
    let mut drag = PinDrag::new(targets);
    assert_eq!(drag.hover(20, 20, 0), None);
    assert_eq!(drag.hover(20, 20, 550), Some(a));
    drag.popup_closed();
    assert_eq!(drag.hover(20, 20, 600), None);
    assert_eq!(drag.hover(20, 20, 1150), Some(a));
    assert_eq!(drag.hover(60, 20, 1200), None);
    assert_eq!(drag.hover(60, 20, 1750), Some(b));
    assert_eq!(drag.release(Some(a)), None);
}

#[test]
fn moving_groups_cannot_cause_a_drop_to_open_the_wrong_group() {
    use taskbar_groups::{DragTarget, PinDrag, Rect};
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let first = Rect {
        left: 0,
        top: 0,
        right: 40,
        bottom: 40,
    };
    let second = Rect {
        left: 40,
        top: 0,
        right: 80,
        bottom: 40,
    };
    let mut drag = PinDrag::new(vec![
        DragTarget {
            group: a,
            bounds: first,
        },
        DragTarget {
            group: b,
            bounds: second,
        },
    ]);
    drag.set_visible_targets(vec![DragTarget {
        group: b,
        bounds: first,
    }]);
    assert_eq!(drag.hover(20, 20, 0), None);
    assert_eq!(drag.hover(20, 20, 1000), None);
    assert_eq!(drag.release(Some(a)), None);

    let mut drag = PinDrag::new(vec![
        DragTarget {
            group: a,
            bounds: first,
        },
        DragTarget {
            group: b,
            bounds: second,
        },
    ]);
    drag.hover(20, 20, 0);
    assert_eq!(drag.hover(20, 20, 550), Some(a));
    drag.set_visible_targets(vec![DragTarget {
        group: b,
        bounds: first,
    }]);
    assert_eq!(drag.hover(20, 20, 600), None);
    assert_eq!(drag.release(Some(a)), None);
}

#[test]
fn dragging_can_follow_a_group_after_windows_moves_its_button() {
    use taskbar_groups::{DragTarget, PinDrag, Rect};
    let group = uuid::Uuid::new_v4();
    let mut drag = PinDrag::new(vec![DragTarget {
        group,
        bounds: Rect {
            left: 100,
            top: 200,
            right: 150,
            bottom: 240,
        },
    }]);
    drag.set_visible_targets(vec![DragTarget {
        group,
        bounds: Rect {
            left: 50,
            top: 200,
            right: 100,
            bottom: 240,
        },
    }]);
    assert_eq!(drag.hover(75, 220, 0), None);
    assert_eq!(drag.hover(75, 220, 550), Some(group));
    assert_eq!(drag.release(Some(group)), Some(group));
}

#[test]
fn importing_a_pin_preserves_its_shortcut_and_latest_group_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let source = dir.path().join("Editor.lnk");
    std::fs::write(&source, b"shortcut with arguments").unwrap();
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store
        .add_app(group, "Existing", "C:\\existing.exe")
        .unwrap();
    store.save(&path).unwrap();

    Store::import_pinned_shortcut(&path, group, "Editor", &source).unwrap();
    let saved = Store::load(&path).unwrap();
    assert_eq!(saved.groups[0].apps.len(), 2);
    let imported = &saved.groups[0].apps[1];
    assert_eq!(imported.name, "Editor");
    assert_ne!(std::path::Path::new(&imported.path), source);
    assert_eq!(
        std::fs::read(&imported.path).unwrap(),
        b"shortcut with arguments"
    );
    std::fs::remove_file(source).unwrap();
    assert!(std::path::Path::new(&imported.path).exists());
}

#[test]
fn importing_into_a_removed_group_does_not_create_a_shortcut() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let source = dir.path().join("Editor.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    Store::default().save(&path).unwrap();
    assert!(Store::import_pinned_shortcut(&path, uuid::Uuid::new_v4(), "Editor", &source).is_err());
    assert!(!dir.path().join("shortcuts").exists());
}

#[test]
fn taskbar_pin_is_not_removed_unless_the_group_copy_was_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let source = dir.path().join("Editor.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    Store::default().save(&path).unwrap();
    let called = std::cell::Cell::new(false);
    let result =
        Store::move_pinned_shortcut(&path, uuid::Uuid::new_v4(), "Editor", &source, |_| {
            called.set(true);
            Ok(())
        });
    assert!(result.is_err());
    assert!(!called.get());
}

#[test]
fn failed_unpin_preserves_the_saved_group_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let source = dir.path().join("Editor.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.save(&path).unwrap();
    let (saved, unpin) = Store::move_pinned_shortcut(&path, group, "Editor", &source, |_| {
        Err("Windows refused".into())
    })
    .unwrap();
    assert_eq!(unpin.unwrap_err(), "Windows refused");
    assert_eq!(saved.groups[0].apps.len(), 1);
    assert_eq!(Store::load(&path).unwrap().groups[0].apps.len(), 1);
}

#[test]
fn removing_a_confirmed_pin_preserves_the_latest_group_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.add_app(group, "Editor", "C:\\editor.exe").unwrap();
    let app = store.groups[0].apps[0].clone();
    store.add_app(group, "Browser", "C:\\browser.exe").unwrap();
    store.save(&path).unwrap();
    let (updated, first_changed) = Store::remove_saved_if_matches(&path, group, &app).unwrap();
    assert!(first_changed);
    assert_eq!(updated.groups[0].apps[0].name, "Browser");
    assert_eq!(Store::load(&path).unwrap(), updated);
}

#[test]
fn stale_drag_cannot_remove_an_edited_or_missing_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.add_app(group, "Editor", "C:\\editor.exe").unwrap();
    let stale = store.groups[0].apps[0].clone();
    store.groups[0].apps[0].path = "C:\\other.exe".into();
    store.save(&path).unwrap();
    assert!(Store::remove_saved_if_matches(&path, group, &stale).is_err());
    assert_eq!(Store::load(&path).unwrap(), store);
}

#[test]
fn importing_a_taskbar_pin_uses_the_visible_insertion_position() {
    use taskbar_groups::DropSide;
    for (side, target_index, expected, first_changed) in [
        (DropSide::Before, 0, vec!["New", "A", "B"], true),
        (DropSide::Before, 1, vec!["A", "New", "B"], false),
        (DropSide::After, 1, vec!["A", "B", "New"], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groups.json");
        let source = dir.path().join("New.lnk");
        std::fs::write(&source, b"shortcut").unwrap();
        let mut store = Store::default();
        let group = store.add_group("Work").unwrap();
        store.add_app(group, "A", "a.exe").unwrap();
        store.add_app(group, "B", "b.exe").unwrap();
        let target = store.groups[0].apps[target_index].id;
        store.save(&path).unwrap();
        let (saved, unpin, changed) = Store::move_pinned_shortcut_at(
            &path,
            group,
            "New",
            &source,
            Some((target, side)),
            |_| Ok(()),
        )
        .unwrap();
        assert!(unpin.is_ok());
        assert_eq!(changed, first_changed);
        assert_eq!(
            saved.groups[0]
                .apps
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(Store::load(&path).unwrap(), saved);
    }
}

#[test]
fn a_stale_insertion_target_does_not_import_or_unpin_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let source = dir.path().join("New.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.save(&path).unwrap();
    let called = std::cell::Cell::new(false);
    let result = Store::move_pinned_shortcut_at(
        &path,
        group,
        "New",
        &source,
        Some((uuid::Uuid::new_v4(), taskbar_groups::DropSide::Before)),
        |_| {
            called.set(true);
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!called.get());
    assert_eq!(Store::load(&path).unwrap(), store);
}
