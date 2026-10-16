//! Rendering media follow compiled BSP leaves, independently of collision
//! hulls and material batching. Separate pools can share a height or texture.
use super::{MapBsp, MapLeafLocator, normalize_material_name};
use crate::math::Vec3;

#[derive(Clone, Debug, PartialEq)]
pub struct MapWaterBody {
    pub surface_z: f32,
    pub min_z: f32,
    pub material_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapWaterVolumes {
    locator: MapLeafLocator,
    leaves: Vec<(i16, i32)>,
    pub bodies: Vec<Option<MapWaterBody>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapWaterSample<'a> {
    pub body_index: usize,
    pub body: &'a MapWaterBody,
    /// Preserve WATER/SLIME identity independently of the resolved shader.
    pub contents: i32,
}

impl MapWaterVolumes {
    #[cfg(feature = "test-support")]
    pub fn box_for_tests(min: Vec3, max: Vec3, material_name: String) -> Self {
        use super::{MapLeaf, MapNode, MapPlane};
        let planes = [
            (Vec3::new(1.0, 0.0, 0.0), max[0]),
            (Vec3::new(-1.0, 0.0, 0.0), -min[0]),
            (Vec3::new(0.0, 1.0, 0.0), max[1]),
            (Vec3::new(0.0, -1.0, 0.0), -min[1]),
            (Vec3::new(0.0, 0.0, 1.0), max[2]),
            (Vec3::new(0.0, 0.0, -1.0), -min[2]),
        ]
        .into_iter()
        .map(|(normal, dist)| MapPlane { normal, dist })
        .collect();
        let nodes = (0..6)
            .map(|i| MapNode {
                plane_index: i,
                children: [-1, if i == 5 { -2 } else { i + 1 }],
            })
            .collect();
        let leaf = MapLeaf {
            cluster: 0,
            mins: [i16::MIN; 3],
            maxs: [i16::MAX; 3],
        };
        Self {
            locator: MapLeafLocator {
                planes,
                nodes,
                leaves: vec![leaf, leaf],
            },
            leaves: vec![(-1, 0), (0, super::contents_flags::WATER)],
            bodies: vec![Some(MapWaterBody {
                surface_z: max[2],
                min_z: min[2],
                material_name,
            })],
        }
    }

    pub(super) fn from_bsp(bsp: &MapBsp) -> Option<Self> {
        if bsp.leaf_water_data.is_empty() {
            return None;
        }
        let bodies = bsp
            .leaf_water_data
            .iter()
            .map(|data| {
                let texinfo = bsp
                    .texinfos
                    .get(usize::try_from(data.surface_texinfo).ok()?)?;
                if !data.surface_z.is_finite()
                    || !data.min_z.is_finite()
                    || data.min_z > data.surface_z
                {
                    log::warn!("invalid compiled liquid height range");
                    return None;
                }
                Some(MapWaterBody {
                    surface_z: data.surface_z,
                    min_z: data.min_z,
                    material_name: normalize_material_name(bsp.texinfo_name(texinfo))?,
                })
            })
            .collect();
        Some(Self {
            locator: MapLeafLocator::from_bsp(bsp),
            leaves: bsp
                .leaves
                .iter()
                .map(|leaf| (leaf.leaf_water_data, leaf.contents))
                .collect(),
            bodies,
        })
    }

    pub fn at(&self, point: Vec3) -> Option<MapWaterSample<'_>> {
        if !point.as_array().iter().all(|v| v.is_finite()) {
            return None;
        }
        let leaf = self.locator.leaf_at(point)?;
        self.at_leaf(u32::try_from(leaf).ok()?)
    }

    pub fn at_leaf(&self, leaf: u32) -> Option<MapWaterSample<'_>> {
        let &(index, contents) = self.leaves.get(leaf as usize)?;
        let body_index = usize::try_from(index).ok()?;
        let body = self.bodies.get(body_index)?.as_ref()?;
        // Compiler leaf association is authoritative, including boundary
        // ownership. Do not select the first material or nearest surface.
        Some(MapWaterSample {
            body_index,
            body,
            contents,
        })
    }
}
