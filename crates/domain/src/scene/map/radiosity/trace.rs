use super::MapRadiositySurfaces;
use crate::math::Vec3;
use vformats::bsp::texture_flags;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MapRadiosityMaterialFlags {
    pub no_draw: bool,
    pub translucent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapRadiosityHit {
    pub face_index: usize,
    pub fraction: f32,
    pub lightmap_coords: [f32; 2],
    pub displacement_index: Option<usize>,
    pub sky: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MapRadiosityTraceError {
    #[error("non-finite radiosity ray")]
    InvalidRay,
    #[error("radiosity material flags do not match the BSP texture data")]
    InvalidMaterials,
    #[error("invalid BSP radiosity traversal")]
    InvalidBsp,
}

enum Step {
    Node(i32, f32, f32),
    Faces(usize, f32),
}

impl MapRadiositySurfaces {
    pub fn material_index(&self, face_index: usize) -> Option<usize> {
        let face = self.faces.get(face_index)?;
        let info = self.texinfos.get(usize::try_from(face.texinfo).ok()?)?;
        let index = usize::try_from(info.texdata).ok()?;
        self.texdatas.get(index)?;
        Some(index)
    }

    pub fn material_name(&self, material_index: usize) -> Option<&str> {
        let data = self.texdatas.get(material_index)?;
        self.texture_names
            .get(usize::try_from(data.name_index).ok()?)
            .map(String::as_str)
    }

    /// Surface-light tracing retains BSP node ordering and samples original
    /// luxels. Collider epsilon and render-atlas interpolation do not apply.
    pub fn trace_surface(
        &self,
        start: Vec3,
        end: Vec3,
        materials: &[MapRadiosityMaterialFlags],
    ) -> Result<Option<MapRadiosityHit>, MapRadiosityTraceError> {
        use MapRadiosityTraceError::{InvalidBsp, InvalidMaterials, InvalidRay};
        if !start.is_finite() || !end.is_finite() {
            return Err(InvalidRay);
        }
        if materials.len() != self.texdatas.len() {
            return Err(InvalidMaterials);
        }
        let delta = end - start;
        let mut stack = vec![Step::Node(self.world_head_node, 0.0, 1.0)];
        let mut sky = None;
        let mut hit = None;
        let mut visited = 0_usize;
        let limit = self
            .nodes
            .len()
            .saturating_mul(4)
            .saturating_add(self.leaves.len())
            .saturating_add(1);
        while let Some(step) = stack.pop() {
            visited += 1;
            if visited > limit {
                return Err(InvalidBsp);
            }
            match step {
                Step::Node(index, lo, hi) if index < 0 => {
                    let leaf_index =
                        usize::try_from(-i64::from(index) - 1).map_err(|_| InvalidBsp)?;
                    let leaf = self.leaves.get(leaf_index).ok_or(InvalidBsp)?;
                    let first = usize::from(leaf.first_leaf_face);
                    let count = usize::from(leaf.leaf_face_count);
                    let faces = self
                        .leaf_faces
                        .get(first..first.checked_add(count).ok_or(InvalidBsp)?)
                        .ok_or(InvalidBsp)?;
                    let mut nearest = None;
                    for &face_index in faces {
                        let face_index = usize::from(face_index);
                        if !self.world_face_range.contains(&face_index) {
                            continue;
                        }
                        let face = self.faces.get(face_index).ok_or(InvalidBsp)?;
                        let info = self
                            .texinfos
                            .get(usize::try_from(face.texinfo).map_err(|_| InvalidBsp)?)
                            .ok_or(InvalidBsp)?;
                        let material = materials
                            .get(self.material_index(face_index).ok_or(InvalidBsp)?)
                            .ok_or(InvalidMaterials)?;
                        if face.on_node != 0
                            || face.displacement >= 0
                            || material.no_draw
                            || material.translucent
                            || info.flags
                                & (texture_flags::NODRAW
                                    | texture_flags::NOLIGHT
                                    | texture_flags::TRANS)
                                != 0
                        {
                            continue;
                        }
                        let plane = self.planes.get(usize::from(face.plane)).ok_or(InvalidBsp)?;
                        // This engine query culls against the stored plane,
                        // independently of the rendered face's side flag.
                        let normal = Vec3::from(plane.normal);
                        let direction = delta.dot(normal);
                        if direction > 0.0 {
                            continue;
                        }
                        let distance = ((start[0] * normal[0] - plane.dist) + start[1] * normal[1])
                            + start[2] * normal[2];
                        let a = distance + lo * direction;
                        let b = distance + hi * direction;
                        if (a < 0.0) == (b < 0.0) {
                            continue;
                        }
                        let relative = a / (a - b);
                        let fraction = (1.0 - relative) * lo + relative * hi;
                        // Native leaf traversal compares its relative crossing
                        // parameter with the stored full-ray hit fraction.
                        if nearest
                            .as_ref()
                            .is_some_and(|old: &MapRadiosityHit| relative >= old.fraction)
                        {
                            continue;
                        }
                        if let Some(found) =
                            self.face_hit(face_index, start + delta * fraction, fraction)?
                        {
                            nearest = Some(found);
                        }
                    }
                    if nearest.is_some() {
                        hit = nearest;
                        break;
                    }
                }
                Step::Node(index, lo, hi) => {
                    let node_index = usize::try_from(index).map_err(|_| InvalidBsp)?;
                    let node = self.nodes.get(node_index).ok_or(InvalidBsp)?;
                    let plane = self
                        .planes
                        .get(usize::try_from(node.plane).map_err(|_| InvalidBsp)?)
                        .ok_or(InvalidBsp)?;
                    let normal = Vec3::from(plane.normal);
                    let distance = start.dot(normal) - plane.dist;
                    let direction = delta.dot(normal);
                    let a = distance + lo * direction;
                    let b = distance + hi * direction;
                    let front = usize::from(a < 0.0);
                    if front == usize::from(b < 0.0) {
                        stack.push(Step::Node(node.children[front], lo, hi));
                    } else {
                        let relative = a / (a - b);
                        let fraction = (1.0 - relative) * lo + relative * hi;
                        stack.push(Step::Node(node.children[front ^ 1], fraction, hi));
                        stack.push(Step::Faces(node_index, fraction));
                        stack.push(Step::Node(node.children[front], lo, fraction));
                    }
                }
                Step::Faces(node_index, fraction) => {
                    let node = self.nodes.get(node_index).ok_or(InvalidBsp)?;
                    let first = usize::from(node.first_face);
                    let end = first
                        .checked_add(usize::from(node.face_count))
                        .ok_or(InvalidBsp)?;
                    for face_index in first..end {
                        if !self.world_face_range.contains(&face_index) {
                            continue;
                        }
                        let face = self.faces.get(face_index).ok_or(InvalidBsp)?;
                        let info = self
                            .texinfos
                            .get(usize::try_from(face.texinfo).map_err(|_| InvalidBsp)?)
                            .ok_or(InvalidBsp)?;
                        if info.flags & (texture_flags::SKY | texture_flags::SKY2D) != 0 {
                            sky = Some(MapRadiosityHit {
                                face_index,
                                fraction,
                                lightmap_coords: [0.0; 2],
                                displacement_index: None,
                                sky: true,
                            });
                            continue;
                        }
                        let material = materials
                            .get(self.material_index(face_index).ok_or(InvalidBsp)?)
                            .ok_or(InvalidMaterials)?;
                        if face.displacement >= 0
                            || info.flags & texture_flags::NODRAW != 0
                            || material.no_draw
                        {
                            continue;
                        }
                        if let Some(found) =
                            self.face_hit(face_index, start + delta * fraction, fraction)?
                        {
                            hit = Some(found);
                            break;
                        }
                    }
                    if hit.is_some() {
                        break;
                    }
                }
            }
        }
        let mut closest = hit.map_or(1.0, |hit| hit.fraction);
        for index in self.triangle_tree.query(start, start + delta * closest) {
            let triangle = &self.triangles[index];
            let material = materials
                .get(self.material_index(triangle.face).ok_or(InvalidBsp)?)
                .ok_or(InvalidMaterials)?;
            if material.no_draw || delta.dot(triangle.normal) >= 0.0 {
                continue;
            }
            let [a, b, c] = triangle.vertices;
            let e1 = b - a;
            let e2 = c - a;
            let p = delta.cross(e2);
            let determinant = e1.dot(p);
            if determinant.abs() < 1.0e-6 {
                continue;
            }
            let inverse = 1.0 / determinant;
            let relative = start - a;
            let u = relative.dot(p) * inverse;
            let q = relative.cross(e1);
            let v = delta.dot(q) * inverse;
            let fraction = e2.dot(q) * inverse;
            if !(0.0..=1.0).contains(&u)
                || v < 0.0
                || u + v > 1.0
                || !(0.0..closest).contains(&fraction)
            {
                continue;
            }
            let coords = std::array::from_fn(|axis| {
                triangle.luxels[0][axis] * (1.0 - u - v)
                    + triangle.luxels[1][axis] * u
                    + triangle.luxels[2][axis] * v
            });
            closest = fraction;
            hit = Some(MapRadiosityHit {
                face_index: triangle.face,
                fraction,
                lightmap_coords: coords,
                displacement_index: Some(triangle.displacement),
                sky: false,
            });
        }
        Ok(hit.or(sky))
    }

    pub(super) fn face_hit(
        &self,
        face_index: usize,
        point: Vec3,
        fraction: f32,
    ) -> Result<Option<MapRadiosityHit>, MapRadiosityTraceError> {
        use MapRadiosityTraceError::InvalidBsp;
        let face = self.faces.get(face_index).ok_or(InvalidBsp)?;
        let info = self
            .texinfos
            .get(usize::try_from(face.texinfo).map_err(|_| InvalidBsp)?)
            .ok_or(InvalidBsp)?;
        let coords = info
            .lightmap_vecs
            .map(|axis| super::super::mesh::texture_coord(point, axis));
        let minimum = face.lightmap_mins.map(|value| value as f32);
        let relative = std::array::from_fn::<_, 2, _>(|axis| coords[axis] - minimum[axis]);
        let maximum = if face.lightmap_size == [0; 2] {
            // Zero-extent surfaces use projected compiled vertex bounds. The
            // engine keeps the stored minima as the initial relative maxima.
            let vertices = self.face_vertices.get(face_index).ok_or(InvalidBsp)?;
            vertices.iter().fold(minimum, |mut maximum, &vertex| {
                for axis in 0..2 {
                    maximum[axis] = maximum[axis].max(
                        super::super::mesh::texture_coord(vertex, info.lightmap_vecs[axis])
                            - minimum[axis],
                    );
                }
                maximum
            })
        } else {
            face.lightmap_size.map(|value| value as f32)
        };
        if (0..2).any(|axis| coords[axis] < minimum[axis] || relative[axis] > maximum[axis]) {
            return Ok(None);
        }
        Ok(Some(MapRadiosityHit {
            face_index,
            fraction,
            lightmap_coords: relative,
            displacement_index: None,
            sky: false,
        }))
    }
}
