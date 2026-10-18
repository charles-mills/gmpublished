use super::super::OverlayDrawItem;
use super::*;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Indexed { key: Key, indices: Range<u32> },
    Overlay,
    Sprite(Range<u32>),
}

type Resolved = Option<(bool, Vec<PageRange>)>;

fn mesh(item: DrawItem, blend: RenderMode, indices: Option<Range<u32>>) -> Draw {
    Draw {
        primitive: Primitive::Mesh(item, indices),
        blend,
        depth: 0.0,
    }
}

fn part(start: u32, page: Option<LightmapPageId>) -> PageRange {
    PageRange {
        indices: start..start + 3,
        page,
    }
}

fn build(slot: &mut Slot, operations: &[Draw], resolutions: &[Resolved]) {
    let mut responses = operations
        .iter()
        .zip(resolutions)
        .filter(|(operation, _)| matches!(operation.primitive, Primitive::Mesh(_, _)))
        .map(|(_, resolved)| resolved);
    slot.build(operations, |_, _| {
        let (visible, parts) = responses.next().unwrap().as_ref()?;
        match parts.as_slice() {
            [part] => Some((*visible, part.clone())),
            _ => None,
        }
    });
    assert!(responses.next().is_none());
}

// Direct-draw oracle, independent of indirect-run grouping.
fn original(operations: &[Draw], resolutions: &[Resolved]) -> Vec<Event> {
    let mut events = Vec::new();
    for (operation, resolved) in operations.iter().zip(resolutions) {
        match &operation.primitive {
            Primitive::Mesh(item, _) => {
                if let Some((visible, parts)) = resolved {
                    for part in parts {
                        events.push(Event::Indexed {
                            key: Key {
                                item: *item,
                                blend: operation.blend,
                                visible: *visible,
                                page: part.page,
                            },
                            indices: part.indices.clone(),
                        });
                    }
                }
            }
            Primitive::Overlay(_) => events.push(Event::Overlay),
            Primitive::Sprite(range) => events.push(Event::Sprite(range.clone())),
        }
    }
    events
}

fn replay(slot: &Slot, operations: &[Draw], resolutions: &[Resolved]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut index = 0;
    while index < operations.len() {
        if let Some(run) = slot.runs.iter().find(|run| run.operations.start == index) {
            let Primitive::Mesh(item, _) = operations[index].primitive else {
                panic!("only complete Mesh operations can form a run");
            };
            let (visible, _) = resolutions[index].as_ref().unwrap();
            let first = usize::try_from(run.offset).unwrap() / ARGUMENT_STRIDE;
            for argument in &slot.arguments[first..first + run.operations.len()] {
                assert_eq!(argument.instance_count, 1);
                assert_eq!(argument.base_vertex, 0);
                assert_eq!(argument.first_instance, 0);
                events.push(Event::Indexed {
                    key: Key {
                        item,
                        blend: operations[index].blend,
                        visible: *visible,
                        page: run.page,
                    },
                    indices: argument.first_index..argument.first_index + argument.index_count,
                });
            }
            index = run.operations.end;
        } else {
            events.extend(original(
                &operations[index..index + 1],
                &resolutions[index..index + 1],
            ));
            index += 1;
        }
    }
    events
}

#[test]
fn ordered_draws_match_independent_oracle_across_whole_operation_barriers() {
    let a = DrawItem {
        mesh_index: 2,
        material_slot: 3,
    };
    let b = DrawItem {
        material_slot: 4,
        ..a
    };
    let mut operations = Vec::new();
    let mut resolutions = Vec::new();
    {
        let mut append = |item, blend, visible, parts: Vec<PageRange>| {
            let range = parts
                .first()
                .map(|part| part.indices.start)
                .zip(parts.last().map(|part| part.indices.end))
                .map(|(start, end)| start..end);
            operations.push(mesh(item, blend, if visible { None } else { range }));
            resolutions.push(Some((visible, parts)));
        };
        append(a, RenderMode::Translucent, false, vec![part(9, None)]);
        append(a, RenderMode::Translucent, false, vec![part(3, None)]);
        append(a, RenderMode::Translucent, false, vec![part(3, None)]);
        append(a, RenderMode::Translucent, true, vec![part(0, None)]);
        append(a, RenderMode::Translucent, true, vec![part(6, None)]);
        append(a, RenderMode::Additive, true, vec![part(12, None)]);
        append(b, RenderMode::Additive, true, vec![part(15, None)]);
        append(b, RenderMode::Additive, true, vec![part(18, None)]);
        append(
            b,
            RenderMode::Additive,
            true,
            vec![part(21, None), part(24, Some(LightmapPageId(5)))],
        );
        append(
            b,
            RenderMode::Additive,
            true,
            vec![part(27, Some(LightmapPageId(5)))],
        );
        append(
            b,
            RenderMode::Additive,
            true,
            vec![part(30, Some(LightmapPageId(5)))],
        );
        let other_mesh = DrawItem { mesh_index: 7, ..b };
        append(
            other_mesh,
            RenderMode::Additive,
            true,
            vec![part(33, Some(LightmapPageId(5)))],
        );
        append(
            other_mesh,
            RenderMode::Additive,
            true,
            vec![part(36, Some(LightmapPageId(5)))],
        );
    }
    let overlay = OverlayDrawItem {
        blend: RenderMode::Translucent,
        receiver_mesh_index: None,
        receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
            face: 0,
            start: 0,
            count: 3,
        },
        overlay_index: 0,
        material_slot: 0,
    };
    operations.push(Draw {
        primitive: Primitive::Overlay(overlay),
        blend: RenderMode::Translucent,
        depth: 0.0,
    });
    resolutions.push(None);
    for primitive in [
        Primitive::Mesh(a, Some(33..36)),
        Primitive::Sprite(3..6),
        Primitive::Mesh(a, Some(36..39)),
        Primitive::Mesh(a, None), // missing material or empty visible buffer
        Primitive::Mesh(a, Some(39..42)),
        Primitive::Mesh(a, Some(42..42)), // an emitted empty part is a barrier
        Primitive::Mesh(a, Some(42..45)),
    ] {
        let response = match &primitive {
            Primitive::Mesh(_, Some(indices)) => Some((
                false,
                vec![PageRange {
                    indices: indices.clone(),
                    page: None,
                }],
            )),
            _ => None,
        };
        operations.push(Draw {
            primitive,
            blend: RenderMode::Translucent,
            depth: 0.0,
        });
        resolutions.push(response);
    }
    let mut slot = Slot::default();
    build(&mut slot, &operations, &resolutions);
    assert_eq!(
        slot.runs
            .iter()
            .map(|run| run.operations.clone())
            .collect::<Vec<_>>(),
        [0..3, 3..5, 6..8, 9..11, 11..13]
    );
    assert_eq!(slot.arguments.len(), 11);
    assert_eq!(
        replay(&slot, &operations, &resolutions),
        original(&operations, &resolutions)
    );
}

#[test]
fn rebuild_replaces_old_descriptors_and_zero_count_and_unsupported_stay_direct() {
    let a = DrawItem {
        mesh_index: 0,
        material_slot: 0,
    };
    let first = vec![mesh(a, RenderMode::Translucent, Some(0..3)); 3];
    let resolutions = vec![Some((false, vec![part(0, None)])); 3];
    let mut slot = Slot::default();
    build(&mut slot, &first, &resolutions);
    assert_eq!(slot.runs.len(), 1);
    let second = [
        mesh(a, RenderMode::Modulate, Some(9..12)),
        mesh(a, RenderMode::Modulate, Some(12..15)),
    ];
    let changed = [
        Some((false, vec![part(9, Some(LightmapPageId(8)))])),
        Some((false, vec![part(12, Some(LightmapPageId(8)))])),
    ];
    build(&mut slot, &second, &changed);
    assert_eq!(slot.runs.len(), 1);
    assert_eq!(slot.arguments.len(), 2);
    assert_eq!(
        replay(&slot, &second, &changed),
        original(&second, &changed)
    );
    build(&mut slot, &[], &[]);
    assert!(slot.arguments.is_empty());
    assert!(slot.runs.is_empty());
    assert_eq!(slot.one_part_operations, 0);
    assert!(slot.program().is_none());
    let unsupported = Slot::default();
    assert!(unsupported.program().is_none());
    assert_eq!(
        replay(&unsupported, &second, &changed),
        original(&second, &changed)
    );
}

#[test]
fn overflow_preserves_every_original_draw_with_bounded_storage() {
    let a = DrawItem {
        mesh_index: 0,
        material_slot: 0,
    };
    let operations = vec![mesh(a, RenderMode::Translucent, Some(0..3)); MAX_ARGUMENTS + 1];
    let mut slot = Slot::default();
    slot.build(&operations, |_, _| Some((false, part(0, None))));
    assert!(slot.overflowed);
    assert!(slot.runs.is_empty());
    assert!(slot.arguments.is_empty());
    assert!(slot.arguments.capacity() <= MAX_ARGUMENTS);
    assert!(slot.runs.capacity() <= MAX_ARGUMENTS / 2);
    assert!(slot.program().is_none());
    let resolutions = vec![Some((false, vec![part(0, None)])); operations.len()];
    assert_eq!(
        replay(&slot, &operations, &resolutions),
        original(&operations, &resolutions)
    );
}

#[test]
fn pass_transfer_segments_never_select_partial_or_neighboring_runs() {
    let runs = [
        Run {
            operations: 0..3,
            page: None,
            offset: 0,
        },
        Run {
            operations: 4..6,
            page: None,
            offset: 60,
        },
        Run {
            operations: 6..9,
            page: None,
            offset: 100,
        },
    ];
    assert_eq!(segment_runs(&runs, 0..6), &runs[..2]);
    assert_eq!(segment_runs(&runs, 6..10), &runs[2..]);
    assert_eq!(segment_runs(&runs, 3..6), &runs[1..2]);
    assert!(segment_runs(&runs, 1..3).is_empty());
    assert!(segment_runs(&runs, 0..2).is_empty());
    assert!(segment_runs(&runs, 6..6).is_empty());
    assert!(segment_runs(&runs, 9..20).is_empty());
}

#[test]
fn original_and_compacted_visible_page_runs_coalesce_equal_neighbors() {
    use super::super::lightmap_ranges::Ranges;
    let vertices = [1, 1, 1, 2, 2, 2, 1, 1, 1].map(|lightmap_page| super::super::ModelVertex {
        lightmap_page,
        ..bytemuck::Zeroable::zeroed()
    });
    let original = Ranges::new(&vertices, &(0..9).collect::<Vec<_>>()).unwrap();
    assert_eq!(original.indexed.len(), 3);
    assert_eq!(original.intersect(0..9).count(), 3);
    // with_lightmaps uses exactly select: removing the intervening other page
    // must join both surviving page runs in the packed visible index space.
    assert_eq!(
        original.select(&[0, 1, 2, 6, 7, 8]).unwrap(),
        [PageRange {
            indices: 0..6,
            page: Some(LightmapPageId(0))
        }]
    );
}

#[test]
fn every_render_mode_is_an_independent_ordered_run() {
    let item = DrawItem {
        mesh_index: 0,
        material_slot: 0,
    };
    let mut operations = Vec::new();
    let mut resolutions = Vec::new();
    for blend in [
        RenderMode::Opaque,
        RenderMode::Cutout,
        RenderMode::Translucent,
        RenderMode::Additive,
        RenderMode::Modulate,
        RenderMode::NoDraw,
    ] {
        for indices in [6..9, 0..3] {
            operations.push(mesh(item, blend, Some(indices.clone())));
            resolutions.push(Some((
                false,
                vec![PageRange {
                    indices,
                    page: None,
                }],
            )));
        }
    }
    let mut slot = Slot::default();
    build(&mut slot, &operations, &resolutions);
    assert_eq!(ARGUMENT_STRIDE, 20);
    assert_eq!(slot.runs.len(), 6);
    assert!(slot.runs.iter().all(|run| run.count() == 2));
    assert_eq!(
        replay(&slot, &operations, &resolutions),
        original(&operations, &resolutions)
    );
}
