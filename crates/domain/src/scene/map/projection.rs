use super::brushes::BrushTransform;
use super::{
    GeometryPartition, MapDoor, MapDoorGeometry, MapDoorMotion, MapMesh, MapMeshIndexRange,
    MapPropVisibility, MapSurfaceOwner, MapVertex,
};
use crate::math::Vec3;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSurfaceVertex {
    pub surface: MapVertex,
    pub uv: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapSurfaceReceiver {
    pub receiver_mesh: super::MapReceiverMesh,
    pub range: MapMeshIndexRange,
    pub material_index: usize,
    pub bounds: super::MapBounds,
    pub plane_normal: Vec3,
    pub plane_distance: f32,
    pub visibility: MapPropVisibility,
    pub displacement: bool,
    pub owner: MapSurfaceOwner,
}

pub(super) fn append_surface_polygon(
    vertices: &mut Vec<MapSurfaceVertex>,
    indices: &mut Vec<u32>,
    polygon: Vec<MapSurfaceVertex>,
) -> Result<(), super::BspError> {
    if polygon.len() < 3 {
        return Ok(());
    }
    let base = u32::try_from(vertices.len()).map_err(|_| super::BspError::TooLarge {
        item: "surface mark vertices",
    })?;
    let count = u32::try_from(polygon.len()).map_err(|_| super::BspError::TooLarge {
        item: "surface mark vertices",
    })?;
    base.checked_add(count).ok_or(super::BspError::TooLarge {
        item: "surface mark vertices",
    })?;
    for corner in 1..polygon.len() - 1 {
        let a = polygon[corner].surface.position - polygon[0].surface.position;
        let b = polygon[corner + 1].surface.position - polygon[0].surface.position;
        if a.cross(b).length_squared() <= 1e-10 {
            continue;
        }
        let corner = corner as u32;
        indices.extend_from_slice(&[base, base + corner, base + corner + 1]);
    }
    vertices.extend(polygon);
    Ok(())
}

pub(super) struct ReceiverSurface<'a> {
    pub(super) mesh: &'a MapMesh,
    pub(super) receiver_mesh: super::MapReceiverMesh,
    pub(super) range: MapMeshIndexRange,
    pub(super) partition: GeometryPartition,
    pub(super) visibility: MapPropVisibility,
    pub(super) owner: MapSurfaceOwner,
    pub(super) solid: bool,
    pub(super) allows_decals: bool,
    pub(super) plane_normal: Vec3,
    pub(super) plane_distance: f32,
    pub(super) displacement: bool,
    pub(super) bounds: super::MapBounds,
    transform: BrushTransform,
}

impl ReceiverSurface<'_> {
    fn polygon(&self) -> Option<Vec<MapVertex>> {
        let start = self.range.start as usize;
        let end = start.checked_add(self.range.count as usize)?;
        let mut triangles = self.mesh.indices.get(start..end)?.chunks_exact(3);
        let first = triangles.next()?;
        let mut indices = first.to_vec();
        for triangle in triangles {
            if triangle[0] != first[0] || Some(&triangle[1]) != indices.last() {
                return None;
            }
            indices.push(triangle[2]);
        }
        indices
            .into_iter()
            .map(|index| {
                Some(transform_surface(
                    *self.mesh.vertices.get(index as usize)?,
                    self.transform,
                ))
            })
            .collect()
    }

    pub(super) fn triangles(&self) -> impl Iterator<Item = [MapVertex; 3]> + '_ {
        self.local_triangles()
            .map(|triangle| triangle.map(|vertex| transform_surface(vertex, self.transform)))
    }

    pub(super) fn local_triangles(&self) -> impl Iterator<Item = [MapVertex; 3]> + '_ {
        let start = self.range.start as usize;
        let end = start.saturating_add(self.range.count as usize);
        self.mesh
            .indices
            .get(start..end)
            .unwrap_or_default()
            .chunks_exact(3)
            .filter_map(|indices| {
                Some([
                    *self.mesh.vertices.get(indices[0] as usize)?,
                    *self.mesh.vertices.get(indices[1] as usize)?,
                    *self.mesh.vertices.get(indices[2] as usize)?,
                ])
            })
    }

    pub(super) fn owner_transform(&self) -> BrushTransform {
        match self.owner {
            MapSurfaceOwner::World => BrushTransform::default(),
            MapSurfaceOwner::Brush { origin, angles, .. } => BrushTransform { origin, angles },
            MapSurfaceOwner::Door { .. } => self.transform,
        }
    }

    pub(super) fn surface_to_local(&self, vertex: MapVertex) -> MapVertex {
        if matches!(self.owner, MapSurfaceOwner::Door { .. }) {
            inverse_transform_surface(vertex, self.transform)
        } else {
            vertex
        }
    }

    pub(super) fn point_to_local(&self, point: Vec3) -> Vec3 {
        if matches!(self.owner, MapSurfaceOwner::Door { .. }) {
            inverse_transform_vector(point - self.transform.origin, self.transform.angles)
        } else {
            point
        }
    }
}

pub(super) fn receiver_surfaces<'a>(
    world: &'a [MapMesh],
    sky: &'a [MapMesh],
    doors: &'a [MapDoor],
) -> BTreeMap<u32, Vec<ReceiverSurface<'a>>> {
    let mut receivers = BTreeMap::<u32, Vec<ReceiverSurface<'a>>>::new();
    for (meshes, partition, door) in [
        (world, GeometryPartition::Visible, None),
        (sky, GeometryPartition::Skybox, None),
    ]
    .into_iter()
    .chain(doors.iter().enumerate().filter_map(|(door_index, door)| {
        let MapDoorGeometry::Brush { meshes, .. } = &door.geometry else {
            return None;
        };
        Some((
            meshes.as_slice(),
            GeometryPartition::Visible,
            Some((door_index, door)),
        ))
    })) {
        for (mesh_index, mesh) in meshes.iter().enumerate() {
            let ownership = mesh
                .surface_owners
                .iter()
                .map(|surface| (surface.index_start, surface))
                .collect::<BTreeMap<_, _>>();
            let mut ranges = BTreeMap::<u32, (MapMeshIndexRange, bool, Vec<u32>)>::new();
            for range in &mesh.visibility.always_visible {
                ranges
                    .entry(range.start)
                    .or_insert((*range, true, Vec::new()))
                    .1 = true;
            }
            for cluster in &mesh.visibility.clusters {
                for range in &cluster.ranges {
                    let clusters = &mut ranges
                        .entry(range.start)
                        .or_insert((*range, false, Vec::new()))
                        .2;
                    if clusters.last() != Some(&cluster.cluster) {
                        clusters.push(cluster.cluster);
                    }
                }
            }
            for (range, always_visible, mut clusters) in ranges.into_values() {
                let metadata = ownership.get(&range.start).copied();
                let (owner, transform) = if let Some((door_index, door)) = door {
                    (
                        MapSurfaceOwner::Door { door_index },
                        door_initial_transform(door),
                    )
                } else {
                    (
                        metadata.map_or(MapSurfaceOwner::World, |surface| surface.owner),
                        BrushTransform::default(),
                    )
                };
                let visibility = if always_visible {
                    MapPropVisibility::Always
                } else {
                    clusters.sort_unstable();
                    clusters.dedup();
                    MapPropVisibility::Clusters(clusters)
                };
                let start = range.start as usize;
                let end = start.saturating_add(range.count as usize);
                let bounds = super::bounds_from_points_iter(
                    mesh.indices
                        .get(start..end)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|index| mesh.vertices.get(*index as usize))
                        .map(|vertex| transform.point(vertex.position)),
                )
                .unwrap_or(super::MapBounds {
                    min: Vec3::ZERO,
                    max: Vec3::ZERO,
                });
                receivers
                    .entry(range.face)
                    .or_default()
                    .push(ReceiverSurface {
                        mesh,
                        receiver_mesh: door.map_or_else(
                            || match partition {
                                GeometryPartition::Visible => {
                                    super::MapReceiverMesh::World(mesh_index)
                                }
                                GeometryPartition::Skybox => {
                                    super::MapReceiverMesh::Skybox(mesh_index)
                                }
                            },
                            |(door_index, _)| super::MapReceiverMesh::Door {
                                door_index,
                                mesh_index,
                            },
                        ),
                        range,
                        partition,
                        visibility,
                        owner,
                        solid: metadata.is_none_or(|surface| surface.solid),
                        allows_decals: metadata.is_none_or(|surface| surface.allows_decals),
                        plane_normal: metadata.map_or_else(
                            || {
                                mesh.vertices
                                    .first()
                                    .map_or(Vec3::ZERO, |vertex| vertex.normal)
                            },
                            |surface| transform.vector(surface.plane_normal),
                        ),
                        plane_distance: metadata.map_or_else(
                            || {
                                mesh.vertices
                                    .first()
                                    .map_or(0.0, |vertex| vertex.normal.dot(vertex.position))
                            },
                            |surface| {
                                surface.plane_distance
                                    + transform.vector(surface.plane_normal).dot(transform.origin)
                            },
                        ),
                        displacement: metadata.is_some_and(|surface| surface.displacement),
                        bounds,
                        transform,
                    });
            }
        }
    }
    receivers
}

pub(super) fn door_initial_transform(door: &MapDoor) -> BrushTransform {
    let progress = door.initial_progress.clamp(0.0, 1.0);
    match door.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            ..
        } => BrushTransform {
            origin: door.origin + direction * (distance * progress),
            angles: door.angles,
        },
        MapDoorMotion::Rotating { angle_delta, .. } => BrushTransform {
            origin: door.origin,
            angles: door.angles + angle_delta * progress,
        },
    }
}

fn transform_surface(mut vertex: MapVertex, transform: BrushTransform) -> MapVertex {
    vertex.position = transform.point(vertex.position);
    vertex.normal = transform.vector(vertex.normal);
    vertex.tangent_s = transform.vector(vertex.tangent_s);
    vertex.tangent_t = transform.vector(vertex.tangent_t);
    vertex
}

pub(super) fn inverse_transform_vector(vector: Vec3, angles: Vec3) -> Vec3 {
    Vec3::new(
        vector.dot(Vec3::new(1.0, 0.0, 0.0).rotate_source(angles)),
        vector.dot(Vec3::new(0.0, 1.0, 0.0).rotate_source(angles)),
        vector.dot(Vec3::new(0.0, 0.0, 1.0).rotate_source(angles)),
    )
}

fn inverse_transform_surface(mut vertex: MapVertex, transform: BrushTransform) -> MapVertex {
    vertex.position =
        inverse_transform_vector(vertex.position - transform.origin, transform.angles);
    vertex.normal = inverse_transform_vector(vertex.normal, transform.angles);
    vertex.tangent_s = inverse_transform_vector(vertex.tangent_s, transform.angles);
    vertex.tangent_t = inverse_transform_vector(vertex.tangent_t, transform.angles);
    vertex
}

#[derive(Clone, Copy)]
pub(super) struct SurfaceProjection {
    pub(super) origin: Vec3,
    pub(super) u_axis: Vec3,
    pub(super) v_axis: Vec3,
    pub(super) corners: [[f32; 2]; 4],
    pub(super) u: [f32; 2],
    pub(super) v: [f32; 2],
}

#[derive(Clone, Copy)]
struct ClipVertex {
    surface: MapVertex,
    point: [f32; 2],
    uv: [f32; 2],
}

impl SurfaceProjection {
    pub(super) fn is_valid(&self) -> bool {
        if !self.origin.is_finite()
            || !self.u_axis.is_finite()
            || !self.v_axis.is_finite()
            || self.u_axis.cross(self.v_axis).length_squared() <= f32::EPSILON
            || !self.corners.iter().flatten().all(|value| value.is_finite())
            || !self.u.iter().chain(&self.v).all(|value| value.is_finite())
        {
            return false;
        }
        let turns = std::array::from_fn::<_, 4, _>(|i| {
            cross(
                subtract(self.corners[(i + 1) % 4], self.corners[i]),
                subtract(self.corners[(i + 2) % 4], self.corners[(i + 1) % 4]),
            )
        });
        turns.iter().all(|turn| *turn > 1e-6) || turns.iter().all(|turn| *turn < -1e-6)
    }

    pub(super) fn clip_triangle(&self, triangle: &[MapVertex; 3]) -> Vec<MapSurfaceVertex> {
        if !self.is_valid() || triangle.iter().any(|vertex| !vertex.position.is_finite()) {
            return Vec::new();
        }
        let polygon = triangle
            .iter()
            .copied()
            .map(|surface| {
                let delta = surface.position - self.origin;
                ClipVertex {
                    surface,
                    point: [delta.dot(self.u_axis), delta.dot(self.v_axis)],
                    uv: [0.0; 2],
                }
            })
            .collect::<Vec<_>>();
        let polygon = clip_convex_polygon(polygon, &self.corners);
        if polygon.len() < 3 {
            return Vec::new();
        }
        polygon
            .into_iter()
            .filter_map(|vertex| {
                let [u, v] = self.inverse_bilinear(vertex.point)?;
                Some(MapSurfaceVertex {
                    surface: vertex.surface,
                    uv: [
                        self.u[0] + (self.u[1] - self.u[0]) * u,
                        self.v[0] + (self.v[1] - self.v[0]) * v,
                    ],
                })
            })
            .collect()
    }

    pub(super) fn clip_quad_to_receiver(
        &self,
        receiver: &ReceiverSurface<'_>,
    ) -> Vec<MapSurfaceVertex> {
        let Some(boundary) = receiver.polygon() else {
            return Vec::new();
        };
        let corners = boundary
            .iter()
            .map(|vertex| self.project_point(vertex.position))
            .collect::<Vec<_>>();
        let Some(triangle) = receiver.triangles().find(|triangle| {
            let a = self.project_point(triangle[0].position);
            let b = self.project_point(triangle[1].position);
            let c = self.project_point(triangle[2].position);
            cross(subtract(b, a), subtract(c, a)).abs() > 1e-12
        }) else {
            return Vec::new();
        };
        let uvs = [
            [self.u[0], self.v[0]],
            [self.u[0], self.v[1]],
            [self.u[1], self.v[1]],
            [self.u[1], self.v[0]],
        ];
        let mut polygon = Vec::with_capacity(4);
        for (point, uv) in self.corners.into_iter().zip(uvs) {
            let Some(surface) = self.surface_at_point(&triangle, point) else {
                return Vec::new();
            };
            polygon.push(ClipVertex { surface, point, uv });
        }
        clip_convex_polygon(polygon, &corners)
            .into_iter()
            .map(|vertex| MapSurfaceVertex {
                surface: vertex.surface,
                uv: vertex.uv,
            })
            .collect()
    }

    fn project_point(&self, position: Vec3) -> [f32; 2] {
        let delta = position - self.origin;
        [delta.dot(self.u_axis), delta.dot(self.v_axis)]
    }

    fn surface_at_point(&self, triangle: &[MapVertex; 3], point: [f32; 2]) -> Option<MapVertex> {
        let a = self.project_point(triangle[0].position);
        let b = subtract(self.project_point(triangle[1].position), a);
        let c = subtract(self.project_point(triangle[2].position), a);
        let delta = subtract(point, a);
        let determinant = cross(b, c);
        let weights = [cross(delta, c) / determinant, cross(b, delta) / determinant];
        weights
            .iter()
            .all(|value| value.is_finite())
            .then(|| affine_surface(triangle, weights))
    }

    fn inverse_bilinear(&self, point: [f32; 2]) -> Option<[f32; 2]> {
        let a = self.corners[0];
        let b = subtract(self.corners[3], a);
        let c = subtract(self.corners[1], a);
        let d = subtract(subtract(self.corners[2], self.corners[1]), b);
        let target = subtract(point, a);
        let mut uv = [0.5_f32; 2];
        for _ in 0..16 {
            let mapped = [
                b[0] * uv[0] + c[0] * uv[1] + d[0] * uv[0] * uv[1],
                b[1] * uv[0] + c[1] * uv[1] + d[1] * uv[0] * uv[1],
            ];
            let residual = subtract(mapped, target);
            let du = [b[0] + d[0] * uv[1], b[1] + d[1] * uv[1]];
            let dv = [c[0] + d[0] * uv[0], c[1] + d[1] * uv[0]];
            let determinant = cross(du, dv);
            if determinant.abs() <= 1e-12 {
                return None;
            }
            let step = [
                cross(residual, dv) / determinant,
                cross(du, residual) / determinant,
            ];
            uv[0] -= step[0];
            uv[1] -= step[1];
            if step[0].abs().max(step[1].abs()) <= 1e-6 {
                break;
            }
        }
        uv.iter()
            .all(|value| value.is_finite())
            .then(|| uv.map(|value| value.clamp(0.0, 1.0)))
    }
}

fn clip_convex_polygon(mut polygon: Vec<ClipVertex>, corners: &[[f32; 2]]) -> Vec<ClipVertex> {
    let orientation = corners
        .iter()
        .zip(corners.iter().cycle().skip(1))
        .map(|(a, b)| cross(*a, *b))
        .sum::<f32>()
        .signum();
    for edge in 0..corners.len() {
        let origin = corners[edge];
        let direction = subtract(corners[(edge + 1) % corners.len()], origin);
        let mut clipped = Vec::with_capacity(polygon.len() + 1);
        let Some(mut previous) = polygon.last().copied() else {
            return Vec::new();
        };
        let mut previous_distance =
            orientation * cross(direction, subtract(previous.point, origin));
        for current in polygon {
            let distance = orientation * cross(direction, subtract(current.point, origin));
            if (distance >= 0.0) != (previous_distance >= 0.0) {
                let t = previous_distance / (previous_distance - distance);
                clipped.push(ClipVertex {
                    surface: interpolate_surface(previous.surface, current.surface, t),
                    point: interpolate_pair(previous.point, current.point, t),
                    uv: interpolate_pair(previous.uv, current.uv, t),
                });
            }
            if distance >= 0.0 {
                clipped.push(current);
            }
            previous = current;
            previous_distance = distance;
        }
        polygon = clipped;
    }
    polygon
}

fn affine_surface(triangle: &[MapVertex; 3], [u, v]: [f32; 2]) -> MapVertex {
    let [a, b, c] = *triangle;
    let vector = |a: Vec3, b: Vec3, c: Vec3| a + (b - a) * u + (c - a) * v;
    let scalar = |a: f32, b: f32, c: f32| a + (b - a) * u + (c - a) * v;
    MapVertex {
        position: vector(a.position, b.position, c.position),
        normal: vector(a.normal, b.normal, c.normal),
        tangent_s: vector(a.tangent_s, b.tangent_s, c.tangent_s),
        tangent_t: vector(a.tangent_t, b.tangent_t, c.tangent_t),
        tex_s: scalar(a.tex_s, b.tex_s, c.tex_s),
        tex_t: scalar(a.tex_t, b.tex_t, c.tex_t),
        texture_size: a.texture_size,
        lightmap_uv: std::array::from_fn(|axis| {
            scalar(
                a.lightmap_uv[axis],
                b.lightmap_uv[axis],
                c.lightmap_uv[axis],
            )
        }),
        has_lightmap: a.has_lightmap && b.has_lightmap && c.has_lightmap,
        lightmap_bump_step: a.lightmap_bump_step,
        blend_alpha: scalar(a.blend_alpha, b.blend_alpha, c.blend_alpha),
        opacity: scalar(a.opacity, b.opacity, c.opacity),
    }
}

pub(super) fn interpolate_surface(a: MapVertex, b: MapVertex, t: f32) -> MapVertex {
    MapVertex {
        position: a.position.lerp(b.position, t),
        normal: a.normal.lerp(b.normal, t),
        tangent_s: a.tangent_s.lerp(b.tangent_s, t),
        tangent_t: a.tangent_t.lerp(b.tangent_t, t),
        tex_s: a.tex_s + (b.tex_s - a.tex_s) * t,
        tex_t: a.tex_t + (b.tex_t - a.tex_t) * t,
        texture_size: a.texture_size,
        lightmap_uv: interpolate_pair(a.lightmap_uv, b.lightmap_uv, t),
        has_lightmap: a.has_lightmap && b.has_lightmap,
        lightmap_bump_step: a.lightmap_bump_step,
        blend_alpha: a.blend_alpha + (b.blend_alpha - a.blend_alpha) * t,
        opacity: a.opacity + (b.opacity - a.opacity) * t,
    }
}

fn interpolate_pair(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn subtract(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

#[cfg(test)]
mod tests;
