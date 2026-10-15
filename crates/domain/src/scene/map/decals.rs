use super::projection::{
    ReceiverSurface, SurfaceProjection, append_surface_polygon, inverse_transform_vector,
    receiver_surfaces,
};
use super::walk::{MapWalkBrushPlane, trace_brush_aabb, walk_brush_from_brush_planes};
use super::{
    MapBsp, MapDoor, MapMesh, MapOverlay, MapPlane, MapSurfaceOwner, Vec3, brush_indices_for_model,
    brushes, normalize_material_name, parse_entity_vec3,
};
use std::collections::HashMap;

const MASK_SOLID: i32 = 0x0200_400b;

#[derive(Clone, Debug, PartialEq)]
pub struct MapDecal {
    pub id: usize,
    pub material_name: String,
    pub origin: Vec3,
    pub owner: MapSurfaceOwner,
}

pub(super) fn map_decals(
    bsp: &MapBsp,
    world: &[MapMesh],
    sky: &[MapMesh],
    doors: &[MapDoor],
) -> Vec<MapDecal> {
    let pending = bsp
        .entities
        .iter()
        .enumerate()
        .filter_map(|(id, entity)| {
            if !entity.is_class("infodecal")
                || entity
                    .prop("targetname")
                    .is_some_and(|name| !name.is_empty())
            {
                return None;
            }
            let origin = entity
                .prop("origin")
                .and_then(parse_entity_vec3)
                .filter(|point| point.is_finite())?;
            let material_name = entity.prop("texture").and_then(normalize_material_name)?;
            Some((id, origin, material_name))
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Vec::new();
    }
    let door_models = doors
        .iter()
        .enumerate()
        .filter_map(|(index, door)| match &door.geometry {
            super::MapDoorGeometry::Brush { model_index, .. } => Some((*model_index, index)),
            super::MapDoorGeometry::Prop { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    let mut collision = Vec::new();
    for instance in brushes::brush_instances(bsp) {
        if !instance.solid {
            continue;
        }
        let owner = door_models.get(&instance.model_index).map_or_else(
            || {
                instance
                    .entity_index
                    .map_or(MapSurfaceOwner::World, |entity_index| {
                        MapSurfaceOwner::Brush {
                            entity_index,
                            model_index: instance.model_index,
                            origin: instance.transform.origin,
                            angles: instance.transform.angles,
                        }
                    })
            },
            |door_index| MapSurfaceOwner::Door {
                door_index: *door_index,
            },
        );
        let transform = match owner {
            MapSurfaceOwner::Door { door_index } => {
                super::projection::door_initial_transform(&doors[door_index])
            }
            _ => instance.transform,
        };
        for index in brush_indices_for_model(bsp, instance.model_index) {
            let Some(brush) = bsp
                .brushes
                .get(index.get())
                .filter(|brush| brush.contents & MASK_SOLID != 0)
            else {
                continue;
            };
            let Ok(start) = usize::try_from(brush.first_side) else {
                continue;
            };
            let Ok(count) = usize::try_from(brush.side_count) else {
                continue;
            };
            let Some(sides) = bsp.brush_sides.get(start..start.saturating_add(count)) else {
                continue;
            };
            let planes = sides
                .iter()
                .filter_map(|side| {
                    let plane = bsp.planes.get(usize::from(side.plane))?;
                    let normal = transform.vector(Vec3::from(plane.normal));
                    Some(MapWalkBrushPlane {
                        plane: MapPlane {
                            normal,
                            dist: plane.dist + normal.dot(transform.origin),
                        },
                        is_sky: false,
                    })
                })
                .collect::<Vec<_>>();
            if let Some(brush) = walk_brush_from_brush_planes(planes, super::contents_flags::SOLID)
            {
                collision.push((owner, brush));
            }
        }
    }
    let receivers = receiver_surfaces(world, sky, doors)
        .into_values()
        .flatten()
        .collect::<Vec<_>>();
    let collision_bounds =
        super::spatial::BoundsTree::new(collision.iter().map(|(_, brush)| brush.bounds()));
    let receiver_bounds =
        super::spatial::BoundsTree::new(receivers.iter().map(|receiver| receiver.bounds));
    pending
        .into_iter()
        .filter_map(|(id, origin, material_name)| {
            let start = origin - Vec3::new(5.0, 5.0, 5.0);
            let end = origin + Vec3::new(5.0, 5.0, 5.0);
            let mut best = collision_bounds
                .query(start, end)
                .into_iter()
                .filter_map(|index| {
                    let (owner, brush) = &collision[index];
                    let hit = trace_brush_aabb(brush, start, end, Vec3::ZERO)?;
                    Some((if hit.start_solid { 0.0 } else { hit.fraction }, *owner))
                })
                .min_by(|left, right| left.0.total_cmp(&right.0));
            for receiver_index in receiver_bounds.query(start, end) {
                let receiver = &receivers[receiver_index];
                if !receiver.solid {
                    continue;
                }
                if (0..3).any(|axis| {
                    start[axis] > receiver.bounds.max[axis] || end[axis] < receiver.bounds.min[axis]
                }) {
                    continue;
                }
                for triangle in receiver.triangles() {
                    if let Some(fraction) =
                        triangle_hit_fraction(start, end, triangle.map(|vertex| vertex.position))
                        && best.is_none_or(|previous| fraction < previous.0)
                    {
                        best = Some((fraction, receiver.owner));
                    }
                }
            }
            let owner = best.map(|(_, owner)| owner)?;
            Some(MapDecal {
                id,
                material_name,
                origin,
                owner,
            })
        })
        .collect()
}

pub struct MapSurfaceProjector<'a> {
    receivers: Vec<ReceiverSurface<'a>>,
    bounds: super::spatial::BoundsTree,
}

impl<'a> MapSurfaceProjector<'a> {
    pub fn new(world: &'a [MapMesh], sky: &'a [MapMesh], doors: &'a [MapDoor]) -> Self {
        let receivers = receiver_surfaces(world, sky, doors)
            .into_values()
            .flatten()
            .collect::<Vec<_>>();
        let bounds =
            super::spatial::BoundsTree::new(receivers.iter().map(|receiver| receiver.bounds));
        Self { receivers, bounds }
    }

    pub fn receiver_metadata(&self) -> Vec<super::MapSurfaceReceiver> {
        self.receivers.iter().map(|receiver|super::MapSurfaceReceiver {
            receiver_mesh:receiver.receiver_mesh,
            range:receiver.range,
            material_index:receiver.mesh.material_index,
            bounds:receiver.bounds,
            plane_normal:receiver.plane_normal,
            plane_distance:receiver.plane_distance,
            visibility:receiver.visibility.clone(),
            displacement:receiver.displacement,
            owner:receiver.owner,
        }).collect()
    }

    pub fn project_decal(
        &self,
        decal: &MapDecal,
        size: [f32; 2],
        receiver_allowed: &[bool],
    ) -> Result<Vec<MapOverlay>, super::BspError> {
        if !size.iter().all(|value| value.is_finite() && *value > 0.0) {
            return Ok(Vec::new());
        }

        let mut marks = Vec::new();
        let radius = (size[0] * size[0] + size[1] * size[1]).sqrt() * 0.5 + 4.0;
        let stamp_bounds = super::MapBounds {
            min: decal.origin - Vec3::splat(radius),
            max: decal.origin + Vec3::splat(radius),
        };
        for index in self.bounds.query_bounds(stamp_bounds) {
            let receiver = &self.receivers[index];
            if receiver.owner != decal.owner
                || !receiver.allows_decals
                || !receiver_allowed
                    .get(receiver.mesh.material_index)
                    .copied()
                    .unwrap_or(false)
            {
                continue;
            }
            let distance = receiver.plane_normal.dot(decal.origin) - receiver.plane_distance;
            if receiver.displacement {
                let radius = size[0].max(size[1]) * 0.5;
                if (0..3).any(|axis| {
                    decal.origin[axis] - radius > receiver.bounds.max[axis]
                        || decal.origin[axis] + radius < receiver.bounds.min[axis]
                }) {
                    continue;
                }
            } else if distance <= -4.0 || distance >= 4.0 {
                continue;
            }
            let transform = receiver.owner_transform();
            let local_normal = inverse_transform_vector(receiver.plane_normal, transform.angles);
            let Some([u_axis, v_axis]) = decal_axes(local_normal) else {
                continue;
            };
            let half = size.map(|value| value * 0.5);
            let projection = SurfaceProjection {
                origin: decal.origin,
                u_axis: transform.vector(u_axis),
                v_axis: transform.vector(v_axis),
                corners: [
                    [-half[0], -half[1]],
                    [-half[0], half[1]],
                    [half[0], half[1]],
                    [half[0], -half[1]],
                ],
                u: [0.0, 1.0],
                v: [0.0, 1.0],
            };
            let mut mapped = MapOverlay {
                source: super::MapSurfaceMarkSource::Decal {
                    entity_index: decal.id,
                },
                material_name: decal.material_name.clone(),
                render_order: 0,
                fade: None,
                fade_origin: receiver.point_to_local(decal.origin),
                receiver_face: receiver.range.face,
                receiver_mesh: receiver.receiver_mesh,
                receiver_range: receiver.range,
                receiver_material_index: receiver.mesh.material_index,
                owner: receiver.owner,
                vertices: Vec::new(),
                indices: Vec::new(),
                visibility: receiver.visibility.clone(),
            };
            for triangle in receiver.triangles() {
                let polygon = projection.clip_triangle(&triangle);
                let polygon = polygon
                    .into_iter()
                    .map(|mut vertex| {
                        vertex.surface = receiver.surface_to_local(vertex.surface);
                        vertex
                    })
                    .collect();
                append_surface_polygon(&mut mapped.vertices, &mut mapped.indices, polygon)?;
            }
            if !mapped.indices.is_empty() {
                marks.push(mapped);
            }
        }
        Ok(marks)
    }
}

fn decal_axes(normal: Vec3) -> Option<[Vec3; 2]> {
    let normal = normal.normalize()?;
    let [x, y, z]: [f32; 3] = normal.into();
    let (u, v) = if z.abs() > std::f32::consts::FRAC_1_SQRT_2 {
        (
            Vec3::new(y * y + z * z, -x * y, -x * z),
            Vec3::new(0.0, -z, y),
        )
    } else {
        (
            Vec3::new(-y, x, 0.0),
            Vec3::new(x * z, y * z, -(x * x + y * y)),
        )
    };
    Some([u.normalize()?, v.normalize()?])
}

fn triangle_hit_fraction(start: Vec3, end: Vec3, triangle: [Vec3; 3]) -> Option<f32> {
    let direction = end - start;
    let edge1 = triangle[1] - triangle[0];
    let edge2 = triangle[2] - triangle[0];
    let h = direction.cross(edge2);
    let determinant = edge1.dot(h);
    if determinant.abs() <= 1e-8 {
        return None;
    }
    let inverse = determinant.recip();
    let s = start - triangle[0];
    let u = inverse * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = inverse * direction.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let fraction = inverse * edge2.dot(q);
    (0.0..=1.0).contains(&fraction).then_some(fraction)
}

#[cfg(test)]
mod tests;
