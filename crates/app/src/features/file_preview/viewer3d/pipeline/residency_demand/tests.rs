use super::*;
use crate::features::file_preview::viewer3d::RenderMode;
use crate::features::file_preview::viewer3d::draw_plan::DrawItem;
use std::sync::atomic::{AtomicUsize, Ordering};

fn item(slot: usize) -> DrawItem {
    DrawItem {
        mesh_index: slot,
        material_slot: slot,
    }
}
fn plans(plan: DrawPlan) -> DrawPlans {
    DrawPlans {
        world: Arc::new(plan),
        ..DrawPlans::default()
    }
}
fn draw(primitive: Primitive) -> super::super::transparency::Draw {
    super::super::transparency::Draw {
        primitive,
        depth: 0.0,
        blend: RenderMode::Translucent,
    }
}

#[test]
fn selected_slots_and_final_queue_are_authoritative() {
    let plan = DrawPlan {
        opaque: vec![item(4)],
        planar: vec![item(7)],
        translucent: vec![item(29)],
        additive: vec![item(30)],
        ..DrawPlan::default()
    };
    let queue = [
        draw(Primitive::Mesh(item(9), Some(0..3))),
        draw(Primitive::Sprite(0..6)),
        draw(Primitive::Mesh(item(31), Some(0..0))),
    ];
    let slots = ViewDemandCache::default()
        .collect_parts(
            &plans(plan),
            WorldDrawGroups::ALL,
            [&queue, &[]],
            [Some(11), None],
            32,
            true,
        )
        .unwrap();
    assert_eq!(&*slots, &[4, 7, 9, 11]);
}

#[test]
fn intersection_uses_world_without_unrendered_skybox_queue() {
    let mut plans = plans(DrawPlan {
        opaque: vec![item(2)],
        ..DrawPlan::default()
    });
    plans.map_skybox = Some(Arc::new(DrawPlan {
        opaque: vec![item(8)],
        ..DrawPlan::default()
    }));
    let sky_queue = [draw(Primitive::Sprite(0..6))];
    let mut cache = ViewDemandCache::default();
    assert_eq!(
        &*cache
            .collect_parts(
                &plans,
                WorldDrawGroups::ALL,
                [&[], &sky_queue],
                [None, Some(9)],
                10,
                false
            )
            .unwrap(),
        &[2]
    );
    assert_eq!(
        &*cache
            .collect_parts(
                &plans,
                WorldDrawGroups::ALL,
                [&[], &sky_queue],
                [None, Some(9)],
                10,
                true
            )
            .unwrap(),
        &[2, 8, 9]
    );
}

#[test]
fn missing_metadata_is_typed_not_a_filtered_material() {
    let plans = plans(DrawPlan {
        opaque: vec![item(3)],
        ..DrawPlan::default()
    });
    let mut cache = ViewDemandCache::default();
    assert_eq!(
        cache.collect_parts(&plans, WorldDrawGroups::ALL, [&[], &[]], [None; 2], 3, true),
        Err(DemandError::InvalidMaterial { slot: 3, count: 3 })
    );
    let queue = [draw(Primitive::Sprite(0..6))];
    assert_eq!(
        cache.collect_parts(
            &DrawPlans::default(),
            WorldDrawGroups::ALL,
            [&queue, &[]],
            [None; 2],
            4,
            true
        ),
        Err(DemandError::MissingDetails { skybox: false })
    );
}

#[test]
fn exact_plan_cache_is_weak_and_replaces_instead_of_accumulating() {
    let plan = Arc::new(DrawPlan {
        opaque: vec![item(1)],
        ..DrawPlan::default()
    });
    let mut entry = None;
    let first = cached_plan(&mut entry, &plan, WorldDrawGroups::ALL);
    let second = cached_plan(&mut entry, &plan, WorldDrawGroups::ALL);
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(Arc::strong_count(&plan), 1);
    let weak = Arc::downgrade(&plan);
    drop(plan);
    assert!(weak.upgrade().is_none());
    let replacement = Arc::new(DrawPlan {
        opaque: vec![item(5)],
        ..DrawPlan::default()
    });
    assert_eq!(
        &*cached_plan(&mut entry, &replacement, WorldDrawGroups::ALL),
        &[5]
    );
}

#[derive(Debug)]
struct Pin(Arc<AtomicUsize>);
impl Drop for Pin {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn submitted_pins_retire_only_at_completion_and_unsubmitted_cancellation_is_local() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut completion = None;
    SubmissionPins::new(vec![Arc::new(Pin(Arc::clone(&dropped)))])
        .retire_with(|callback| completion = Some(callback));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    completion.take().unwrap()();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    let cancelled = SubmissionPins::new(vec![Arc::new(Pin(Arc::clone(&dropped)))]);
    drop(cancelled);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

#[test]
fn completed_material_slots_match_independent_set_oracle_for_all_small_inputs() {
    let values = [0, 1, 2, 3, usize::MAX];
    for mask in 0..1usize << values.len() {
        let original: Arc<[usize]> = values
            .iter()
            .enumerate()
            .filter_map(|(bit, &slot)| (mask & (1 << bit) != 0).then_some(slot))
            .collect();
        for sun in values {
            for debug in values {
                for duplicate in values {
                    let additional = [sun, debug, duplicate];
                    let expected: BTreeSet<_> =
                        original.iter().copied().chain(additional).collect();
                    let expected: Arc<[usize]> = expected.into_iter().collect();
                    let actual = sorted_material_slots(Arc::clone(&original), additional);
                    assert_eq!(actual, expected, "mask={mask}, additional={additional:?}");
                    for count in [0, 1, 3, 4, usize::MAX] {
                        let expected = match expected.iter().find(|&&slot| slot >= count) {
                            Some(&slot) => Err(DemandError::InvalidMaterial { slot, count }),
                            None => Ok(Arc::clone(&expected)),
                        };
                        assert_eq!(
                            complete_material_slots(Arc::clone(&original), additional, count),
                            expected,
                            "mask={mask}, additional={additional:?}, count={count}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sorted_material_slots_reuse_unchanged_arc_and_keep_sorted_sun_debug_additions() {
    let original: Arc<[usize]> = Arc::from([1, 4, 9]);
    let none = sorted_material_slots(Arc::clone(&original), []);
    let existing = sorted_material_slots(Arc::clone(&original), [9, 1, 4, 1]);
    assert!(Arc::ptr_eq(&none, &original));
    assert!(Arc::ptr_eq(&existing, &original));
    let appended = sorted_material_slots(Arc::clone(&original), [8, 0, 4, 10, 8]);
    assert_eq!(&*appended, &[0, 1, 4, 8, 9, 10]);
    assert_eq!(&*original, &[1, 4, 9]);
    assert!(!Arc::ptr_eq(&appended, &original));
    let empty: Arc<[usize]> = Arc::from([]);
    let unchanged = sorted_material_slots(Arc::clone(&empty), []);
    assert!(Arc::ptr_eq(&empty, &unchanged));
}

#[test]
fn completed_material_slots_preserve_first_invalid_slot_including_maximum() {
    assert_eq!(
        complete_material_slots(Arc::from([2, 9]), [usize::MAX, 7, 5], 5),
        Err(DemandError::InvalidMaterial { slot: 5, count: 5 })
    );
    assert_eq!(
        complete_material_slots(Arc::from([]), [usize::MAX], usize::MAX),
        Err(DemandError::InvalidMaterial {
            slot: usize::MAX,
            count: usize::MAX,
        })
    );
}
