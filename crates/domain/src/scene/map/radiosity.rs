//! Compiled surface-light inputs, independent of render-atlas packing and walking.
use super::{
    LightmapSource, MapBounds, MapBsp, lighting::selected_lightmap_samples, spatial::BoundsTree,
};
use crate::math::Vec3;
use vformats::bsp::{ColorRgbExp, Face, Leaf, Node, Plane, TexData, TexInfo};

#[derive(Clone, Debug, PartialEq)]
pub struct MapRadiositySurfaces {
    pub source: LightmapSource,
    pub world_head_node: i32,
    pub world_face_range: std::ops::Range<usize>,
    pub planes: Vec<Plane>,
    pub nodes: Vec<Node>,
    pub leaves: Vec<Leaf>,
    pub leaf_faces: Vec<u16>,
    pub faces: Vec<Face>,
    pub face_vertices: Vec<Vec<Vec3>>,
    pub texinfos: Vec<TexInfo>,
    pub texdatas: Vec<TexData>,
    pub texture_names: Vec<String>,
    pub samples: Vec<ColorRgbExp>,
    pub displacements: Vec<MapRadiosityDisplacement>,
    triangles: Vec<RadiosityTriangle>,
    triangle_tree: BoundsTree,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapRadiosityDisplacement {
    pub original_index: usize,
    pub face: usize,
    pub steps: usize,
    /// Full-resolution column-major grid; render allowed-vertex fans are separate.
    pub positions: Vec<Vec3>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RadiosityTriangle {
    face: usize,
    displacement: usize,
    vertices: [Vec3; 3],
    luxels: [[f32; 2]; 3],
    normal: Vec3,
    bounds: MapBounds,
}

mod trace;
pub use trace::{MapRadiosityHit, MapRadiosityMaterialFlags, MapRadiosityTraceError};
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapRadiosityStyles {
    First,
    All,
}

impl MapRadiositySurfaces {
    pub(super) fn from_bsp(bsp: &MapBsp) -> Option<Self> {
        let world = bsp.models.first()?;
        let start = usize::try_from(world.first_face).ok()?;
        let end = start.checked_add(usize::try_from(world.face_count).ok()?)?;
        bsp.faces.get(start..end)?;
        let (samples, source) = selected_lightmap_samples(bsp);
        let mut displacements = Vec::new();
        let mut triangles = Vec::new();
        for (original_index, displacement) in bsp.displacements.iter().enumerate() {
            let face_index = usize::from(displacement.map_face);
            if !(start..end).contains(&face_index) {
                continue;
            }
            let face = bsp.faces.get(face_index)?;
            if !(2..=4).contains(&displacement.power) {
                return None;
            }
            let steps = 1_usize.checked_shl(u32::try_from(displacement.power).ok()?)?;
            let grid = super::mesh::displacement_grid(bsp, face, displacement, steps).ok()?;
            if grid.len() != (steps + 1).checked_mul(steps + 1)? {
                return None;
            }
            for vertices in
                super::mesh::tessellate_displacement_grid(&grid, steps, steps + 1).chunks_exact(3)
            {
                let mut positions = [
                    vertices[0].position,
                    vertices[1].position,
                    vertices[2].position,
                ];
                let mut luxels = std::array::from_fn(|i| {
                    [
                        vertices[i].row as f32 / steps as f32 * face.lightmap_size[0] as f32,
                        vertices[i].column as f32 / steps as f32 * face.lightmap_size[1] as f32,
                    ]
                });
                let mut normal = (positions[1] - positions[0]).cross(positions[2] - positions[0]);
                if normal.dot(bsp.face_plane_normal(face)) < 0.0 {
                    positions.swap(1, 2);
                    luxels.swap(1, 2);
                    normal = -normal;
                }
                let bounds = super::bounds_from_points_iter(positions.into_iter())?;
                triangles.push(RadiosityTriangle {
                    face: face_index,
                    displacement: original_index,
                    vertices: positions,
                    luxels,
                    normal: normal.normalize()?,
                    bounds,
                });
            }
            displacements.push(MapRadiosityDisplacement {
                original_index,
                face: face_index,
                steps,
                positions: grid.into_iter().map(|vertex| vertex.position).collect(),
            });
        }
        let triangle_tree = BoundsTree::new(triangles.iter().map(|triangle| triangle.bounds));
        Some(Self {
            source: source?,
            world_head_node: world.head_node,
            world_face_range: start..end,
            planes: bsp.planes.clone(),
            nodes: bsp.nodes.clone(),
            leaves: bsp.leaves.clone(),
            leaf_faces: bsp.leaf_faces.clone(),
            faces: bsp.faces.clone(),
            face_vertices: bsp
                .faces
                .iter()
                .map(|face| bsp.face_vertex_positions(face))
                .collect(),
            texinfos: bsp.texinfos.clone(),
            texdatas: bsp.texdatas.clone(),
            texture_names: bsp.texdata_strings.clone(),
            samples: samples?.to_vec(),
            displacements,
            triangles,
            triangle_tree,
        })
    }

    /// Native surface-light queries truncate luxel coordinates and use the
    /// unbumped base plane of each style, before material reflectivity.
    pub fn sample_luxel(
        &self,
        face_index: usize,
        luxel: [usize; 2],
        style_values: &[f32; 128],
        styles: MapRadiosityStyles,
    ) -> Option<Vec3> {
        let face = self.faces.get(face_index)?;
        let width = usize::try_from(face.lightmap_size[0])
            .ok()?
            .checked_add(1)?;
        let height = usize::try_from(face.lightmap_size[1])
            .ok()?
            .checked_add(1)?;
        if luxel[0] >= width || luxel[1] >= height {
            return None;
        }
        let texinfo = self.texinfos.get(usize::try_from(face.texinfo).ok()?)?;
        let bases = if texinfo.flags & 0x0800 != 0 { 4 } else { 1 };
        let stride = width.checked_mul(height)?.checked_mul(bases)?;
        let offset = usize::try_from(face.light_offset).ok()?;
        if offset % 4 != 0 {
            return None;
        }
        let sample = offset
            .checked_div(4)?
            .checked_add(luxel[1].checked_mul(width)?)?
            .checked_add(luxel[0])?;
        let mut result = Vec3::splat(0.0);
        let count = match styles {
            MapRadiosityStyles::First => 1,
            MapRadiosityStyles::All => 4,
        };
        for (index, style) in face
            .styles
            .into_iter()
            .take(count)
            .take_while(|style| *style != 255)
            .enumerate()
        {
            let value = *self
                .samples
                .get(sample.checked_add(index.checked_mul(stride)?)?)?;
            let factor = style_values.get(usize::from(style)).copied().unwrap_or(1.0);
            if !factor.is_finite() || factor < 0.0 {
                return None;
            }
            let scale = 2.0_f32.powi(i32::from(value.exponent)) / 255.0;
            result += Vec3::new(
                f32::from(value.r) * scale * factor,
                f32::from(value.g) * scale * factor,
                f32::from(value.b) * scale * factor,
            );
        }
        Some(result)
    }

    /// Compiler-authored style averages precede the first luxel in reverse
    /// style order. The default engine surface query uses these records for
    /// ordinary faces; displacement queries still sample their hit luxel.
    pub fn sample_face_average(
        &self,
        face_index: usize,
        style_values: &[f32; 128],
        styles: MapRadiosityStyles,
    ) -> Option<Vec3> {
        let face = self.faces.get(face_index)?;
        let offset = usize::try_from(face.light_offset).ok()?;
        if offset % 4 != 0 {
            return None;
        }
        let first = offset / 4;
        let count = match styles {
            MapRadiosityStyles::First => 1,
            MapRadiosityStyles::All => 4,
        };
        let mut result = Vec3::ZERO;
        for (index, style) in face
            .styles
            .into_iter()
            .take(count)
            .take_while(|style| *style != 255)
            .enumerate()
        {
            let value = *self.samples.get(first.checked_sub(index + 1)?)?;
            let factor = style_values.get(usize::from(style)).copied().unwrap_or(1.0);
            if !factor.is_finite() || factor < 0.0 {
                return None;
            }
            let scale = 2.0_f32.powi(i32::from(value.exponent)) / 255.0;
            result += Vec3::new(
                f32::from(value.r) * scale * factor,
                f32::from(value.g) * scale * factor,
                f32::from(value.b) * scale * factor,
            );
        }
        Some(result)
    }
}
