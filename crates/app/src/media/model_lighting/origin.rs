use crate::media::preview_model::ModelData;
use gmpublished_domain::{
    math::Vec3,
    scene::map::{MapBounds, StaticPropPlacement},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelLightingOriginFailure {
    InvalidTransform,
    UnresolvedAnchor { name: String },
    AmbiguousAnchor { name: String, matches: usize },
    UnresolvedAttachment { index: usize },
}

pub fn placement_sample_point(
    model: &ModelData,
    placement: &StaticPropPlacement,
) -> Result<Vec3, ModelLightingOriginFailure> {
    resolved_sample_point(
        model.illumination_position,
        model.illumination_attachment_index,
        model.illumination_attachment_position,
        placement,
    )
}

pub fn placement_static_bounds(
    model: &ModelData,
    placement: &StaticPropPlacement,
) -> Option<MapBounds> {
    if placement.entity
        || !model.hull_min.is_finite()
        || !model.hull_max.is_finite()
        || !placement.scale.is_finite()
        || placement.scale <= 0.0
        || !placement.origin.is_finite()
        || !placement.angles.is_finite()
        || (0..3).any(|axis| model.hull_min[axis] > model.hull_max[axis])
    {
        return None;
    }
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for corner in 0..8 {
        let local = Vec3::from(std::array::from_fn(|axis| {
            if corner & (1 << axis) == 0 {
                model.hull_min[axis]
            } else {
                model.hull_max[axis]
            }
        }));
        let world = (local * placement.scale).rotate_source(placement.angles) + placement.origin;
        if !world.is_finite() {
            return None;
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(world[axis]);
            max[axis] = max[axis].max(world[axis]);
        }
    }
    Some(MapBounds { min, max })
}

fn resolved_sample_point(
    illumination: Vec3,
    attachment_index: Option<usize>,
    attachment_position: Option<Vec3>,
    placement: &StaticPropPlacement,
) -> Result<Vec3, ModelLightingOriginFailure> {
    if !placement.entity && (!placement.scale.is_finite() || placement.scale <= 0.0) {
        return Err(ModelLightingOriginFailure::InvalidTransform);
    }
    if let Some(reference) = &placement.lighting_origin_reference {
        if reference.match_count > 1 {
            return Err(ModelLightingOriginFailure::AmbiguousAnchor {
                name: reference.name.clone(),
                matches: reference.match_count,
            });
        }
        if placement.lighting_origin.is_none() {
            return Err(ModelLightingOriginFailure::UnresolvedAnchor {
                name: reference.name.clone(),
            });
        }
    }
    if let Some(anchor) = placement.lighting_origin {
        return anchor
            .is_finite()
            .then_some(anchor)
            .ok_or(ModelLightingOriginFailure::InvalidTransform);
    }
    let offset = if placement.entity {
        if let Some(index) = attachment_index {
            attachment_position.ok_or(ModelLightingOriginFailure::UnresolvedAttachment { index })?
        } else {
            illumination
        }
    } else {
        illumination * placement.scale
    };
    if !offset.is_finite() || !placement.origin.is_finite() || !placement.angles.is_finite() {
        return Err(ModelLightingOriginFailure::InvalidTransform);
    }
    let point = offset.rotate_source(placement.angles) + placement.origin;
    point
        .is_finite()
        .then_some(point)
        .ok_or(ModelLightingOriginFailure::InvalidTransform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::{
        MapLightingOriginReference, MapPropSolid, MapPropVisibility,
    };

    fn placement(entity: bool) -> StaticPropPlacement {
        StaticPropPlacement {
            entity,
            static_prop_index: None,
            lighting_origin: None,
            lighting_origin_reference: None,
            diffuse_modulation: [255; 4],
            render_mode: 0,
            flags: 0,
            flags_ex: None,
            extra_flags: None,
            model_path: "models/fixture.mdl".into(),
            origin: Vec3::new(10.0, 20.0, 30.0),
            angles: Vec3::new(0.0, 90.0, 0.0),
            skin: 0,
            scale: 2.0,
            solid: MapPropSolid::None,
            visibility: MapPropVisibility::Always,
        }
    }

    #[test]
    fn native_entity_unscaled_offset_and_static_scaled_offset_rotate_before_translation() {
        let illumination = Vec3::new(0.0, 32.0, 0.0);
        let entity = resolved_sample_point(illumination, None, None, &placement(true)).unwrap();
        let compiled = resolved_sample_point(illumination, None, None, &placement(false)).unwrap();
        assert!(entity.distance(Vec3::new(-22.0, 20.0, 30.0)) < 1.0e-5);
        assert!(compiled.distance(Vec3::new(-54.0, 20.0, 30.0)) < 1.0e-5);
    }

    #[test]
    fn entity_attachment_is_independent_of_static_illumination_and_unresolved_selector_is_explicit()
    {
        let illumination = Vec3::new(0.0, 32.0, 0.0);
        let attachment = Some(Vec3::new(0.0, 128.0, 0.0));
        let entity =
            resolved_sample_point(illumination, Some(0), attachment, &placement(true)).unwrap();
        let compiled =
            resolved_sample_point(illumination, Some(0), attachment, &placement(false)).unwrap();
        assert!(entity.distance(Vec3::new(-118.0, 20.0, 30.0)) < 1.0e-5);
        assert!(compiled.distance(Vec3::new(-54.0, 20.0, 30.0)) < 1.0e-5);
        assert_eq!(
            resolved_sample_point(illumination, Some(9), None, &placement(true)),
            Err(ModelLightingOriginFailure::UnresolvedAttachment { index: 9 })
        );
    }

    #[test]
    fn world_anchor_replaces_complete_offset_and_unresolved_names_do_not_invent_positions() {
        let mut prop = placement(true);
        prop.lighting_origin = Some(Vec3::new(7.0, 8.0, 9.0));
        assert_eq!(
            resolved_sample_point(Vec3::splat(32.0), Some(9), None, &prop),
            prop.lighting_origin
                .ok_or(ModelLightingOriginFailure::InvalidTransform)
        );
        prop.lighting_origin = None;
        prop.lighting_origin_reference = Some(MapLightingOriginReference {
            name: "missing".into(),
            first_entity: None,
            match_count: 0,
        });
        assert_eq!(
            resolved_sample_point(Vec3::ZERO, None, None, &prop),
            Err(ModelLightingOriginFailure::UnresolvedAnchor {
                name: "missing".into()
            })
        );
        prop.lighting_origin = Some(Vec3::ZERO);
        prop.lighting_origin_reference.as_mut().unwrap().match_count = 2;
        assert_eq!(
            resolved_sample_point(Vec3::ZERO, None, None, &prop),
            Err(ModelLightingOriginFailure::AmbiguousAnchor {
                name: "missing".into(),
                matches: 2
            })
        );
    }
}
