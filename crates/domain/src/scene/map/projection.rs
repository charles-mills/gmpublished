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
    pub visibility: MapPropVisibility,
    pub owner: MapSurfaceOwner,
    /// Same coordinate space as the receiver vertices.
    pub plane: MapSurfacePlane,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSurfacePlane {
    pub normal: Vec3,
    pub distance: f32,
}

impl MapSurfacePlane {
    pub fn signed_distance(self, point: Vec3) -> f32 {
        (point[0] * self.normal[0] + point[1] * self.normal[1]) + point[2] * self.normal[2]
            - self.distance
    }
}

impl super::MapSurfaceTextureProjection {
    pub(super) fn from_points(vectors: [[f32; 4]; 2], points: &[Vec3]) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let mut minimum = [0; 2];
        let mut extent = [0; 2];
        for axis in 0..2 {
            let mut low = SOURCE_TEXTURE_MINIMUM_SEED;
            let mut high = SOURCE_TEXTURE_MAXIMUM_SEED;
            for &point in points {
                let coordinate = compiled_texture_coordinate(point, vectors[axis]);
                if !coordinate.is_finite() {
                    return None;
                }
                low = low.min(coordinate);
                high = high.max(coordinate);
            }
            minimum[axis] = low as i32;
            extent[axis] = (high.ceil() as i32).wrapping_sub(minimum[axis]) as i16;
        }
        Some(Self {
            vectors,
            minimum,
            extent,
        })
    }

    pub(super) fn admits_decal(&self, origin: Vec3, axes: [Vec3; 2], size: [f32; 2]) -> bool {
        (0..2).all(|axis| {
            let vector = Vec3::new(
                self.vectors[axis][0],
                self.vectors[axis][1],
                self.vectors[axis][2],
            );
            let span =
                (axes[0].dot(vector) * size[0]).abs() + (axes[1].dot(vector) * size[1]).abs();
            let coordinate =
                compiled_texture_coordinate(origin, self.vectors[axis]) - self.minimum[axis] as f32;
            let start = (f64::from(coordinate) - f64::from(span) * 0.5) as f32;
            start > -span && start <= f32::from(self.extent[axis]) + span
        })
    }
}

fn compiled_texture_coordinate(point: Vec3, vector: [f32; 4]) -> f32 {
    (point[0] * vector[0] + point[1] * vector[1] + vector[3]) + point[2] * vector[2]
}

const SOURCE_TEXTURE_MINIMUM_SEED: f32 = 999_999.0;
const SOURCE_TEXTURE_MAXIMUM_SEED: f32 = -99_999.0;

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
    source: Option<&'a super::MapMeshSurfaceOwner>,
    pub(super) receiver_mesh: super::MapReceiverMesh,
    pub(super) range: MapMeshIndexRange,
    pub(super) partition: GeometryPartition,
    pub(super) visibility: MapPropVisibility,
    pub(super) owner: MapSurfaceOwner,
    pub(super) solid: bool,
    pub(super) allows_decals: bool,
    pub(super) plane_normal: Vec3,
    pub(super) plane_distance: f32,
    pub(super) displacement: Option<super::MapDisplacementProjection>,
    pub(super) texture_projection: Option<super::MapSurfaceTextureProjection>,
    pub(super) bounds: super::MapBounds,
    transform: BrushTransform,
}

impl ReceiverSurface<'_> {
    pub(super) fn render_plane(&self) -> MapSurfacePlane {
        if matches!(self.owner, MapSurfaceOwner::Door { .. }) {
            if let Some(source) = self.source {
                return MapSurfacePlane {
                    normal: source.plane_normal,
                    distance: source.plane_distance,
                };
            }
            return MapSurfacePlane {
                normal: inverse_transform_vector(self.plane_normal, self.transform.angles),
                distance: self.plane_distance - self.plane_normal.dot(self.transform.origin),
            };
        }
        MapSurfacePlane {
            normal: self.plane_normal,
            distance: self.plane_distance,
        }
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
        let MapDoorGeometry::Brush {
            model_index,
            meshes,
        } = &door.geometry
        else {
            return None;
        };
        Some((
            meshes.as_slice(),
            GeometryPartition::Visible,
            Some((door_index, *model_index, door)),
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
                let (owner, transform) = if let Some((door_index, model_index, door)) = door {
                    (
                        MapSurfaceOwner::Door {
                            door_index,
                            model_index,
                        },
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
                        source: metadata,
                        receiver_mesh: door.map_or_else(
                            || match partition {
                                GeometryPartition::Visible => {
                                    super::MapReceiverMesh::World(mesh_index)
                                }
                                GeometryPartition::Skybox => {
                                    super::MapReceiverMesh::Skybox(mesh_index)
                                }
                            },
                            |(door_index, _, _)| super::MapReceiverMesh::Door {
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
                        displacement: metadata
                            .and_then(|surface| surface.displacement.clone())
                            .map(|mut displacement| {
                                displacement.base_corners = displacement
                                    .base_corners
                                    .map(|point| transform.point(point));
                                if transform != BrushTransform::default() {
                                    displacement.positions = displacement
                                        .positions
                                        .iter()
                                        .map(|point| transform.point(*point))
                                        .collect();
                                }
                                displacement
                            }),
                        texture_projection: metadata.and_then(|surface| surface.texture_projection),
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
    vertex.base_position = transform.point(vertex.base_position);
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
    vertex.base_position =
        inverse_transform_vector(vertex.base_position - transform.origin, transform.angles);
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
struct ClipVertex<T> {
    attributes: T,
    point: [f32; 2],
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
                    attributes: surface,
                    point: [delta.dot(self.u_axis), delta.dot(self.v_axis)],
                }
            })
            .collect::<Vec<_>>();
        let polygon = clip_convex_polygon(polygon, &self.corners, 0.0, interpolate_surface);
        self.textured_polygon(polygon)
    }

    pub(super) fn clip_quad_to_triangle(&self, triangle: &[MapVertex; 3]) -> Vec<MapSurfaceVertex> {
        if !self.is_valid() || triangle.iter().any(|vertex| !vertex.position.is_finite()) {
            return Vec::new();
        }
        let corners = triangle.map(|vertex| self.project_point(vertex.position));
        let mut polygon = Vec::with_capacity(4);
        for point in self.corners {
            let Some(surface) = self.surface_at_point(triangle, point) else {
                return Vec::new();
            };
            polygon.push(ClipVertex {
                attributes: surface,
                point,
            });
        }
        self.textured_polygon(clip_convex_polygon(
            polygon,
            &corners,
            COMPILED_OVERLAY_CLIP_EPSILON,
            interpolate_surface,
        ))
    }

    pub(super) fn project_displacement(
        &self,
        displacement: &super::MapDisplacementProjection,
        triangles: impl Iterator<Item = [MapVertex; 3]>,
    ) -> Vec<Vec<MapSurfaceVertex>> {
        if !self.is_valid() || displacement.steps == 0 {
            return Vec::new();
        }
        let [a, b, _, d] = displacement.base_corners;
        let Some(u_axis) = (d - a).normalize() else {
            return Vec::new();
        };
        let Some(normal) = (d - a).cross(b - a).normalize() else {
            return Vec::new();
        };
        let mut parent = Self {
            origin: a,
            u_axis,
            v_axis: normal.cross(u_axis),
            corners: [[0.0; 2]; 4],
            u: [0.0, 1.0],
            v: [0.0, 1.0],
        };
        parent.corners = displacement
            .base_corners
            .map(|point| parent.project_point(point));
        if !parent.is_valid() {
            return Vec::new();
        }
        let corners = displacement
            .base_corners
            .map(|point| self.project_point(point));
        let texture_corners = [
            [self.u[0], self.v[0]],
            [self.u[0], self.v[1]],
            [self.u[1], self.v[1]],
            [self.u[1], self.v[0]],
        ];
        let polygon = self
            .corners
            .into_iter()
            .zip(texture_corners)
            .map(|(point, attributes)| ClipVertex { attributes, point })
            .collect();
        let polygon = clip_convex_polygon(
            polygon,
            &corners,
            COMPILED_OVERLAY_CLIP_EPSILON,
            interpolate_pair,
        )
        .into_iter()
        .map(|vertex| {
            let position =
                self.origin + self.u_axis * vertex.point[0] + self.v_axis * vertex.point[1];
            Some(ClipVertex {
                point: parent.inverse_bilinear(parent.project_point(position))?,
                ..vertex
            })
        })
        .collect::<Option<Vec<_>>>();
        let Some(polygon) = polygon.filter(|polygon| polygon.len() >= 3) else {
            return Vec::new();
        };
        let mut polygons = vec![polygon];
        let scale = 1.0 / displacement.steps as f32;
        for normal in [[1.0, 0.0], [0.0, 1.0]] {
            for index in 1..displacement.steps {
                polygons = split_texture_polygons(polygons, normal, index as f32 * scale);
            }
        }
        let diagonal = SOURCE_DISPLACEMENT_DIAGONAL_COMPONENT;
        for (normal, first, end) in [
            ([diagonal, diagonal], 2, 2 * displacement.steps as i32 - 1),
            (
                [-diagonal, diagonal],
                2 - displacement.steps as i32,
                displacement.steps as i32 - 1,
            ),
        ] {
            for index in (first..end).step_by(2) {
                polygons =
                    split_texture_polygons(polygons, normal, index as f32 * (diagonal * scale));
            }
        }
        let triangles = triangles
            .filter_map(|triangle| {
                let mut corners = [[0.0; 2]; 3];
                for index in 0..3 {
                    corners[index] = parent
                        .inverse_bilinear(parent.project_point(triangle[index].base_position))?;
                }
                Some((triangle, corners))
            })
            .collect::<Vec<_>>();
        polygons
            .into_iter()
            .filter_map(|polygon| {
                polygon
                    .into_iter()
                    .map(|vertex| {
                        let point = vertex.point.map(|value| value.clamp(0.0, 1.0));
                        let mut surface = triangles.iter().find_map(|(triangle, corners)| {
                            let weights = triangle_weights(corners, point)?;
                            weights_inside(weights).then(|| affine_surface(triangle, weights))
                        })?;
                        surface.position = displacement_position(displacement, point)?;
                        Some(MapSurfaceVertex {
                            surface,
                            uv: vertex.attributes,
                        })
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .collect()
    }

    fn textured_polygon(&self, polygon: Vec<ClipVertex<MapVertex>>) -> Vec<MapSurfaceVertex> {
        if polygon.len() < 3 {
            return Vec::new();
        }
        polygon
            .into_iter()
            .map(|vertex| {
                let [u, v] = self.inverse_bilinear(vertex.point)?;
                Some(MapSurfaceVertex {
                    surface: vertex.attributes,
                    uv: [
                        self.u[0] + (self.u[1] - self.u[0]) * u,
                        self.v[0] + (self.v[1] - self.v[0]) * v,
                    ],
                })
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    fn project_point(&self, position: Vec3) -> [f32; 2] {
        let delta = position - self.origin;
        [delta.dot(self.u_axis), delta.dot(self.v_axis)]
    }

    fn surface_at_point(&self, triangle: &[MapVertex; 3], point: [f32; 2]) -> Option<MapVertex> {
        triangle_weights(
            &triangle.map(|vertex| self.project_point(vertex.position)),
            point,
        )
        .map(|weights| affine_surface(triangle, weights))
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

const COMPILED_OVERLAY_CLIP_EPSILON: f32 = 1e-4;
const SOURCE_DISPLACEMENT_DIAGONAL_COMPONENT: f32 = 0.707;

fn split_texture_polygons(
    polygons: Vec<Vec<ClipVertex<[f32; 2]>>>,
    normal: [f32; 2],
    distance: f32,
) -> Vec<Vec<ClipVertex<[f32; 2]>>> {
    let mut split = Vec::with_capacity(polygons.len() * 2);
    for polygon in polygons {
        let distances = polygon
            .iter()
            .map(|vertex| normal[0] * vertex.point[0] + normal[1] * vertex.point[1] - distance)
            .collect::<Vec<_>>();
        if !distances
            .iter()
            .any(|&value| value > COMPILED_OVERLAY_CLIP_EPSILON)
            || !distances
                .iter()
                .any(|&value| value < -COMPILED_OVERLAY_CLIP_EPSILON)
        {
            split.push(polygon);
            continue;
        }
        for sign in [1.0, -1.0] {
            let distances = distances
                .iter()
                .map(|value| value * sign)
                .collect::<Vec<_>>();
            let polygon = clip_half_plane(
                &polygon,
                &distances,
                COMPILED_OVERLAY_CLIP_EPSILON,
                interpolate_pair,
            );
            if polygon.len() >= 3 {
                split.push(polygon);
            }
        }
    }
    split
}

fn triangle_weights(corners: &[[f32; 2]; 3], point: [f32; 2]) -> Option<[f32; 2]> {
    let b = subtract(corners[1], corners[0]);
    let c = subtract(corners[2], corners[0]);
    let delta = subtract(point, corners[0]);
    let determinant = cross(b, c);
    let weights = [cross(delta, c) / determinant, cross(b, delta) / determinant];
    weights
        .iter()
        .all(|value| value.is_finite())
        .then_some(weights)
}

fn weights_inside([u, v]: [f32; 2]) -> bool {
    u >= -COMPILED_OVERLAY_CLIP_EPSILON
        && v >= -COMPILED_OVERLAY_CLIP_EPSILON
        && u + v <= 1.0 + COMPILED_OVERLAY_CLIP_EPSILON
}

fn displacement_position(
    displacement: &super::MapDisplacementProjection,
    point: [f32; 2],
) -> Option<Vec3> {
    let steps = displacement.steps;
    let row = ((point[0] * steps as f32) as usize).min(steps - 1);
    let column = ((point[1] * steps as f32) as usize).min(steps - 1);
    for triangle in super::mesh::displacement_cell_triangles(column, row) {
        let corners =
            triangle.map(|(column, row)| [row as f32 / steps as f32, column as f32 / steps as f32]);
        let [u, v] = triangle_weights(&corners, point)?;
        if !weights_inside([u, v]) {
            continue;
        }
        let [a, b, c] = triangle.map(|(column, row)| {
            displacement
                .positions
                .get(column * (steps + 1) + row)
                .copied()
        });
        let (a, b, c) = (a?, b?, c?);
        return Some(a + (b - a) * u + (c - a) * v);
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
enum ClipSide {
    Outside,
    On,
    Inside,
}

fn clip_convex_polygon<T: Copy>(
    mut polygon: Vec<ClipVertex<T>>,
    corners: &[[f32; 2]],
    epsilon: f32,
    interpolate: fn(T, T, f32) -> T,
) -> Vec<ClipVertex<T>> {
    let orientation = cross(
        subtract(corners[1], corners[0]),
        subtract(corners[2], corners[0]),
    )
    .signum();
    for edge in 0..corners.len() {
        if polygon.is_empty() {
            return polygon;
        }
        let origin = corners[edge];
        let direction = subtract(corners[(edge + 1) % corners.len()], origin);
        let length = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
        if length <= 1e-12 {
            continue;
        }
        let scale = if epsilon > 0.0 { length.recip() } else { 1.0 };
        let distances = polygon
            .iter()
            .map(|vertex| orientation * cross(direction, subtract(vertex.point, origin)) * scale)
            .collect::<Vec<_>>();
        polygon = clip_half_plane(&polygon, &distances, epsilon, interpolate);
    }
    polygon
}

fn clip_half_plane<T: Copy>(
    polygon: &[ClipVertex<T>],
    distances: &[f32],
    epsilon: f32,
    interpolate: fn(T, T, f32) -> T,
) -> Vec<ClipVertex<T>> {
    let sides = distances
        .iter()
        .map(|&distance| {
            if distance > epsilon {
                ClipSide::Inside
            } else if distance < -epsilon {
                ClipSide::Outside
            } else {
                ClipSide::On
            }
        })
        .collect::<Vec<_>>();
    let mut clipped = Vec::with_capacity(polygon.len() + 1);
    for index in 0..polygon.len() {
        let next = (index + 1) % polygon.len();
        let current = polygon[index];
        if sides[index] != ClipSide::Outside {
            clipped.push(current);
        }
        if sides[index] == ClipSide::On
            || sides[next] == ClipSide::On
            || sides[index] == sides[next]
        {
            continue;
        }
        let t = distances[index] / (distances[index] - distances[next]);
        clipped.push(ClipVertex {
            attributes: interpolate(current.attributes, polygon[next].attributes, t),
            point: interpolate_pair(current.point, polygon[next].point, t),
        });
    }
    clipped
}

fn affine_surface(triangle: &[MapVertex; 3], [u, v]: [f32; 2]) -> MapVertex {
    let [a, b, c] = *triangle;
    let vector = |a: Vec3, b: Vec3, c: Vec3| a + (b - a) * u + (c - a) * v;
    let scalar = |a: f32, b: f32, c: f32| a + (b - a) * u + (c - a) * v;
    MapVertex {
        position: vector(a.position, b.position, c.position),
        base_position: vector(a.base_position, b.base_position, c.base_position),
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
        lightmap_page: a
            .lightmap_page
            .filter(|page| b.lightmap_page == Some(*page) && c.lightmap_page == Some(*page)),
        lightmap_bump_step: a.lightmap_bump_step,
        blend_alpha: scalar(a.blend_alpha, b.blend_alpha, c.blend_alpha),
        opacity: scalar(a.opacity, b.opacity, c.opacity),
    }
}

pub(super) fn interpolate_surface(a: MapVertex, b: MapVertex, t: f32) -> MapVertex {
    MapVertex {
        position: a.position.lerp(b.position, t),
        base_position: a.base_position.lerp(b.base_position, t),
        normal: a.normal.lerp(b.normal, t),
        tangent_s: a.tangent_s.lerp(b.tangent_s, t),
        tangent_t: a.tangent_t.lerp(b.tangent_t, t),
        tex_s: a.tex_s + (b.tex_s - a.tex_s) * t,
        tex_t: a.tex_t + (b.tex_t - a.tex_t) * t,
        texture_size: a.texture_size,
        lightmap_uv: interpolate_pair(a.lightmap_uv, b.lightmap_uv, t),
        lightmap_page: a
            .lightmap_page
            .filter(|page| b.lightmap_page == Some(*page)),
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
