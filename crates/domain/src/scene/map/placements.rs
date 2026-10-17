use super::{
    BTreeSet, GeometryPartition, MapBsp, MapEntity, MapPropLeafMembership, MapPropSolid,
    MapPropVisibility, MapVisibilityBucket, SkyboxPartition, StaticProp, StaticPropPlacement,
    cluster_in_range, normalize_entity_prop_model_path, normalize_static_prop_model_path,
    parse_entity_bool, parse_entity_float, parse_entity_i32, parse_entity_vec3,
    point_visibility_bucket,
};
use crate::math::Vec3;

/// `SolidType::Physics` (the vphysics collision mode) in the static prop
/// game lump's `solid` byte.
const STATIC_PROP_SOLID_PHYSICS: u8 = 6;
const STATIC_PROP_USE_LIGHTING_ORIGIN: u8 = 0x02;

const DETAIL_PROP_TYPE_SPRITE: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapLightingOriginReference {
    pub name: String,
    /// Authored BSP entity index, independent of runtime edict allocation.
    pub first_entity: Option<usize>,
    pub match_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapDetailSprite {
    pub origin: Vec3,
    pub angles: Vec3,
    pub orientation: u8,
    pub fast_eligible: bool,
    pub leaf_index: u16,
    pub underwater_leaf: bool,
    pub upper_left: [f32; 2],
    pub lower_right: [f32; 2],
    pub tex_upper_left: [f32; 2],
    pub tex_lower_right: [f32; 2],
    pub visibility: MapVisibilityBucket,
}

pub(super) fn partitioned_static_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for prop in bsp
        .static_props_iter()
        .enumerate()
        .filter_map(|(index, prop)| {
            let origin = prop.origin;
            let leaves = static_prop_render_leaves(bsp, prop);
            let visibility = static_prop_leaf_visibility(bsp, prop, &leaves, cluster_count);
            Some(StaticPropPlacement {
                entity: false,
                initially_visible: true,
                animation: None,
                static_prop_index: Some(index),
                lighting_origin: (prop.flags & STATIC_PROP_USE_LIGHTING_ORIGIN != 0)
                    .then_some(Vec3::from(prop.lighting_origin))
                    .filter(|origin| origin.is_finite()),
                lighting_origin_reference: None,
                diffuse_modulation: prop.diffuse_modulation.unwrap_or([255; 4]),
                render_mode: 0,
                flags: prop.flags,
                flags_ex: prop.flags_ex,
                extra_flags: prop.extra_flags,
                model_path: normalize_static_prop_model_path(bsp.static_prop_model(prop))?,
                origin: Vec3::from(origin),
                angles: Vec3::from(prop.angles),
                skin: prop.skin,
                scale: prop
                    .uniform_scale
                    .filter(|scale| scale.is_finite() && *scale > 0.0)
                    .unwrap_or(1.0),
                solid: static_prop_solid(prop.solid),
                leaf_membership: MapPropLeafMembership::Compiled(leaves),
                visibility,
            })
        })
    {
        match partition.point_partition(bsp, prop.origin) {
            GeometryPartition::Visible => visible.push(prop),
            GeometryPartition::Skybox => skybox.push(prop),
        }
    }
    (visible, skybox)
}

fn static_prop_render_leaves(bsp: &MapBsp, prop: &StaticProp) -> Vec<u16> {
    let model = bsp.static_prop_model(prop);
    let leaf_count = usize::from(prop.leaf_count);
    if leaf_count == 0 {
        log::debug!(
            "bsp static prop {model} at {:?} has empty leaf list: not registered",
            prop.origin
        );
        return Vec::new();
    }
    let Some(prop_leaves) = bsp.static_prop_leaves(prop) else {
        log::debug!(
            "bsp static prop {model} leaf list out of range first={} count={leaf_count}: not registered",
            prop.first_leaf
        );
        return Vec::new();
    };
    prop_leaves.to_vec()
}

fn static_prop_leaf_visibility(
    bsp: &MapBsp,
    prop: &StaticProp,
    prop_leaves: &[u16],
    cluster_count: u32,
) -> MapPropVisibility {
    if prop_leaves.is_empty() {
        return MapPropVisibility::Always;
    }
    let model = bsp.static_prop_model(prop);
    let leaf_count = prop_leaves.len();
    let mut clusters = BTreeSet::<u32>::new();
    let mut invalid_clusters = 0_usize;
    for leaf_index in prop_leaves {
        let Some(leaf) = bsp.leaves.get(usize::from(*leaf_index)) else {
            log::debug!(
                "bsp static prop {model} leaf {leaf_index} missing from BSP leaves: using Always"
            );
            return MapPropVisibility::Always;
        };
        if cluster_in_range(leaf.cluster, cluster_count) {
            let Ok(cluster) = u32::try_from(leaf.cluster) else {
                debug_assert!(false, "cluster_in_range admitted a negative cluster");
                continue;
            };
            clusters.insert(cluster);
        } else {
            invalid_clusters = invalid_clusters.saturating_add(1);
        }
    }
    if clusters.is_empty() {
        log::debug!(
            "bsp static prop {model} leaf list first={} count={leaf_count} had no valid clusters (invalid leaves {invalid_clusters}): using Always",
            prop.first_leaf
        );
        MapPropVisibility::Always
    } else {
        MapPropVisibility::Clusters(clusters.into_iter().collect())
    }
}

const ENTITY_PROP_CLASSNAMES: &[&str] = &[
    "dynamic_prop",
    "prop_dynamic",
    "prop_dynamic_override",
    "prop_physics",
    "prop_physics_multiplayer",
    "prop_physics_override",
];

const EFFECT_NO_DRAW: i32 = 0x20;

fn is_dynamic_prop(classname: &str) -> bool {
    ["prop_dynamic", "prop_dynamic_override", "dynamic_prop"]
        .iter()
        .any(|expected| classname.eq_ignore_ascii_case(expected))
}

pub(super) fn partitioned_entity_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for (entity_index, entity) in bsp.entities.iter().enumerate() {
        let Some(classname) = entity.prop("classname").filter(|classname| {
            ENTITY_PROP_CLASSNAMES
                .iter()
                .any(|expected| classname.eq_ignore_ascii_case(expected))
        }) else {
            continue;
        };
        let Some(placement) =
            entity_prop_placement(entity, entity_index, classname, bsp, cluster_count)
        else {
            continue;
        };
        if entity.prop("parentname").is_some()
            || entity.prop("parentattachment").is_some()
            || entity.prop("moveparent").is_some()
        {
            log::debug!(
                "bsp entity prop {classname} has parent/attachment fields; rendering at own origin"
            );
        }
        match partition.point_partition(bsp, placement.origin) {
            GeometryPartition::Visible => visible.push(placement),
            GeometryPartition::Skybox => skybox.push(placement),
        }
    }
    (visible, skybox)
}

fn entity_prop_placement(
    entity: &MapEntity,
    entity_index: usize,
    classname: &str,
    bsp: &MapBsp,
    cluster_count: u32,
) -> Option<StaticPropPlacement> {
    let Some(model) = entity.prop("model") else {
        log::debug!("bsp entity prop {classname} skipped: missing model");
        return None;
    };
    let Some(model_path) = normalize_entity_prop_model_path(model) else {
        log::debug!("bsp entity prop {classname} skipped: invalid model {model:?}");
        return None;
    };
    let Some(origin) = entity.prop("origin") else {
        log::debug!("bsp entity prop {classname} skipped: missing origin");
        return None;
    };
    let Some(origin) = parse_entity_vec3(origin) else {
        log::debug!("bsp entity prop {classname} skipped: invalid origin");
        return None;
    };
    let angles = entity.prop("angles").map_or(Vec3::ZERO, |value| {
        parse_entity_vec3(value).unwrap_or_else(|| {
            log::debug!("bsp entity prop {classname} angles invalid: defaulting to 0 0 0");
            Vec3::splat(0.0)
        })
    });
    let skin = entity.prop("skin").map_or(0, |value| {
        parse_entity_i32(value).unwrap_or_else(|| {
            log::debug!("bsp entity prop {classname} skin invalid: defaulting to 0");
            0
        })
    });
    let scale = entity
        .prop("modelscale")
        .map_or(1.0, |value| parse_entity_prop_model_scale(value, classname));
    let solid = entity_prop_solid(entity);

    let (lighting_origin, lighting_origin_reference) = entity_lighting_origin(entity, bsp);
    Some(StaticPropPlacement {
        entity: true,
        initially_visible: entity
            .prop("effects")
            .and_then(parse_entity_i32)
            .unwrap_or(0)
            & EFFECT_NO_DRAW
            == 0
            && !(is_dynamic_prop(classname)
                && entity
                    .prop("startdisabled")
                    .and_then(super::parse_entity_bool)
                    .unwrap_or(false)),
        animation: entity_prop_animation(entity, entity_index, classname),
        static_prop_index: None,
        lighting_origin,
        lighting_origin_reference,
        diffuse_modulation: entity_diffuse_modulation(entity),
        render_mode: entity
            .prop("rendermode")
            .and_then(parse_entity_i32)
            .unwrap_or(0),
        flags: 0,
        flags_ex: None,
        extra_flags: None,
        model_path,
        origin,
        angles,
        skin,
        scale,
        solid,
        leaf_membership: MapPropLeafMembership::SpatialBounds,
        visibility: point_visibility_bucket(bsp, origin, cluster_count).into(),
    })
}

fn entity_prop_animation(
    entity: &MapEntity,
    entity_index: usize,
    classname: &str,
) -> Option<super::MapPropAnimation> {
    if !is_dynamic_prop(classname) {
        return None;
    }
    let default_sequence = entity
        .prop("defaultanim")
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let sequence_index = entity
        .prop("sequence")
        .and_then(parse_entity_i32)
        .and_then(|index| usize::try_from(index).ok());
    let random = entity
        .prop("randomanimation")
        .and_then(parse_entity_i32)
        .filter(|value| *value != 0)
        .map(|_| super::MapPropRandomAnimation {
            minimum_interval: entity
                .prop("minanimtime")
                .and_then(super::parse_entity_float)
                .unwrap_or(5.0),
            maximum_interval: entity
                .prop("maxanimtime")
                .and_then(super::parse_entity_float)
                .unwrap_or(10.0),
        });
    if default_sequence.is_none() && sequence_index.is_none() && random.is_none() {
        return None;
    }
    Some(super::MapPropAnimation {
        entity_index,
        hammer_id: entity.prop("hammerid").and_then(|value| value.parse().ok()),
        default_sequence,
        sequence_index: sequence_index.unwrap_or(0),
        initial_cycle: entity
            .prop("cycle")
            .and_then(super::parse_entity_float)
            .unwrap_or(0.0),
        playback_rate: entity
            .prop("playbackrate")
            .and_then(super::parse_entity_float)
            .unwrap_or(1.0),
        random,
    })
}

fn entity_lighting_origin(
    entity: &MapEntity,
    bsp: &MapBsp,
) -> (Option<Vec3>, Option<MapLightingOriginReference>) {
    let Some(name) = entity
        .prop("lightingorigin")
        .filter(|name| !name.is_empty())
    else {
        return (None, None);
    };
    let mut matches = bsp
        .entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| entity.prop("targetname") == Some(name));
    let first = matches.next();
    let match_count = usize::from(first.is_some()) + matches.count();
    let origin = first.and_then(|(_, target)| target.prop("origin").and_then(parse_entity_vec3));
    let reference = MapLightingOriginReference {
        name: name.to_owned(),
        first_entity: first.map(|(index, _)| index),
        match_count,
    };
    if origin.is_none() || match_count > 1 {
        log::warn!(
            "map prop lighting anchor {name:?}: matches {match_count}, first entity {:?}, finite origin {}",
            reference.first_entity,
            origin.is_some()
        );
    }
    (origin, Some(reference))
}

fn static_prop_solid(solid: u8) -> MapPropSolid {
    match solid {
        STATIC_PROP_SOLID_PHYSICS => MapPropSolid::Physics,
        _ => MapPropSolid::None,
    }
}

pub(super) fn entity_diffuse_modulation(entity: &MapEntity) -> [u8; 4] {
    let mut color = [255_u8; 4];
    if let Some(value) = entity.prop("rendercolor") {
        let components = value
            .split_ascii_whitespace()
            .map(parse_entity_float)
            .collect::<Option<Vec<_>>>();
        if let Some(components) = components.filter(|components| matches!(components.len(), 3 | 4))
        {
            for (channel, value) in color.iter_mut().zip(components) {
                *channel = value.clamp(0.0, 255.0) as u8;
            }
        }
    }
    if let Some(alpha) = entity.prop("renderamt").and_then(parse_entity_float) {
        color[3] = alpha.clamp(0.0, 255.0) as u8;
    }
    color
}

fn entity_prop_solid(entity: &MapEntity) -> MapPropSolid {
    if entity
        .prop("startdisabled")
        .is_some_and(|value| parse_entity_bool(value).unwrap_or(false))
    {
        return MapPropSolid::None;
    }
    match entity.prop("solid").and_then(parse_entity_i32) {
        Some(6) => MapPropSolid::Physics,
        _ => MapPropSolid::None,
    }
}

pub(super) fn parse_entity_prop_model_scale(value: &str, classname: &str) -> f32 {
    let Some(scale) = parse_entity_float(value).filter(|scale| *scale > 0.0) else {
        log::debug!("bsp entity prop {classname} modelscale invalid: defaulting to 1.0");
        return 1.0;
    };
    scale
}

fn detail_sprite_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        log::warn!("invalid detail sprite scale {scale}; using one");
        1.0
    }
}

fn detail_sprite_fast_eligible(prop: &vformats::bsp::DetailProp) -> bool {
    prop.prop_type == DETAIL_PROP_TYPE_SPRITE
        && prop.light_style_count == 0
        && prop.orientation == 2
        && prop.shape_angle == 0
        && prop.shape_size == 0
        && prop.sway_amount == 0
}

pub(super) struct DetailSpritePlacements {
    pub world: Vec<MapDetailSprite>,
    pub skybox: Vec<MapDetailSprite>,
    pub invalid_leaves: u32,
}

pub(super) fn partitioned_detail_sprite_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> DetailSpritePlacements {
    let sprites = bsp.detail_sprites();
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let mut invalid_leaves = 0_u32;
    let cluster_count = bsp.cluster_count();
    for prop in bsp.detail_props_iter() {
        if prop.prop_type != DETAIL_PROP_TYPE_SPRITE {
            continue;
        }
        let Some(leaf) = bsp.leaves.get(usize::from(prop.leaf)) else {
            invalid_leaves = invalid_leaves.saturating_add(1);
            continue;
        };
        if prop.orientation > 2 {
            log::warn!("unsupported detail sprite orientation {}", prop.orientation);
            continue;
        }
        let Some(sprite) = sprites.get(usize::from(prop.model_index)) else {
            log::debug!(
                "bsp detail sprite placement skipped: sprite dict index {} missing",
                prop.model_index
            );
            continue;
        };
        let placement = MapDetailSprite {
            origin: Vec3::from(prop.origin),
            angles: Vec3::from(prop.angles),
            orientation: prop.orientation,
            fast_eligible: detail_sprite_fast_eligible(prop),
            leaf_index: prop.leaf,
            underwater_leaf: leaf.leaf_water_data >= 0,
            upper_left: sprite
                .upper_left
                .map(|value| value * detail_sprite_scale(prop.scale)),
            lower_right: sprite
                .lower_right
                .map(|value| value * detail_sprite_scale(prop.scale)),
            tex_upper_left: sprite.tex_upper_left,
            tex_lower_right: sprite.tex_lower_right,
            visibility: super::visibility::visibility_bucket(leaf.cluster, cluster_count),
        };
        match partition.cluster_partition(leaf.cluster) {
            GeometryPartition::Visible => visible.push(placement),
            GeometryPartition::Skybox => skybox.push(placement),
        }
    }
    DetailSpritePlacements {
        world: visible,
        skybox,
        invalid_leaves,
    }
}

#[cfg(test)]
#[path = "placements/tests.rs"]
mod tests;
