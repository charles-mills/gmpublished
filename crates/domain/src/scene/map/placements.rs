//! Render-facing BSP prop, sprite, and overlay placements.

use super::{
    BTreeSet, GeometryPartition, MapBsp, MapEntity, MapPropSolid, MapPropVisibility,
    MapVisibilityBucket, Overlay, SkyboxPartition, StaticProp, StaticPropPlacement,
    cluster_in_range, is_preview_material_visible, normalize_entity_prop_model_path,
    normalize_material_name, normalize_static_prop_model_path, parse_entity_bool,
    parse_entity_float, parse_entity_i32, parse_entity_vec3, point_visibility_bucket,
    vector_is_finite_nonzero,
};
use crate::math::Vec3;

/// `SolidType::Physics` (the vphysics collision mode) in the static prop
/// game lump's `solid` byte.
const STATIC_PROP_SOLID_PHYSICS: u8 = 6;

const DETAIL_PROP_TYPE_SPRITE: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapDetailSprite {
    pub origin: Vec3,
    pub upper_left: [f32; 2],
    pub lower_right: [f32; 2],
    pub tex_upper_left: [f32; 2],
    pub tex_lower_right: [f32; 2],
    pub visibility: MapVisibilityBucket,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapOverlay {
    pub id: i32,
    pub material_name: String,
    pub positions: [Vec3; 4],
    pub normal: Vec3,
    pub u: [f32; 2],
    pub v: [f32; 2],
    pub face_count: u16,
    pub visibility: MapVisibilityBucket,
}

pub(super) fn partitioned_static_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for prop in bsp.static_props_iter().filter_map(|prop| {
        let origin = prop.origin;
        let visibility = static_prop_leaf_visibility(bsp, prop, cluster_count);
        Some(StaticPropPlacement {
            model_path: normalize_static_prop_model_path(bsp.static_prop_model(prop))?,
            origin: Vec3::from(origin),
            angles: Vec3::from(prop.angles),
            skin: prop.skin,
            scale: 1.0,
            solid: static_prop_solid(prop.solid),
            visibility,
        })
    }) {
        match partition.point_partition(bsp, prop.origin) {
            GeometryPartition::Visible => visible.push(prop),
            GeometryPartition::Skybox => skybox.push(prop),
        }
    }
    (visible, skybox)
}

fn static_prop_leaf_visibility(
    bsp: &MapBsp,
    prop: &StaticProp,
    cluster_count: u32,
) -> MapPropVisibility {
    let model = bsp.static_prop_model(prop);
    let leaf_count = usize::from(prop.leaf_count);
    if leaf_count == 0 {
        log::debug!(
            "bsp static prop {model} at {:?} has empty leaf list: using Always",
            prop.origin
        );
        return MapPropVisibility::Always;
    }
    let Some(prop_leaves) = bsp.static_prop_leaves(prop) else {
        log::debug!(
            "bsp static prop {model} leaf list out of range first={} count={leaf_count}: using Always",
            prop.first_leaf
        );
        return MapPropVisibility::Always;
    };
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
            // `cluster_in_range` above has already established the cluster is
            // non-negative and within count, so this cannot fail. Landing on
            // cluster 0 instead would attribute the prop to the wrong cell and
            // look like a visibility bug rather than a conversion one.
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
    "prop_dynamic",
    "prop_dynamic_override",
    "prop_physics",
    "prop_physics_multiplayer",
    "prop_physics_override",
];

pub(super) fn partitioned_entity_prop_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<StaticPropPlacement>, Vec<StaticPropPlacement>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for entity in bsp.entities.iter() {
        let Some(classname) = entity.prop("classname").filter(|classname| {
            ENTITY_PROP_CLASSNAMES
                .iter()
                .any(|expected| classname.eq_ignore_ascii_case(expected))
        }) else {
            continue;
        };
        let Some(placement) = entity_prop_placement(entity, classname, bsp, cluster_count) else {
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

    Some(StaticPropPlacement {
        model_path,
        origin,
        angles,
        skin,
        scale,
        solid,
        visibility: point_visibility_bucket(bsp, origin, cluster_count).into(),
    })
}

fn static_prop_solid(solid: u8) -> MapPropSolid {
    match solid {
        STATIC_PROP_SOLID_PHYSICS => MapPropSolid::Physics,
        _ => MapPropSolid::None,
    }
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

pub(super) fn partitioned_detail_sprite_placements(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<MapDetailSprite>, Vec<MapDetailSprite>) {
    let sprites = bsp.detail_sprites();
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for prop in bsp.detail_props_iter() {
        // vformats names the kind byte `prop_type` and the dict index
        // `model_index` — the reverse of vbsp's `detail_type`/`prop_type`.
        if prop.prop_type != DETAIL_PROP_TYPE_SPRITE {
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
            upper_left: sprite.upper_left,
            lower_right: sprite.lower_right,
            tex_upper_left: sprite.tex_upper_left,
            tex_lower_right: sprite.tex_lower_right,
            visibility: point_visibility_bucket(bsp, Vec3::from(prop.origin), cluster_count),
        };
        match partition.point_partition(bsp, placement.origin) {
            GeometryPartition::Visible => visible.push(placement),
            GeometryPartition::Skybox => skybox.push(placement),
        }
    }
    (visible, skybox)
}

pub(super) fn partitioned_map_overlays(
    bsp: &MapBsp,
    partition: &SkyboxPartition,
) -> (Vec<MapOverlay>, Vec<MapOverlay>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    let cluster_count = bsp.cluster_count();
    for (overlay, mapped) in bsp.overlays.iter().filter_map(|overlay| {
        let texinfo = usize::try_from(overlay.texinfo)
            .ok()
            .and_then(|index| bsp.texinfos.get(index))?;
        let material_name = normalize_material_name(bsp.texinfo_name(texinfo))?;
        if !is_preview_material_visible(&material_name) {
            return None;
        }
        Some((
            overlay,
            MapOverlay {
                id: overlay.id,
                material_name,
                positions: overlay_quad_positions(OverlayBasis::from_overlay(overlay))?,
                normal: Vec3::from(overlay.basis_normal).normalize_or_zero(),
                u: overlay.u,
                v: overlay.v,
                face_count: overlay.face_count().try_into().unwrap_or(u16::MAX),
                visibility: point_visibility_bucket(bsp, Vec3::from(overlay.origin), cluster_count),
            },
        ))
    }) {
        match partition.point_partition(bsp, Vec3::from(overlay.origin)) {
            GeometryPartition::Visible => visible.push(mapped),
            GeometryPartition::Skybox => skybox.push(mapped),
        }
    }
    (visible, skybox)
}

/// The overlay projection basis: just the fields
/// [`overlay_quad_positions`] needs, decoupled from
/// [`vformats::bsp::Overlay`] (whose packed face-table fields are
/// private) so tests can build fixtures with a plain struct literal.
#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayBasis {
    pub(super) id: i32,
    pub(super) basis_normal: Vec3,
    pub(super) uv_points: [Vec3; 4],
    pub(super) origin: Vec3,
}

impl OverlayBasis {
    pub(super) fn from_overlay(overlay: &Overlay) -> Self {
        Self {
            id: overlay.id,
            basis_normal: Vec3::from(overlay.basis_normal),
            uv_points: overlay.uv_points.map(Vec3::from),
            origin: Vec3::from(overlay.origin),
        }
    }
}

pub(super) fn overlay_quad_positions(overlay: OverlayBasis) -> Option<[Vec3; 4]> {
    let normal = overlay.basis_normal.normalize_or_zero();
    if !vector_is_finite_nonzero(normal) {
        log::debug!("bsp overlay {} skipped: invalid basis normal", overlay.id);
        return None;
    }
    // vbsp packs the overlay's real U basis into the z components of the
    // first three UV points and flags a flipped V basis via
    // uv_points[3].z == 1.0 (source-sdk-2013 utils/vbsp/overlay.cpp:
    // vecUVPoints[i].z = vecBasis[0][i]; [3].z = 1.0 when
    // normal.cross(basisU) . basisV < 0). The xy pairs are corner
    // coordinates in that basis — z is NOT a normal offset.
    let uv_points = overlay.uv_points;
    let u_axis = Vec3::new(uv_points[0][2], uv_points[1][2], uv_points[2][2]).normalize_or_zero();
    if !vector_is_finite_nonzero(u_axis) {
        log::debug!(
            "bsp overlay {} skipped: degenerate packed U basis",
            overlay.id
        );
        return None;
    }
    let mut v_axis = normal.cross(u_axis).normalize_or_zero();
    if uv_points[3][2] == 1.0 {
        v_axis *= -1.0;
    }
    let origin = overlay.origin;
    // Preview simplification: render one quad from the overlay plane points
    // and do not clip it back to the referenced faces.
    Some(uv_points.map(|point| origin + ((u_axis * point[0]) + (v_axis * point[1]))))
}
