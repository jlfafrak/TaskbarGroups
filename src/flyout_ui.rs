use eframe::egui::{self, Response, TextureId, Ui, Vec2};
use taskbar_groups::DropSide;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InsertionMarker {
    pub target: Uuid,
    pub side: DropSide,
    pub x: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DropLayout {
    pub group: Uuid,
    pixels_per_point: f32,
    items: Vec<(Uuid, [f32; 4])>,
    clip: [f32; 4],
}

impl DropLayout {
    pub fn new(
        group: Uuid,
        pixels_per_point: f32,
        items: &[(Uuid, egui::Rect)],
        clip: egui::Rect,
    ) -> Self {
        let rect = |r: egui::Rect| [r.left(), r.top(), r.right(), r.bottom()];
        Self {
            group,
            pixels_per_point,
            items: items.iter().map(|(id, r)| (*id, rect(*r))).collect(),
            clip: rect(clip),
        }
    }

    pub fn marker_at_client(&self, x: i32, y: i32) -> Option<InsertionMarker> {
        if !self.pixels_per_point.is_finite() || self.pixels_per_point <= 0.0 {
            return None;
        }
        let rect =
            |r: [f32; 4]| egui::Rect::from_min_max(egui::pos2(r[0], r[1]), egui::pos2(r[2], r[3]));
        let items: Vec<_> = self.items.iter().map(|(id, r)| (*id, rect(*r))).collect();
        insertion_marker(
            &items,
            rect(self.clip),
            egui::pos2(
                x as f32 / self.pixels_per_point,
                y as f32 / self.pixels_per_point,
            ),
        )
    }

    pub fn write_atomic(&self, path: &std::path::Path) -> std::io::Result<()> {
        let temporary = path.with_extension("tmp");
        let data = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        std::fs::write(&temporary, data)?;
        std::fs::rename(temporary, path)
    }

    pub fn read(path: &std::path::Path) -> std::io::Result<Self> {
        let layout: Self =
            serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
        let rect_is_valid = |rect: [f32; 4]| {
            rect.iter().all(|value| value.is_finite()) && rect[0] <= rect[2] && rect[1] <= rect[3]
        };
        if !layout.pixels_per_point.is_finite()
            || layout.pixels_per_point <= 0.0
            || !rect_is_valid(layout.clip)
            || layout.items.iter().any(|(_, rect)| !rect_is_valid(*rect))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid flyout layout",
            ));
        }
        Ok(layout)
    }
}

pub fn icon_rows<T>(
    ui: &mut Ui,
    items: &[T],
    icons_per_row: usize,
    mut show: impl FnMut(&mut Ui, &T),
) {
    let columns = taskbar_groups::flyout_grid_with_columns(items.len(), icons_per_row).0;
    for row in items.chunks(columns) {
        ui.horizontal(|ui| {
            for item in row {
                show(ui, item);
            }
        });
    }
}

pub fn drop_hit_area(clip: egui::Rect) -> egui::Rect {
    clip.expand2(egui::vec2(8.0, 0.0))
}

pub fn settings_button(ui: &mut Ui) -> Response {
    let size = Vec2::splat(24.0);
    let rect = egui::Rect::from_min_size(ui.max_rect().right_bottom() - size, size);
    let response = ui.interact(
        rect,
        ui.scope_id().with("flyout-settings"),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), "Open settings")
    });
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 5.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let center = rect.center();
    let stroke = egui::Stroke::new(1.5, ui.visuals().text_color());
    ui.painter().circle_stroke(center, 5.0, stroke);
    ui.painter().circle_stroke(center, 1.8, stroke);
    for index in 0..8 {
        let angle = index as f32 * std::f32::consts::TAU / 8.0;
        let direction = egui::vec2(angle.cos(), angle.sin());
        ui.painter()
            .line_segment([center + direction * 5.0, center + direction * 8.0], stroke);
    }
    response.on_hover_text("Open settings")
}

pub fn insertion_marker(
    items: &[(Uuid, egui::Rect)],
    clip: egui::Rect,
    pointer: egui::Pos2,
) -> Option<InsertionMarker> {
    if !clip.contains(pointer) {
        return None;
    }
    let row_y = items
        .iter()
        .filter(|(_, rect)| rect.intersects(clip))
        .map(|(_, rect)| rect.center().y)
        .min_by(|a, b| (a - pointer.y).abs().total_cmp(&(b - pointer.y).abs()))?;
    let mut row: Vec<_> = items
        .iter()
        .filter(|(_, rect)| rect.intersects(clip) && (rect.center().y - row_y).abs() < 1.0)
        .collect();
    row.sort_by(|a, b| a.1.center().x.total_cmp(&b.1.center().x));
    for (index, (id, rect)) in row.iter().enumerate() {
        if pointer.x < rect.center().x {
            let x = if index == 0 {
                rect.left() - 4.0
            } else {
                (row[index - 1].1.right() + rect.left()) / 2.0
            };
            return Some(InsertionMarker {
                target: *id,
                side: DropSide::Before,
                x,
                top: rect.top() + 8.0,
                bottom: rect.bottom() - 8.0,
            });
        }
    }
    let (id, rect) = row.last()?;
    Some(InsertionMarker {
        target: *id,
        side: DropSide::After,
        x: rect.right() + 4.0,
        top: rect.top() + 8.0,
        bottom: rect.bottom() - 8.0,
    })
}

pub fn finish_drag(
    ui: &mut Ui,
    items: &[(Uuid, egui::Rect)],
    clip: egui::Rect,
) -> Option<(Uuid, Uuid, DropSide)> {
    let source = egui::DragAndDrop::payload::<Uuid>(ui.ctx())?;
    let pointer = ui.ctx().input(|input| input.pointer.hover_pos())?;
    let marker = insertion_marker(items, clip, pointer)?;
    paint_insertion_marker(ui, marker);
    if ui
        .ctx()
        .input(|input| input.pointer.button_released(egui::PointerButton::Primary))
    {
        egui::DragAndDrop::take_payload::<Uuid>(ui.ctx());
        Some((*source, marker.target, marker.side))
    } else {
        None
    }
}

fn paint_insertion_marker(ui: &Ui, marker: InsertionMarker) {
    ui.painter().line_segment(
        [
            egui::pos2(marker.x, marker.top),
            egui::pos2(marker.x, marker.bottom),
        ],
        egui::Stroke::new(3.0, ui.visuals().hyperlink_color),
    );
}

pub fn show_external_insertion(
    ui: &Ui,
    items: &[(Uuid, egui::Rect)],
    clip: egui::Rect,
    pointer: Option<egui::Pos2>,
) -> Option<InsertionMarker> {
    let marker = insertion_marker(items, clip, pointer?)?;
    paint_insertion_marker(ui, marker);
    Some(marker)
}

#[derive(Debug, PartialEq, Eq)]
pub enum IconAction {
    Launch,
    Activate,
    Choose,
}

pub fn icon_action(windows: usize, shift: bool) -> IconAction {
    if shift || windows == 0 {
        IconAction::Launch
    } else if windows == 1 {
        IconAction::Activate
    } else {
        IconAction::Choose
    }
}

pub fn render_icon(ui: &mut Ui, id: Uuid, name: &str, texture: TextureId) -> Response {
    let image = egui::Image::new((texture, Vec2::splat(32.0)));
    let response = ui
        .push_id(id, |ui| {
            ui.add(
                egui::Button::image(image)
                    .min_size(Vec2::splat(52.0))
                    .sense(egui::Sense::click_and_drag()),
            )
        })
        .inner
        .on_hover_text(format!(
            "{name}\nDrag to reorder or pin to the taskbar\nShift-click to open another window"
        ));
    if response.drag_started_by(egui::PointerButton::Primary) {
        egui::DragAndDrop::set_payload(ui.ctx(), id);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{
        self, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, TextureId, Vec2,
    };
    use uuid::Uuid;

    #[test]
    fn clicking_a_running_app_activates_or_offers_its_windows() {
        assert_eq!(icon_action(0, false), IconAction::Launch);
        assert_eq!(icon_action(1, false), IconAction::Activate);
        assert_eq!(icon_action(2, false), IconAction::Choose);
        assert_eq!(icon_action(2, true), IconAction::Launch);
    }

    #[test]
    fn icons_wrap_at_the_configured_row_length() {
        for columns in [1, 3, 4, 7, 12] {
            let ctx = egui::Context::default();
            let ids: Vec<_> = (0..13).map(|_| Uuid::new_v4()).collect();
            let mut rects = Vec::new();
            let mut output = ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 1000.0))),
                    ..Default::default()
                },
                |ui| {
                    icon_rows(ui, &ids, columns, |ui, id| {
                        rects.push(render_icon(ui, *id, "App", TextureId::default()).rect);
                    });
                },
            );
            output.textures_delta.clear();
            assert_eq!(rects.len(), ids.len());
            for index in 1..rects.len() {
                if index % columns == 0 {
                    assert!(
                        rects[index].top() > rects[index - 1].top(),
                        "column count {columns}"
                    );
                    assert_eq!(rects[index].left(), rects[0].left());
                } else {
                    assert_eq!(rects[index].top(), rects[index - 1].top());
                    assert!(rects[index].left() > rects[index - 1].left());
                }
            }
        }
    }

    #[test]
    fn settings_gear_is_clickable_at_the_lower_right_of_the_flyout() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(272.0, 112.0));
        let mut rect = Rect::NOTHING;
        let mut clicks = 0;
        let mut run = |events: Vec<Event>| {
            let mut output = ctx.run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    ui.set_min_size(ui.available_size());
                    let response = settings_button(ui);
                    rect = response.rect;
                    clicks += usize::from(response.clicked());
                },
            );
            output.textures_delta.clear();
            rect
        };
        let rect = run(vec![]);
        assert!(rect.right() >= screen.right() - 20.0);
        assert!(rect.bottom() >= screen.bottom() - 20.0);
        let point = rect.center();
        run(vec![
            Event::PointerMoved(point),
            Event::PointerButton {
                pos: point,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        run(vec![Event::PointerButton {
            pos: point,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert_eq!(clicks, 1);
    }

    #[test]
    fn settings_gear_does_not_overlap_visible_icons_in_the_flyout() {
        for count in [0, 1, 8, 9, 64] {
            let ctx = egui::Context::default();
            let (width, height) = taskbar_groups::flyout_size(count);
            let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height));
            let mut gear = Rect::NOTHING;
            let mut visible_icons = Vec::new();
            let mut scroll_clip = Rect::NOTHING;
            for _ in 0..2 {
                let mut output = ctx.run_ui(
                    RawInput {
                        screen_rect: Some(screen),
                        focused: true,
                        ..Default::default()
                    },
                    |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_min_size(ui.available_size());
                                if count == 0 {
                                    ui.label("No apps in this group yet.");
                                }
                                let mut items = Vec::new();
                                let ids: Vec<_> = (0..count).map(|_| Uuid::new_v4()).collect();
                                let scroll = egui::ScrollArea::vertical()
                                    .min_scrolled_height(0.0)
                                    .max_height((ui.available_height() - 28.0).max(0.0))
                                    .show(ui, |ui| {
                                        icon_rows(ui, &ids, 4, |ui, id| {
                                            items.push(
                                                render_icon(ui, *id, "App", TextureId::default())
                                                    .rect,
                                            );
                                        });
                                    });
                                scroll_clip = scroll.inner_rect;
                                visible_icons = items
                                    .into_iter()
                                    .filter(|rect| rect.intersects(scroll_clip))
                                    .map(|rect| rect.intersect(scroll_clip))
                                    .collect();
                                gear = settings_button(ui).rect;
                            });
                    },
                );
                output.textures_delta.clear();
            }
            assert!(
                screen.contains(gear.min) && screen.contains(gear.max),
                "gear must stay inside the window with {count} apps"
            );
            assert!(
                !scroll_clip.intersects(gear),
                "gear {gear:?} overlaps scrolling area {scroll_clip:?} with {count} apps"
            );
            assert!(
                visible_icons.iter().all(|rect| !rect.intersects(gear)),
                "gear overlaps an icon with {count} apps"
            );
        }
    }

    #[test]
    fn external_taskbar_drag_paints_the_same_insertion_divider() {
        let ctx = egui::Context::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let rect = |x| Rect::from_min_size(Pos2::new(x, 10.0), Vec2::splat(52.0));
        let items = [(first, rect(10.0)), (second, rect(70.0))];
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(150.0, 100.0));
        let mut result = None;
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(clip),
                ..Default::default()
            },
            |ui| {
                result = show_external_insertion(ui, &items, clip, Some(Pos2::new(66.0, 30.0)));
            },
        );
        assert_eq!(
            result.map(|m| (m.target, m.side)),
            Some((second, DropSide::Before))
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::LineSegment { points, stroke }
                if (points[0].x - 66.0).abs() < 0.1 && stroke.width == 3.0)));
        output.textures_delta.clear();
    }

    #[test]
    fn shared_layout_uses_physical_release_coordinates_at_any_dpi() {
        let group = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let items = [
            (
                a,
                Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(52.0)),
            ),
            (
                b,
                Rect::from_min_size(Pos2::new(70.0, 10.0), Vec2::splat(52.0)),
            ),
        ];
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(150.0, 100.0));
        let layout = DropLayout::new(group, 1.5, &items, clip);
        let serialized = serde_json::to_vec(&layout).unwrap();
        let restored: DropLayout = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored.group, group);
        let marker = restored.marker_at_client(99, 45).unwrap();
        assert_eq!((marker.target, marker.side), (b, DropSide::Before));
        assert!((marker.x - 66.0).abs() < 0.1);
        assert!(restored.marker_at_client(300, 45).is_none());
    }

    #[test]
    fn taskbar_drop_layout_is_replaced_when_the_flyout_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("layout.json");
        let group = Uuid::new_v4();
        let item = Uuid::new_v4();
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(150.0, 100.0));
        let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(52.0));
        let initial = DropLayout::new(group, 1.0, &[(item, rect)], clip);
        initial.write_atomic(&path).unwrap();
        assert_eq!(DropLayout::read(&path).unwrap(), initial);
        let updated = DropLayout::new(group, 2.0, &[(item, rect)], clip);
        updated.write_atomic(&path).unwrap();
        assert_eq!(DropLayout::read(&path).unwrap(), updated);
    }

    #[test]
    fn an_invalid_layout_is_not_used_for_a_taskbar_drop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("layout.json");
        let layout = DropLayout::new(
            Uuid::new_v4(),
            0.0,
            &[],
            Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 100.0)),
        );
        layout.write_atomic(&path).unwrap();
        assert!(DropLayout::read(&path).is_err());
    }

    #[test]
    fn a_shared_layout_places_a_taskbar_pin_at_the_drawn_divider() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("groups.json");
        let source = temp.path().join("New.lnk");
        let layout_path = temp.path().join("layout.json");
        std::fs::write(&source, b"shortcut").unwrap();
        let mut store = taskbar_groups::Store::default();
        let group = store.add_group("Work").unwrap();
        store.add_app(group, "A", "a.exe").unwrap();
        store.add_app(group, "B", "b.exe").unwrap();
        let a = store.groups[0].apps[0].id;
        let b = store.groups[0].apps[1].id;
        store.save(&store_path).unwrap();
        let items = [
            (
                a,
                Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(52.0)),
            ),
            (
                b,
                Rect::from_min_size(Pos2::new(70.0, 10.0), Vec2::splat(52.0)),
            ),
        ];
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(150.0, 100.0));
        DropLayout::new(group, 1.5, &items, clip)
            .write_atomic(&layout_path)
            .unwrap();
        let marker = DropLayout::read(&layout_path)
            .unwrap()
            .marker_at_client(99, 45)
            .unwrap();
        let (saved, unpin, _) = taskbar_groups::Store::move_pinned_shortcut_at(
            &store_path,
            group,
            "New",
            &source,
            Some((marker.target, marker.side)),
            |_| Ok(()),
        )
        .unwrap();
        assert!(unpin.is_ok());
        assert_eq!(
            saved.groups[0]
                .apps
                .iter()
                .map(|app| app.name.as_str())
                .collect::<Vec<_>>(),
            ["A", "New", "B"]
        );
    }

    #[test]
    fn insertion_marker_targets_gaps_and_both_row_ends() {
        use taskbar_groups::DropSide;
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let rect = |x, y| Rect::from_min_size(Pos2::new(x, y), Vec2::splat(52.0));
        let items = [
            (ids[0], rect(10.0, 10.0)),
            (ids[1], rect(70.0, 10.0)),
            (ids[2], rect(10.0, 70.0)),
        ];
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(150.0, 130.0));
        let at = |x, y| {
            insertion_marker(&items, clip, Pos2::new(x, y))
                .map(|marker| (marker.target, marker.side, marker.x))
        };
        assert_eq!(at(3.0, 30.0), Some((ids[0], DropSide::Before, 6.0)));
        assert_eq!(at(66.0, 30.0), Some((ids[1], DropSide::Before, 66.0)));
        assert_eq!(at(135.0, 30.0), Some((ids[1], DropSide::After, 126.0)));
        assert_eq!(at(90.0, 90.0), Some((ids[2], DropSide::After, 66.0)));
        assert_eq!(at(151.0, 30.0), None);
    }

    #[test]
    fn dropping_in_the_gap_uses_the_visible_insertion_marker() {
        let ctx = egui::Context::default();
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let mut rects = Vec::new();
        let mut drops = Vec::new();
        let mut run = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 100.0))),
                events,
                focused: true,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                rects.clear();
                ui.horizontal(|ui| {
                    for id in ids {
                        let response = render_icon(ui, id, "App", TextureId::default());
                        rects.push((id, response.rect));
                    }
                });
                if let Some(drop) = finish_drag(
                    ui,
                    &rects,
                    Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 100.0)),
                ) {
                    drops.push(drop);
                }
            });
            output.textures_delta.clear();
            rects.clone()
        };
        let rects = run(vec![]);
        let source = rects[2].1.center();
        let gap = Pos2::new(
            (rects[0].1.right() + rects[1].1.left()) / 2.0,
            rects[0].1.center().y,
        );
        run(vec![
            Event::PointerMoved(source),
            Event::PointerButton {
                pos: source,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        run(vec![Event::PointerMoved(gap)]);
        run(vec![Event::PointerButton {
            pos: gap,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert_eq!(drops, [(ids[2], ids[1], DropSide::Before)]);
    }

    #[test]
    fn insertion_works_at_start_between_icons_and_at_end_in_flyout_layout() {
        for case in 0..3 {
            let ctx = egui::Context::default();
            let ids = [
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
            ];
            let mut rects = Vec::new();
            let mut drops = Vec::new();
            let mut visible_markers = Vec::new();
            let marker_color = ctx.style_of(ctx.theme()).visuals.hyperlink_color;
            let mut run = |events: Vec<Event>| {
                let input = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(272.0, 84.0))),
                    events,
                    focused: true,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                    ui.spacing_mut().button_padding = Vec2::ZERO;
                    rects.clear();
                    egui::Frame::new()
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            let scroll = egui::ScrollArea::vertical().show(ui, |ui| {
                                icon_rows(ui, &ids, 4, |ui, id| {
                                    let response =
                                        render_icon(ui, *id, "App", TextureId::default());
                                    rects.push((*id, response.rect));
                                });
                            });
                            if let Some(drop) =
                                finish_drag(ui, &rects, drop_hit_area(scroll.inner_rect))
                            {
                                drops.push(drop);
                            }
                        });
                });
                for shape in &output.shapes {
                    if let egui::Shape::LineSegment { points, stroke } = &shape.shape
                        && stroke.width == 3.0
                        && stroke.color == marker_color
                        && shape.clip_rect.contains(points[0].lerp(points[1], 0.5))
                    {
                        visible_markers.push(points[0].x);
                    }
                }
                output.textures_delta.clear();
                rects.clone()
            };
            let rects = run(vec![]);
            let (source, destination, expected) = match case {
                0 => (
                    rects[3].1.center(),
                    Pos2::new(rects[0].1.left() - 4.0, rects[0].1.center().y),
                    (ids[3], ids[0], DropSide::Before),
                ),
                1 => (
                    rects[3].1.center(),
                    Pos2::new(
                        (rects[0].1.right() + rects[1].1.left()) / 2.0,
                        rects[0].1.center().y,
                    ),
                    (ids[3], ids[1], DropSide::Before),
                ),
                _ => (
                    rects[0].1.center(),
                    Pos2::new(rects[3].1.right() + 4.0, rects[3].1.center().y),
                    (ids[0], ids[3], DropSide::After),
                ),
            };
            run(vec![
                Event::PointerMoved(source),
                Event::PointerButton {
                    pos: source,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            run(vec![Event::PointerMoved(destination)]);
            run(vec![Event::PointerButton {
                pos: destination,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            assert_eq!(drops, [expected]);
            assert!(
                visible_markers
                    .iter()
                    .any(|x| (*x - destination.x).abs() < 0.1),
                "the insertion marker must be visible at the chosen position"
            );
        }
    }

    #[test]
    fn dragging_to_the_end_of_a_wrapped_row_uses_that_row() {
        let ctx = egui::Context::default();
        let ids: Vec<_> = (0..10).map(|_| Uuid::new_v4()).collect();
        let mut rects = Vec::new();
        let mut drops = Vec::new();
        let mut run = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(512.0, 144.0))),
                events,
                focused: true,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                ui.spacing_mut().button_padding = Vec2::ZERO;
                rects.clear();
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        let scroll = egui::ScrollArea::vertical().show(ui, |ui| {
                            icon_rows(ui, &ids, 8, |ui, id| {
                                let response = render_icon(ui, *id, "App", TextureId::default());
                                rects.push((*id, response.rect));
                            });
                        });
                        if let Some(drop) = finish_drag(ui, &rects, scroll.inner_rect) {
                            drops.push(drop);
                        }
                    });
            });
            output.textures_delta.clear();
            rects.clone()
        };
        let _ = run(vec![]);
        let rects = run(vec![]);
        assert!(rects[9].1.center().y > rects[0].1.center().y);
        let source = rects[0].1.center();
        let destination = Pos2::new(rects[9].1.right() - 2.0, rects[9].1.center().y);
        run(vec![
            Event::PointerMoved(source),
            Event::PointerButton {
                pos: source,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        run(vec![Event::PointerMoved(destination)]);
        run(vec![Event::PointerButton {
            pos: destination,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert_eq!(drops, [(ids[0], ids[9], DropSide::After)]);
    }

    #[test]
    fn dragging_between_icons_reorders_without_launching_either_app() {
        let ctx = egui::Context::default();
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let mut positions = Vec::new();
        let mut rects = Vec::new();
        let mut drops = Vec::new();
        let mut clicks = 0;
        let mut run = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 100.0))),
                events,
                focused: true,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                positions.clear();
                rects.clear();
                ui.horizontal(|ui| {
                    for id in ids {
                        let response = render_icon(ui, id, "App", TextureId::default());
                        positions.push(response.rect.center());
                        rects.push((id, response.rect));
                        if response.clicked() {
                            clicks += 1;
                        }
                    }
                });
                if let Some(drop) = finish_drag(
                    ui,
                    &rects,
                    Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 100.0)),
                ) {
                    drops.push(drop);
                }
            });
            output.textures_delta.clear();
            positions.clone()
        };
        let positions = run(vec![]);
        let source = positions[0];
        let destination = positions[1];
        run(vec![
            Event::PointerMoved(source),
            Event::PointerButton {
                pos: source,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        run(vec![Event::PointerMoved(destination)]);
        run(vec![Event::PointerButton {
            pos: destination,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert_eq!(drops, [(ids[0], ids[1], DropSide::After)]);
        assert_eq!(clicks, 0);
    }

    #[test]
    fn ordinary_and_secondary_clicks_still_work() {
        let ctx = egui::Context::default();
        let id = Uuid::new_v4();
        let run = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(150.0, 100.0))),
                events,
                focused: true,
                ..Default::default()
            };
            let mut result = None;
            let mut output = ctx.run_ui(input, |ui| {
                let response = render_icon(ui, id, "App", TextureId::default());
                result = Some((
                    response.rect.center(),
                    response.clicked(),
                    response.secondary_clicked(),
                    egui::DragAndDrop::has_payload_of_type::<Uuid>(ui.ctx()),
                ));
            });
            output.textures_delta.clear();
            result.unwrap()
        };
        let point = run(vec![]).0;
        for button in [PointerButton::Primary, PointerButton::Secondary] {
            run(vec![
                Event::PointerMoved(point),
                Event::PointerButton {
                    pos: point,
                    button,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            let (_, primary, secondary, drop) = run(vec![Event::PointerButton {
                pos: point,
                button,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            assert_eq!(primary, button == PointerButton::Primary);
            assert_eq!(secondary, button == PointerButton::Secondary);
            assert!(!drop);
        }
    }
}
