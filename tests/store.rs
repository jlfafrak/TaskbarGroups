use taskbar_groups::{DropSide, Store};

#[test]
fn legacy_stores_default_to_four_icons_per_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    std::fs::write(&path, br#"{"groups":[]}"#).unwrap();
    assert_eq!(Store::load(&path).unwrap().icons_per_row, 4);
    assert_eq!(Store::default().icons_per_row, 4);
}

#[test]
fn icons_per_row_is_validated_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    store.set_icons_per_row(7).unwrap();
    store.save(&path).unwrap();
    assert_eq!(Store::load(&path).unwrap().icons_per_row, 7);
    assert!(store.set_icons_per_row(0).is_err());
    assert!(store.set_icons_per_row(13).is_err());
    assert_eq!(store.icons_per_row, 7);
}

#[test]
fn groups_and_apps_survive_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store
        .add_app(group, "Editor", r"C:\Tools\editor.exe")
        .unwrap();
    store.save(&path).unwrap();
    let loaded = Store::load(&path).unwrap();
    assert_eq!(loaded.groups[0].name, "Work");
    assert_eq!(loaded.groups[0].apps[0].path, r"C:\Tools\editor.exe");
}

#[test]
fn rejects_blank_and_duplicate_group_names() {
    let mut store = Store::default();
    assert!(store.add_group("  ").is_err());
    store.add_group("Work").unwrap();
    assert!(store.add_group(" work ").is_err());
}

#[test]
fn deleting_a_group_does_not_delete_other_groups() {
    let mut store = Store::default();
    let a = store.add_group("Work").unwrap();
    let b = store.add_group("Games").unwrap();
    store.remove_group(a);
    assert_eq!(store.groups.len(), 1);
    assert_eq!(store.groups[0].id, b);
}

#[test]
fn saving_again_replaces_the_previous_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    store.add_group("First").unwrap();
    store.save(&path).unwrap();
    store.add_group("Second").unwrap();
    store.save(&path).unwrap();
    assert_eq!(Store::load(&path).unwrap().groups.len(), 2);
}

#[test]
fn moving_apps_updates_order_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.add_app(group, "One", r"C:\one.exe").unwrap();
    store.add_app(group, "Two", r"C:\two.exe").unwrap();
    store.add_app(group, "Three", r"C:\three.exe").unwrap();
    let second = store.groups[0].apps[1].id;
    assert!(store.move_app(group, second, -1).unwrap());
    assert_eq!(
        store.groups[0]
            .apps
            .iter()
            .map(|app| app.name.as_str())
            .collect::<Vec<_>>(),
        ["Two", "One", "Three"]
    );
    store.save(&path).unwrap();
    assert_eq!(Store::load(&path).unwrap().groups[0].apps[0].name, "Two");
}

#[test]
fn moving_an_app_past_an_edge_does_not_change_the_group() {
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.add_app(group, "First", r"C:\first.exe").unwrap();
    let app = store.groups[0].apps[0].id;
    assert!(!store.move_app(group, app, -1).unwrap());
    assert!(!store.move_app(group, app, 1).unwrap());
    assert_eq!(store.groups[0].apps[0].id, app);
    assert!(store.move_app(group, uuid::Uuid::new_v4(), 1).is_err());
}

#[test]
fn dragging_an_icon_reorders_the_latest_saved_group() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    store.add_app(group, "One", r"C:\one.exe").unwrap();
    store.add_app(group, "Two", r"C:\two.exe").unwrap();
    store.add_app(group, "Three", r"C:\three.exe").unwrap();
    let dragged = store.groups[0].apps[2].id;
    let target = store.groups[0].apps[0].id;
    store.save(&path).unwrap();
    let (updated, first_changed) =
        Store::insert_saved(&path, group, dragged, target, DropSide::Before).unwrap();
    assert!(first_changed);
    assert_eq!(
        updated.groups[0]
            .apps
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["Three", "One", "Two"]
    );
    assert_eq!(Store::load(&path).unwrap(), updated);
}

#[test]
fn stale_manager_save_cannot_erase_a_flyout_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut manager = Store::default();
    let group = manager.add_group("Work").unwrap();
    manager.add_app(group, "One", r"C:\one.exe").unwrap();
    manager.add_app(group, "Two", r"C:\two.exe").unwrap();
    manager.save(&path).unwrap();
    let baseline = manager.clone();
    let first = manager.groups[0].apps[0].id;
    let second = manager.groups[0].apps[1].id;
    Store::insert_saved(&path, group, second, first, DropSide::Before).unwrap();
    manager.add_group("Other").unwrap();
    assert_eq!(
        manager
            .save_if_unchanged(&path, &baseline)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let saved = Store::load(&path).unwrap();
    assert_eq!(saved.groups.len(), 1);
    assert_eq!(saved.groups[0].apps[0].id, second);
}

#[test]
fn simultaneous_reorders_in_different_groups_are_both_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let a = store.add_group("A").unwrap();
    let b = store.add_group("B").unwrap();
    for group in [a, b] {
        store.add_app(group, "First", r"C:\first.exe").unwrap();
        store.add_app(group, "Second", r"C:\second.exe").unwrap();
    }
    store.save(&path).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = store
        .groups
        .iter()
        .map(|group| {
            let path = path.clone();
            let barrier = barrier.clone();
            let (id, first, second) = (group.id, group.apps[0].id, group.apps[1].id);
            std::thread::spawn(move || {
                barrier.wait();
                Store::insert_saved(&path, id, second, first, DropSide::Before).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let saved = Store::load(&path).unwrap();
    assert!(
        saved
            .groups
            .iter()
            .all(|group| group.apps[0].name == "Second")
    );
}

#[test]
fn dragging_an_app_removed_by_another_window_does_not_overwrite_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let id = store.add_group("Work").unwrap();
    store.add_app(id, "One", r"C:\one.exe").unwrap();
    store.add_app(id, "Two", r"C:\two.exe").unwrap();
    let removed = store.groups[0].apps.remove(0).id;
    let remaining = store.groups[0].apps[0].id;
    store.save(&path).unwrap();
    assert_eq!(
        Store::insert_saved(&path, id, removed, remaining, DropSide::Before)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(Store::load(&path).unwrap(), store);
}

#[test]
fn drop_before_or_after_places_app_at_the_marked_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    for name in ["A", "B", "C", "D"] {
        store
            .add_app(group, name, &format!(r"C:\{name}.exe"))
            .unwrap();
    }
    let ids: Vec<_> = store.groups[0].apps.iter().map(|app| app.id).collect();
    store.save(&path).unwrap();
    let (updated, changed) =
        Store::insert_saved(&path, group, ids[0], ids[2], DropSide::Before).unwrap();
    assert!(changed);
    assert_eq!(
        updated.groups[0]
            .apps
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["B", "A", "C", "D"]
    );
    let (updated, changed) =
        Store::insert_saved(&path, group, ids[0], ids[3], DropSide::After).unwrap();
    assert!(!changed);
    assert_eq!(
        updated.groups[0]
            .apps
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["B", "C", "D", "A"]
    );
    let (updated, changed) =
        Store::insert_saved(&path, group, ids[0], ids[1], DropSide::Before).unwrap();
    assert!(changed);
    assert_eq!(
        updated.groups[0]
            .apps
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "C", "D"]
    );
    assert_eq!(Store::load(&path).unwrap(), updated);
}

#[test]
fn dropping_at_current_boundary_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.json");
    let mut store = Store::default();
    let group = store.add_group("Work").unwrap();
    for name in ["A", "B", "C"] {
        store
            .add_app(group, name, &format!(r"C:\{name}.exe"))
            .unwrap();
    }
    let ids: Vec<_> = store.groups[0].apps.iter().map(|app| app.id).collect();
    store.save(&path).unwrap();
    for (target, side) in [
        (ids[0], DropSide::After),
        (ids[2], DropSide::Before),
        (ids[1], DropSide::Before),
    ] {
        let (saved, changed) = Store::insert_saved(&path, group, ids[1], target, side).unwrap();
        assert!(!changed);
        assert_eq!(saved, store);
    }
}
