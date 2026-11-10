use taskbar_groups::{Store, group_for_taskbar_name, shortcut_title};

#[test]
fn taskbar_names_match_only_the_unique_group_shortcut() {
    let mut store = Store::default();
    let work = store.add_group("Work").unwrap();
    let other = store.add_group("Games").unwrap();
    let title = shortcut_title(&store.groups[0]);
    assert_eq!(group_for_taskbar_name(&store.groups, &title), Some(work));
    assert_eq!(group_for_taskbar_name(&store.groups, "Work"), None);
    assert_ne!(group_for_taskbar_name(&store.groups, &title), Some(other));
}

#[test]
fn shortcut_title_is_a_valid_windows_filename_and_matches_hover_name() {
    let mut store = Store::default();
    let id = store.add_group("Dev: tools").unwrap();
    let title = shortcut_title(&store.groups[0]);
    assert!(!title.contains(':'));
    assert_eq!(group_for_taskbar_name(&store.groups, &title), Some(id));
}

#[test]
fn groups_have_distinct_stable_windows_app_ids() {
    let mut store = Store::default();
    store.add_group("Work").unwrap();
    store.add_group("Games").unwrap();
    let a = taskbar_groups::app_id(&store.groups[0].id);
    let b = taskbar_groups::app_id(&store.groups[1].id);
    assert_ne!(a, b);
    assert!(a.starts_with("TaskbarGroups.Group."));
}

#[test]
fn flyout_position_stays_in_the_selected_monitor_work_area() {
    let work = taskbar_groups::Rect {
        left: -1920,
        top: 0,
        right: 0,
        bottom: 1040,
    };
    let taskbar = taskbar_groups::Rect {
        left: -1920,
        top: 1040,
        right: 0,
        bottom: 1080,
    };
    assert_eq!(
        taskbar_groups::popup_position(work, taskbar, -50, 1050, 100.0, 1.0),
        (-320.0, 940.0)
    );
    let top_bar = taskbar_groups::Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 40,
    };
    let work = taskbar_groups::Rect {
        left: 0,
        top: 40,
        right: 1920,
        bottom: 1080,
    };
    assert_eq!(
        taskbar_groups::popup_position(work, top_bar, 100, 10, 200.0, 1.0),
        (100.0, 40.0)
    );
    let side = taskbar_groups::Rect {
        left: 1920,
        top: 0,
        right: 1980,
        bottom: 1080,
    };
    let work = taskbar_groups::Rect {
        left: 1980,
        top: 0,
        right: 3900,
        bottom: 1080,
    };
    assert_eq!(
        taskbar_groups::popup_position(work, side, 1950, 600, 200.0, 1.5),
        (1980.0, 600.0)
    );
}

#[test]
fn hovering_the_taskbar_anchor_keeps_the_flyout_open() {
    assert!(!taskbar_groups::should_dismiss_hover(true, false, 5000));
    assert!(!taskbar_groups::should_dismiss_hover(false, true, 5000));
    assert!(!taskbar_groups::should_dismiss_hover(false, false, 700));
    assert!(taskbar_groups::should_dismiss_hover(false, false, 1600));
}

#[test]
fn matches_windows_taskbar_button_by_app_id_even_when_display_name_changes() {
    let mut store = Store::default();
    let id = store.add_group("Work").unwrap();
    store.add_group("Games").unwrap();
    let automation_id = format!("Appid: {}", taskbar_groups::app_id(&id));
    assert_eq!(
        taskbar_groups::group_for_automation_id(&store.groups, &automation_id),
        Some(id)
    );
    assert_eq!(
        taskbar_groups::group_for_automation_id(&store.groups, "Appid: Other.Application"),
        None
    );
    assert_eq!(
        taskbar_groups::group_for_automation_id(&store.groups, ""),
        None
    );
}

#[test]
fn flyout_icon_grid_is_compact_and_wraps_large_groups() {
    assert_eq!(taskbar_groups::flyout_grid(4), (4, 1));
    assert_eq!(taskbar_groups::flyout_grid(10), (4, 3));
    assert_eq!(taskbar_groups::flyout_grid(0), (1, 1));
}

#[test]
fn flyout_grid_obeys_the_configured_icons_per_row() {
    use taskbar_groups::{flyout_grid_with_columns, flyout_size_with_columns};
    assert_eq!(flyout_grid_with_columns(10, 3), (3, 4));
    assert_eq!(flyout_grid_with_columns(10, 7), (7, 2));
    assert_eq!(flyout_grid_with_columns(3, 12), (3, 1));
    assert_eq!(flyout_grid_with_columns(0, 1), (1, 1));
    assert_eq!(flyout_grid_with_columns(20, 0), (1, 20));
    assert_eq!(flyout_grid_with_columns(20, 100), (12, 2));
    assert_eq!(flyout_size_with_columns(9, 4), (272.0, 232.0));
}

#[test]
fn flyout_reserves_a_footer_for_the_settings_control() {
    assert_eq!(taskbar_groups::flyout_size(1), (120.0, 112.0));
    assert_eq!(taskbar_groups::flyout_size(9), (272.0, 232.0));
}

#[test]
fn compact_flyout_is_centered_on_the_taskbar_button_and_clamped() {
    use taskbar_groups::{Rect, position_flyout};
    let work = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    let bar = Rect {
        left: 0,
        top: 1040,
        right: 1920,
        bottom: 1080,
    };
    let anchor = Rect {
        left: 900,
        top: 1040,
        right: 944,
        bottom: 1080,
    };
    assert_eq!(
        position_flyout(work, bar, anchor, 272.0, 72.0, 1.0),
        (786, 968)
    );
    let edge = Rect {
        left: 0,
        top: 1040,
        right: 44,
        bottom: 1080,
    };
    assert_eq!(position_flyout(work, bar, edge, 272.0, 72.0, 1.0), (0, 968));
}

#[test]
fn taller_flyout_stays_above_the_taskbar() {
    use taskbar_groups::{Rect, flyout_size, position_flyout};
    let work = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    let bar = Rect {
        left: 0,
        top: 1040,
        right: 1920,
        bottom: 1080,
    };
    let anchor = Rect {
        left: 900,
        top: 1040,
        right: 944,
        bottom: 1080,
    };
    let (width, height) = flyout_size(9);
    let (_, y) = position_flyout(work, bar, anchor, width, height, 1.0);
    assert_eq!(y + height as i32, work.bottom);
}
