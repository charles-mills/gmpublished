//! World-model collision for lighting rays, independent of player hulls and props.

use super::{MapBounds, MapBsp, ModelIndex, NodeChild, Plane, brush_indices_for_model};
use crate::math::Vec3;
use vformats::bsp::{Node, contents_flags};

pub const MASK_OPAQUE: i32 =
    contents_flags::SOLID | contents_flags::MOVEABLE | contents_flags::OPAQUE;
const DIST_EPSILON: f32 = 0.03125;

use super::spatial::BoundsTree;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapLightingTrace {
    pub fraction: f32,
    pub end_position: Vec3,
    pub normal: Vec3,
    pub start_solid: bool,
    pub all_solid: bool,
    pub contents: i32,
    pub surface_flags: i32,
    pub brush_index: Option<usize>,
    pub displacement_index: Option<usize>,
}

impl MapLightingTrace {
    fn clear(end: Vec3) -> Self {
        Self {
            fraction: 1.0,
            end_position: end,
            normal: Vec3::ZERO,
            start_solid: false,
            all_solid: false,
            contents: 0,
            surface_flags: 0,
            brush_index: None,
            displacement_index: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LightingSide {
    plane: Plane,
    flags: i32,
}

#[derive(Clone, Debug, PartialEq)]
struct LightingBrush {
    index: usize,
    contents: i32,
    sides: Vec<LightingSide>,
    bounds: MapBounds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LightingTriangle {
    displacement: usize,
    contents: i32,
    flags: i32,
    vertices: [Vec3; 3],
    normal: Vec3,
    bounds: MapBounds,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapLightingCollision {
    brushes: Vec<LightingBrush>,
    triangles: Vec<LightingTriangle>,
    brush_tree: BoundsTree,
    triangle_tree: BoundsTree,
    planes: Vec<Plane>,
    nodes: Vec<Node>,
    leaf_contents: Vec<i32>,
    leaf_flags: Vec<i16>,
    root: i32,
}

impl MapLightingCollision {
    pub(super) fn from_bsp(bsp: &MapBsp) -> Option<Self> {
        let model = bsp.models.first()?;
        let mut brushes = Vec::new();
        for index in brush_indices_for_model(bsp, ModelIndex::new(0)) {
            let Some(brush) = bsp.brushes.get(index.get()) else {
                continue;
            };
            let sides = (|| {
                let start = usize::try_from(brush.first_side).ok()?;
                let count = usize::try_from(brush.side_count).ok()?;
                let raw = bsp.brush_sides.get(start..start.checked_add(count)?)?;
                raw.iter()
                    .map(|side| {
                        let plane = *bsp.planes.get(usize::from(side.plane))?;
                        if !Vec3::from(plane.normal).is_finite()
                            || Vec3::from(plane.normal).length_squared() <= 0.0
                            || !plane.dist.is_finite()
                        {
                            return None;
                        }
                        let flags = usize::try_from(side.texinfo)
                            .ok()
                            .and_then(|i| bsp.texinfos.get(i))
                            .map_or(0, |info| info.flags);
                        Some(LightingSide { plane, flags })
                    })
                    .collect::<Option<Vec<_>>>()
            })();
            let Some(sides) = sides.filter(|sides| sides.len() >= 4) else {
                log::warn!(
                    "map lighting brush {} ignored: invalid side/plane data",
                    index.get()
                );
                continue;
            };
            let bounds_planes = sides
                .iter()
                .map(|side| super::MapPlane {
                    normal: Vec3::from(side.plane.normal),
                    dist: side.plane.dist,
                })
                .collect::<Vec<_>>();
            let Some(bounds) = super::walk::brush_bounds_from_planes(&bounds_planes) else {
                log::warn!("map lighting brush {} ignored: invalid bounds", index.get());
                continue;
            };
            brushes.push(LightingBrush {
                index: index.get(),
                contents: brush.contents,
                sides,
                bounds,
            });
        }
        let mut triangles = Vec::new();
        let first_face = usize::try_from(model.first_face).ok()?;
        let face_count = usize::try_from(model.face_count).ok()?;
        let world_faces = bsp
            .faces
            .get(first_face..first_face.checked_add(face_count)?)?;
        for face in world_faces {
            let Some(displacement) = bsp.face_displacement(face) else {
                continue;
            };
            if !(2..=4).contains(&displacement.power) {
                continue;
            }
            let steps = 1 << displacement.power;
            let Ok(grid) = super::mesh::displacement_grid(bsp, face, displacement, steps) else {
                continue;
            };
            let flags = bsp.face_texinfo(face).map_or(0, |info| info.flags);
            // Runtime collision uses the full checkerboard grid, independently
            // of the renderer's allowed-vertex fan and offline VRAD's diagonal.
            let full_grid = super::mesh::tessellate_displacement_grid(&grid, steps, steps + 1);
            for chunk in full_grid.chunks_exact(3) {
                let mut vertices = [chunk[0].position, chunk[1].position, chunk[2].position];
                let mut normal = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
                if normal.dot(bsp.face_plane_normal(face)) < 0.0 {
                    vertices.swap(1, 2);
                    normal = -normal;
                }
                if !vertices.into_iter().all(Vec3::is_finite) {
                    continue;
                }
                let Some(normal) = normal.normalize() else {
                    continue;
                };
                let Some(bounds) = super::bounds_from_points_iter(vertices.into_iter()) else {
                    continue;
                };
                triangles.push(LightingTriangle {
                    displacement: usize::try_from(face.displacement).ok()?,
                    contents: displacement.contents,
                    flags,
                    vertices,
                    normal,
                    bounds,
                });
            }
        }
        Some(Self {
            brush_tree: BoundsTree::new(brushes.iter().map(|brush| brush.bounds)),
            triangle_tree: BoundsTree::new(triangles.iter().map(|triangle| triangle.bounds)),
            brushes,
            triangles,
            planes: bsp.planes.clone(),
            nodes: bsp.nodes.clone(),
            leaf_contents: bsp.leaves.iter().map(|leaf| leaf.contents).collect(),
            leaf_flags: bsp.leaves.iter().map(vformats::bsp::Leaf::flags).collect(),
            root: model.head_node,
        })
    }

    pub fn brush_count(&self) -> usize {
        self.brushes.len()
    }

    pub fn displacement_triangle_count(&self) -> usize {
        self.triangles.len()
    }

    pub fn leaf_at(&self, position: Vec3) -> Option<usize> {
        if !position.is_finite() {
            return None;
        }
        let mut child = self.root;
        for _ in 0..=self.nodes.len() {
            match NodeChild::decode(child)? {
                NodeChild::Leaf(leaf) => return self.leaf_contents.get(leaf).map(|_| leaf),
                NodeChild::Node(index) => {
                    let node = self.nodes.get(index)?;
                    let plane = self.planes.get(usize::try_from(node.plane).ok()?)?;
                    let distance = plane_distance(*plane, position);
                    if !distance.is_finite() {
                        return None;
                    }
                    child = node.children[usize::from(distance < 0.0)];
                }
            }
        }
        None
    }

    pub fn leaf_contents(&self, leaf: usize) -> Option<i32> {
        self.leaf_contents.get(leaf).copied()
    }

    pub fn leaf_flags(&self, leaf: usize) -> Option<i16> {
        self.leaf_flags.get(leaf).copied()
    }

    /// Adjust only across the original point's BSP branch planes. Leaf AABBs
    /// cannot represent sloped partitions or preserve Source boundary behavior.
    pub fn snap_to_point_leaf(&self, point: Vec3, mut candidate: Vec3) -> Option<Vec3> {
        if !point.is_finite() || !candidate.is_finite() {
            return None;
        }
        let mut child = self.root;
        for _ in 0..=self.nodes.len() {
            match NodeChild::decode(child)? {
                NodeChild::Leaf(leaf) => return self.leaf_contents.get(leaf).map(|_| candidate),
                NodeChild::Node(index) => {
                    let node = self.nodes.get(index)?;
                    let plane = *self.planes.get(usize::try_from(node.plane).ok()?)?;
                    let normal = Vec3::from(plane.normal);
                    let original_distance = plane_distance(plane, point);
                    let candidate_distance = plane_distance(plane, candidate);
                    if !original_distance.is_finite() || !candidate_distance.is_finite() {
                        return None;
                    }
                    let back = original_distance < 0.0;
                    if back && candidate_distance > 0.0 {
                        candidate -= normal * (candidate_distance + 0.5);
                    } else if !back && candidate_distance < 0.0 {
                        candidate += normal * (0.5 - candidate_distance);
                    }
                    child = node.children[usize::from(back)];
                }
            }
        }
        None
    }

    /// `TRACE_WORLD_ONLY`: world-model brushes and displacements, with no
    /// static props, dynamic entities or transformed brush models.
    pub fn trace_opaque(&self, start: Vec3, end: Vec3) -> Option<MapLightingTrace> {
        if !start.is_finite() || !end.is_finite() || !(end - start).is_finite() {
            return None;
        }
        let ray_bounds = super::bounds_from_points_iter([start, end].into_iter())?;
        let mut result = MapLightingTrace::clear(end);
        for index in self.brush_tree.query(start, end) {
            let brush = &self.brushes[index];
            if brush.contents & MASK_OPAQUE == 0 || !intersects(ray_bounds, brush.bounds) {
                continue;
            }
            if let Some(hit) = trace_brush(brush, start, end) {
                merge_hit(&mut result, hit);
            }
        }
        for index in self.triangle_tree.query(start, end) {
            let triangle = &self.triangles[index];
            if triangle.contents & MASK_OPAQUE == 0 || !intersects(ray_bounds, triangle.bounds) {
                continue;
            }
            if let Some(hit) = trace_triangle(*triangle, start, end) {
                merge_hit(&mut result, hit);
            }
        }
        result.end_position = start.lerp(end, result.fraction);
        Some(result)
    }
}

fn plane_distance(plane: Plane, point: Vec3) -> f32 {
    if (0..=2).contains(&plane.axis_type) && plane.normal[plane.axis_type as usize] == 1.0 {
        point[plane.axis_type as usize] - plane.dist
    } else {
        point.dot(Vec3::from(plane.normal)) - plane.dist
    }
}

fn intersects(a: MapBounds, b: MapBounds) -> bool {
    (0..3).all(|axis| a.min[axis] <= b.max[axis] && a.max[axis] >= b.min[axis])
}

fn merge_hit(result: &mut MapLightingTrace, hit: MapLightingTrace) {
    let start_solid = result.start_solid || hit.start_solid;
    let all_solid = result.all_solid || hit.all_solid;
    if hit.fraction < result.fraction
        || hit.all_solid
        || (hit.start_solid && !result.start_solid && result.fraction == 1.0)
    {
        *result = hit;
    }
    result.start_solid = start_solid;
    result.all_solid = all_solid;
}

fn trace_brush(brush: &LightingBrush, start: Vec3, end: Vec3) -> Option<MapLightingTrace> {
    let mut enter = -1.0_f32;
    let mut leave = 1.0_f32;
    let mut side_hit = None;
    let mut start_out = false;
    let mut end_out = false;
    for side in &brush.sides {
        let a = plane_distance(side.plane, start);
        let b = plane_distance(side.plane, end);
        start_out |= a > 0.0;
        end_out |= b > 0.0;
        if a > 0.0 && b > 0.0 {
            return None;
        }
        if a <= 0.0 && b <= 0.0 {
            continue;
        }
        if a > b {
            let fraction = (a - DIST_EPSILON) / (a - b);
            if fraction > enter {
                enter = fraction;
                side_hit = Some(side);
            }
        } else {
            leave = leave.min((a + DIST_EPSILON) / (a - b));
        }
        if enter > leave {
            return None;
        }
    }
    if !start_out {
        let mut hit = MapLightingTrace::clear(end);
        hit.start_solid = true;
        hit.all_solid = !end_out;
        hit.fraction = if end_out { 1.0 } else { 0.0 };
        hit.contents = brush.contents;
        hit.brush_index = Some(brush.index);
        return Some(hit);
    }
    let side = side_hit?;
    if enter >= leave || enter <= -1.0 || enter >= 1.0 {
        return None;
    }
    Some(MapLightingTrace {
        fraction: enter.max(0.0),
        end_position: end,
        normal: Vec3::from(side.plane.normal),
        start_solid: false,
        all_solid: false,
        contents: brush.contents,
        surface_flags: side.flags,
        brush_index: Some(brush.index),
        displacement_index: None,
    })
}

fn trace_triangle(triangle: LightingTriangle, start: Vec3, end: Vec3) -> Option<MapLightingTrace> {
    let direction = end - start;
    if direction.dot(triangle.normal) >= 0.0 {
        return None;
    }
    let [a, b, c] = triangle.vertices;
    let edge1 = b - a;
    let edge2 = c - a;
    let p = direction.cross(edge2);
    let determinant = edge1.dot(p);
    if determinant.abs() < 1.0e-6 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let relative = start - a;
    let u = relative.dot(p) * inverse;
    let q = relative.cross(edge1);
    let v = direction.dot(q) * inverse;
    let fraction = edge2.dot(q) * inverse;
    if !(0.0..=1.0).contains(&u) || v < 0.0 || u + v > 1.0 || !(-0.001..=1.001).contains(&fraction)
    {
        return None;
    }
    Some(MapLightingTrace {
        fraction: fraction.clamp(0.0, 1.0),
        end_position: end,
        normal: triangle.normal,
        start_solid: false,
        all_solid: false,
        contents: triangle.contents,
        surface_flags: triangle.flags,
        brush_index: None,
        displacement_index: Some(triangle.displacement),
    })
}

#[cfg(test)]
mod tests;
