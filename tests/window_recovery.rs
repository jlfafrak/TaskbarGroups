use taskbar_groups::window_recovery::{IdentityChange, RecoveryJournal, WindowKey};

fn window(hwnd: isize, pid: u32, started: u64) -> WindowKey {
    WindowKey {
        hwnd,
        pid,
        process_started: started,
        marker: format!("test-{hwnd}-{started}"),
        identity: None,
    }
}

#[test]
fn identity_change_is_recorded_before_grouping_and_survives_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    let mut entry = window(42, 30, 123);
    entry.identity = Some(IdentityChange {
        app: uuid::Uuid::from_u128(1),
        group: uuid::Uuid::from_u128(2),
        original: Some("Original.Application".into()),
        assigned: "TaskbarGroups.Group".into(),
    });
    let mut journal = RecoveryJournal::load(&path).unwrap();
    journal
        .hide(&entry, || {
            assert_eq!(
                RecoveryJournal::load(&path).unwrap().entries(),
                std::slice::from_ref(&entry)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn old_journal_entries_remain_readable_for_legacy_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    std::fs::write(
        &path,
        r#"{"windows":[{"hwnd":42,"pid":30,"process_started":123,"marker":"test"}]}"#,
    )
    .unwrap();
    let journal = RecoveryJournal::load(&path).unwrap();
    assert_eq!(journal.entries()[0].identity, None);
}

#[test]
fn hidden_window_is_journaled_before_its_taskbar_button_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    let entry = window(42, 30, 123);
    let mut journal = RecoveryJournal::load(&path).unwrap();
    journal
        .hide(&entry, || {
            assert_eq!(
                RecoveryJournal::load(&path).unwrap().entries(),
                std::slice::from_ref(&entry)
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(RecoveryJournal::load(&path).unwrap().entries(), &[entry]);
}

#[test]
fn failed_hiding_keeps_a_recovery_record_until_restoration_is_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    let entry = window(42, 30, 123);
    let mut journal = RecoveryJournal::load(&path).unwrap();
    assert!(
        journal
            .hide(&entry, || Err(std::io::Error::other("shell failure")))
            .is_err()
    );
    assert_eq!(RecoveryJournal::load(&path).unwrap().entries(), &[entry]);
    assert!(
        journal
            .restore_all(|_| Err(std::io::Error::other("shell unavailable")))
            .is_err()
    );
    assert_eq!(RecoveryJournal::load(&path).unwrap().entries().len(), 1);
    journal.restore_all(|_| Ok(())).unwrap();
    assert!(RecoveryJournal::load(&path).unwrap().entries().is_empty());
}

#[test]
fn recovery_keeps_failed_entries_and_clears_successful_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    let first = window(42, 30, 123);
    let second = window(43, 31, 456);
    let mut journal = RecoveryJournal::load(&path).unwrap();
    journal.hide(&first, || Ok(())).unwrap();
    journal.hide(&second, || Ok(())).unwrap();
    assert!(
        journal
            .restore_all(|entry| if entry == &first {
                Ok(())
            } else {
                Err(std::io::Error::other("still in use"))
            })
            .is_err()
    );
    assert_eq!(RecoveryJournal::load(&path).unwrap().entries(), &[second]);
}

#[test]
fn damaged_recovery_data_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.json");
    std::fs::write(&path, b"not valid json").unwrap();
    assert!(RecoveryJournal::load(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not valid json");
}
