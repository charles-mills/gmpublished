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
