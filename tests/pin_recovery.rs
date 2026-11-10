use taskbar_groups::pin_recovery::PendingPin;

#[test]
fn staged_pin_survives_the_loss_of_its_original_shortcut() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    std::fs::write(&source, b"shortcut with arguments").unwrap();
    let root = temp.path().join("pending");
    let staged = PendingPin::stage(&root, &source, "App", "app.identity", false).unwrap();
    std::fs::remove_file(&source).unwrap();

    let loaded = PendingPin::load_all(&root).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "App");
    assert_eq!(loaded[0].identity, "app.identity");
    assert!(!loaded[0].packaged);
    assert_eq!(loaded[0].original, source);
    assert_eq!(loaded[0].shortcut(), staged.shortcut());
    assert_eq!(
        std::fs::read(loaded[0].shortcut()).unwrap(),
        b"shortcut with arguments"
    );
}

#[test]
fn staging_a_missing_shortcut_does_not_create_a_recovery_record() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pending");
    assert!(
        PendingPin::stage(&root, &temp.path().join("missing.lnk"), "App", "id", false).is_err()
    );
    assert!(PendingPin::load_all(&root).unwrap().is_empty());
}

#[test]
fn clearing_one_completed_transfer_keeps_the_other_recovery_records() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    let root = temp.path().join("pending");
    let first = PendingPin::stage(&root, &source, "First", "first.id", false).unwrap();
    let second = PendingPin::stage(&root, &source, "Second", "second.id", true).unwrap();
    first.clear().unwrap();
    let remaining = PendingPin::load_all(&root).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].identity, second.identity);
    assert!(remaining[0].packaged);
    assert!(remaining[0].shortcut().is_file());
}

#[test]
fn a_read_only_source_shortcut_can_be_staged() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    std::fs::write(&source, b"readonly shortcut").unwrap();
    let original_permissions = std::fs::metadata(&source).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&source, permissions.clone()).unwrap();
    let result = PendingPin::stage(&temp.path().join("pending"), &source, "App", "id", false);
    std::fs::set_permissions(&source, original_permissions).unwrap();
    let pending = result.unwrap();
    assert_eq!(
        std::fs::read(pending.shortcut()).unwrap(),
        b"readonly shortcut"
    );
}

#[test]
fn damaged_metadata_is_reported_without_deleting_the_recovery_copy() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    std::fs::write(&source, b"shortcut").unwrap();
    let root = temp.path().join("pending");
    let pending = PendingPin::stage(&root, &source, "App", "id", false).unwrap();
    let metadata = pending.shortcut().parent().unwrap().join("pending.json");
    std::fs::write(metadata, b"{broken").unwrap();
    assert!(PendingPin::load_all(&root).is_err());
    assert_eq!(std::fs::read(pending.shortcut()).unwrap(), b"shortcut");
}

#[test]
fn a_pin_is_backed_up_before_the_original_is_detached() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    let root = temp.path().join("pending");
    std::fs::write(&source, b"original shortcut").unwrap();
    let (pending, outcome) =
        PendingPin::stage_and_detach(&root, &source, "App", "app.identity", false, |original| {
            let records = PendingPin::load_all(&root).unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(
                std::fs::read(records[0].shortcut()).unwrap(),
                b"original shortcut"
            );
            std::fs::remove_file(original).map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(outcome.is_ok());
    assert!(!source.exists());
    assert!(pending.shortcut().is_file());
}

#[test]
fn staging_failure_never_invokes_unpin() {
    let temp = tempfile::tempdir().unwrap();
    let called = std::cell::Cell::new(false);
    let result = PendingPin::stage_and_detach(
        &temp.path().join("pending"),
        &temp.path().join("missing.lnk"),
        "App",
        "id",
        false,
        |_| {
            called.set(true);
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!called.get());
}

#[test]
fn unverified_restoration_preserves_the_recovery_record() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    let root = temp.path().join("pending");
    std::fs::write(&source, b"shortcut").unwrap();
    let pending = PendingPin::stage(&root, &source, "App", "id", false).unwrap();
    let result = pending.restore_and_clear(|record| {
        assert_eq!(std::fs::read(record.shortcut()).unwrap(), b"shortcut");
        Ok(false)
    });
    assert!(result.is_err());
    assert_eq!(PendingPin::load_all(&root).unwrap().len(), 1);
}

#[test]
fn confirmed_restoration_clears_the_recovery_record() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    let root = temp.path().join("pending");
    std::fs::write(&source, b"shortcut").unwrap();
    let pending = PendingPin::stage(&root, &source, "App", "id", false).unwrap();
    pending.restore_and_clear(|_| Ok(true)).unwrap();
    assert!(PendingPin::load_all(&root).unwrap().is_empty());
}

#[test]
fn unpin_failure_keeps_the_recovery_copy_even_if_the_original_disappears() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    let root = temp.path().join("pending");
    std::fs::write(&source, b"shortcut").unwrap();
    let (pending, outcome) =
        PendingPin::stage_and_detach(&root, &source, "App", "id", false, |original| {
            std::fs::remove_file(original).unwrap();
            Err("Windows did not confirm removal".into())
        })
        .unwrap();
    assert!(outcome.is_err());
    assert_eq!(std::fs::read(pending.shortcut()).unwrap(), b"shortcut");
    assert_eq!(PendingPin::load_all(&root).unwrap().len(), 1);
}

#[test]
fn restoration_error_does_not_delete_the_recovery_record() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("App.lnk");
    let root = temp.path().join("pending");
    std::fs::write(&source, b"shortcut").unwrap();
    let pending = PendingPin::stage(&root, &source, "App", "id", false).unwrap();
    assert!(
        pending
            .restore_and_clear(|_| Err("Windows error".into()))
            .is_err()
    );
    assert_eq!(PendingPin::load_all(&root).unwrap().len(), 1);
}
