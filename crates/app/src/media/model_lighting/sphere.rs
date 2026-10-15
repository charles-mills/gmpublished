use super::{ModelLightingFailure, ModelLightingSceneInputs};
use gmpublished_domain::{
    math::Vec3,
    scene::map::{
        AmbientCube, MapRadiosityMaterialFlags, MapRadiosityStyles, MapRadiositySurfaces,
    },
};

pub(super) struct Sampler<'a> {
    inputs: &'a ModelLightingSceneInputs,
    surfaces: &'a MapRadiositySurfaces,
    flags: Vec<MapRadiosityMaterialFlags>,
    sky: Vec3,
}

impl<'a> Sampler<'a> {
    pub fn new(inputs: &'a ModelLightingSceneInputs) -> Result<Self, ModelLightingFailure> {
        let surfaces = inputs
            .radiosity_surfaces
            .as_ref()
            .ok_or(ModelLightingFailure::MissingRadiositySurfaces)?;
        if inputs.world_lighting.source != Some(surfaces.source)
            || inputs.surface_materials.len() != surfaces.texdatas.len()
        {
            return Err(ModelLightingFailure::IncoherentMode);
        }
        let sky = inputs
            .world_lighting
            .selected()
            .and_then(|set| {
                set.lights
                    .iter()
                    .find(|light| light.kind == vformats::bsp::WorldLightKind::SkyAmbient)
            })
            .map_or(Vec3::ZERO, |light| Vec3::from(light.intensity));
        Ok(Self {
            inputs,
            surfaces,
            flags: inputs
                .surface_materials
                .iter()
                .map(|material| {
                    material.map_or_else(MapRadiosityMaterialFlags::default, |material| {
                        material.flags
                    })
                })
                .collect(),
            sky,
        })
    }

    pub fn sample(&self, point: Vec3) -> Result<AmbientCube, ModelLightingFailure> {
        let mut colors = [Vec3::ZERO; 6];
        let mut weights = [0.0_f32; 6];
        let styles = self.inputs.style_values(0.0);
        for bits in DIRECTION_BITS {
            let direction = Vec3::from(bits.map(f32::from_bits));
            let end = point + direction * f32::from_bits(0x475e_b852);
            let hit = self
                .surfaces
                .trace_surface(point, end, &self.flags)
                .map_err(|_| ModelLightingFailure::InvalidTrace)?;
            let radiance = if let Some(hit) = hit {
                if hit.sky {
                    self.sky
                } else {
                    let index = self
                        .surfaces
                        .material_index(hit.face_index)
                        .ok_or(ModelLightingFailure::InvalidTrace)?;
                    let material = self.inputs.surface_materials[index]
                        .ok_or(ModelLightingFailure::MissingSurfaceMaterial(index))?;
                    let raw = if hit.displacement_index.is_some() {
                        self.surfaces.sample_luxel(
                            hit.face_index,
                            hit.lightmap_coords.map(|value| value as usize),
                            &styles,
                            MapRadiosityStyles::First,
                        )
                    } else {
                        self.surfaces.sample_face_average(
                            hit.face_index,
                            &styles,
                            MapRadiosityStyles::First,
                        )
                    }
                    .ok_or(ModelLightingFailure::MissingSurfaceSample(hit.face_index))?;
                    Vec3::from(std::array::from_fn(|channel| {
                        raw[channel] * material.reflectivity[channel]
                    }))
                }
            } else {
                Vec3::ZERO
            };
            for axis in 0..6 {
                let weight = direction[axis / 2] * if axis % 2 == 0 { 1.0 } else { -1.0 };
                if weight > 0.0 {
                    colors[axis] += radiance * weight;
                    weights[axis] += weight;
                }
            }
        }
        Ok(AmbientCube {
            colors: std::array::from_fn(|axis| colors[axis] * (1.0 / weights[axis])),
        })
    }
}

// Installed Source sphere order and original float bits are retained: both
// direction rounding and accumulation order affect the six-axis result.
const DIRECTION_BITS: [[u32; 3]; 162] = [
    [0xbf06964f, 0x00000000, 0x3f59c444],
    [0xbee2bef0, 0x3e7496ab, 0x3f5d3b6d],
    [0xbe9729f6, 0x00000000, 0x3f74969a],
    [0xbe9e377a, 0x3f000000, 0x3f4f1bbd],
    [0xbe265bea, 0x3e86965f, 0x3f737868],
    [0x00000000, 0x00000000, 0x3f800000],
    [0x00000000, 0x3f59c444, 0x3f06964f],
    [0xbe1729f6, 0x3f3770ef, 0x3f2e8512],
    [0x3e1729f6, 0x3f3770ef, 0x3f2e8512],
    [0x00000000, 0x3f06964f, 0x3f59c444],
    [0x3e9e377a, 0x3f000000, 0x3f4f1bbd],
    [0x3f06964f, 0x00000000, 0x3f59c444],
    [0x3e9729f6, 0x00000000, 0x3f74969a],
    [0x3ee2bef0, 0x3e7496ab, 0x3f5d3b6d],
    [0x3e265bea, 0x3e86965f, 0x3f737868],
    [0xbf2e8512, 0x3e1729f6, 0x3f3770ef],
    [0xbf4f1bbd, 0x3e9e377a, 0x3f000000],
    [0xbf167914, 0x3ed9c433, 0x3f302d49],
    [0xbf59c444, 0x3f06964f, 0x00000000],
    [0xbf5d3b6d, 0x3ee2bef0, 0x3e7496ab],
    [0xbf3770ef, 0x3f2e8512, 0x3e1729f6],
    [0xbf302d49, 0x3f167914, 0x3ed9c433],
    [0xbf000000, 0x3f4f1bbd, 0x3e9e377a],
    [0xbe7496ab, 0x3f5d3b6d, 0x3ee2bef0],
    [0xbed9c433, 0x3f302d49, 0x3f167914],
    [0xbf3770ef, 0x3f2e8512, 0xbe1729f6],
    [0xbf000000, 0x3f4f1bbd, 0xbe9e377a],
    [0xbf06964f, 0x3f59c444, 0x00000000],
    [0x00000000, 0x3f59c444, 0xbf06964f],
    [0xbe7496ab, 0x3f5d3b6d, 0xbee2bef0],
    [0x00000000, 0x3f74969a, 0xbe9729f6],
    [0xbe86965f, 0x3f737868, 0xbe265bea],
    [0x00000000, 0x3f800000, 0x00000000],
    [0x00000000, 0x3f74969a, 0x3e9729f6],
    [0xbe86965f, 0x3f737868, 0x3e265bea],
    [0x3e7496ab, 0x3f5d3b6d, 0x3ee2bef0],
    [0x3e86965f, 0x3f737868, 0x3e265bea],
    [0x3f000000, 0x3f4f1bbd, 0x3e9e377a],
    [0x3e7496ab, 0x3f5d3b6d, 0xbee2bef0],
    [0x3e86965f, 0x3f737868, 0xbe265bea],
    [0x3f000000, 0x3f4f1bbd, 0xbe9e377a],
    [0x3f59c444, 0x3f06964f, 0x00000000],
    [0x3f3770ef, 0x3f2e8512, 0x3e1729f6],
    [0x3f3770ef, 0x3f2e8512, 0xbe1729f6],
    [0x3f06964f, 0x3f59c444, 0x00000000],
    [0x3ed9c433, 0x3f302d49, 0x3f167914],
    [0x3f5d3b6d, 0x3ee2bef0, 0x3e7496ab],
    [0x3f302d49, 0x3f167914, 0x3ed9c433],
    [0x3f4f1bbd, 0x3e9e377a, 0x3f000000],
    [0x3f2e8512, 0x3e1729f6, 0x3f3770ef],
    [0x3f167914, 0x3ed9c433, 0x3f302d49],
    [0x3f74969a, 0x3e9729f6, 0x00000000],
    [0x3f800000, 0x00000000, 0x00000000],
    [0x3f737868, 0x3e265bea, 0x3e86965f],
    [0x3f59c444, 0xbf06964f, 0x00000000],
    [0x3f74969a, 0xbe9729f6, 0x00000000],
    [0x3f5d3b6d, 0xbee2bef0, 0x3e7496ab],
    [0x3f737868, 0xbe265bea, 0x3e86965f],
    [0x3f4f1bbd, 0xbe9e377a, 0x3f000000],
    [0x3f2e8512, 0xbe1729f6, 0x3f3770ef],
    [0x3f59c444, 0x00000000, 0x3f06964f],
    [0x3f5d3b6d, 0x3ee2bef0, 0xbe7496ab],
    [0x3f4f1bbd, 0x3e9e377a, 0xbf000000],
    [0x3f737868, 0x3e265bea, 0xbe86965f],
    [0x3f06964f, 0x00000000, 0xbf59c444],
    [0x3f2e8512, 0x3e1729f6, 0xbf3770ef],
    [0x3f2e8512, 0xbe1729f6, 0xbf3770ef],
    [0x3f59c444, 0x00000000, 0xbf06964f],
    [0x3f4f1bbd, 0xbe9e377a, 0xbf000000],
    [0x3f5d3b6d, 0xbee2bef0, 0xbe7496ab],
    [0x3f737868, 0xbe265bea, 0xbe86965f],
    [0x3e1729f6, 0x3f3770ef, 0xbf2e8512],
    [0x3e9e377a, 0x3f000000, 0xbf4f1bbd],
    [0x3ed9c433, 0x3f302d49, 0xbf167914],
    [0x3ee2bef0, 0x3e7496ab, 0xbf5d3b6d],
    [0x3f167914, 0x3ed9c433, 0xbf302d49],
    [0x3f302d49, 0x3f167914, 0xbed9c433],
    [0xbe1729f6, 0x3f3770ef, 0xbf2e8512],
    [0xbe9e377a, 0x3f000000, 0xbf4f1bbd],
    [0x00000000, 0x3f06964f, 0xbf59c444],
    [0xbf06964f, 0x00000000, 0xbf59c444],
    [0xbee2bef0, 0x3e7496ab, 0xbf5d3b6d],
    [0xbe9729f6, 0x00000000, 0xbf74969a],
    [0xbe265bea, 0x3e86965f, 0xbf737868],
    [0x00000000, 0x00000000, 0xbf800000],
    [0x3e9729f6, 0x00000000, 0xbf74969a],
    [0x3e265bea, 0x3e86965f, 0xbf737868],
    [0xbee2bef0, 0xbe7496ab, 0xbf5d3b6d],
    [0xbe9e377a, 0xbf000000, 0xbf4f1bbd],
    [0xbe265bea, 0xbe86965f, 0xbf737868],
    [0x00000000, 0xbf59c444, 0xbf06964f],
    [0xbe1729f6, 0xbf3770ef, 0xbf2e8512],
    [0x3e1729f6, 0xbf3770ef, 0xbf2e8512],
    [0x00000000, 0xbf06964f, 0xbf59c444],
    [0x3e9e377a, 0xbf000000, 0xbf4f1bbd],
    [0x3ee2bef0, 0xbe7496ab, 0xbf5d3b6d],
    [0x3e265bea, 0xbe86965f, 0xbf737868],
    [0x3e7496ab, 0xbf5d3b6d, 0xbee2bef0],
    [0x3f000000, 0xbf4f1bbd, 0xbe9e377a],
    [0x3ed9c433, 0xbf302d49, 0xbf167914],
    [0x3f3770ef, 0xbf2e8512, 0xbe1729f6],
    [0x3f302d49, 0xbf167914, 0xbed9c433],
    [0x3f167914, 0xbed9c433, 0xbf302d49],
    [0x00000000, 0xbf74969a, 0xbe9729f6],
    [0x00000000, 0xbf800000, 0x00000000],
    [0x3e86965f, 0xbf737868, 0xbe265bea],
    [0x00000000, 0xbf59c444, 0x3f06964f],
    [0x00000000, 0xbf74969a, 0x3e9729f6],
    [0x3e7496ab, 0xbf5d3b6d, 0x3ee2bef0],
    [0x3e86965f, 0xbf737868, 0x3e265bea],
    [0x3f000000, 0xbf4f1bbd, 0x3e9e377a],
    [0x3f3770ef, 0xbf2e8512, 0x3e1729f6],
    [0x3f06964f, 0xbf59c444, 0x00000000],
    [0xbe7496ab, 0xbf5d3b6d, 0xbee2bef0],
    [0xbf000000, 0xbf4f1bbd, 0xbe9e377a],
    [0xbe86965f, 0xbf737868, 0xbe265bea],
    [0xbf59c444, 0xbf06964f, 0x00000000],
    [0xbf3770ef, 0xbf2e8512, 0xbe1729f6],
    [0xbf3770ef, 0xbf2e8512, 0x3e1729f6],
    [0xbf06964f, 0xbf59c444, 0x00000000],
    [0xbf000000, 0xbf4f1bbd, 0x3e9e377a],
    [0xbe7496ab, 0xbf5d3b6d, 0x3ee2bef0],
    [0xbe86965f, 0xbf737868, 0x3e265bea],
    [0xbf5d3b6d, 0xbee2bef0, 0x3e7496ab],
    [0xbf4f1bbd, 0xbe9e377a, 0x3f000000],
    [0xbf302d49, 0xbf167914, 0x3ed9c433],
    [0xbf2e8512, 0xbe1729f6, 0x3f3770ef],
    [0xbee2bef0, 0xbe7496ab, 0x3f5d3b6d],
    [0xbf167914, 0xbed9c433, 0x3f302d49],
    [0xbe9e377a, 0xbf000000, 0x3f4f1bbd],
    [0xbe1729f6, 0xbf3770ef, 0x3f2e8512],
    [0xbed9c433, 0xbf302d49, 0x3f167914],
    [0xbe265bea, 0xbe86965f, 0x3f737868],
    [0x3ee2bef0, 0xbe7496ab, 0x3f5d3b6d],
    [0x3e265bea, 0xbe86965f, 0x3f737868],
    [0x3e9e377a, 0xbf000000, 0x3f4f1bbd],
    [0x3e1729f6, 0xbf3770ef, 0x3f2e8512],
    [0x00000000, 0xbf06964f, 0x3f59c444],
    [0x3ed9c433, 0xbf302d49, 0x3f167914],
    [0x3f167914, 0xbed9c433, 0x3f302d49],
    [0x3f302d49, 0xbf167914, 0x3ed9c433],
    [0xbf74969a, 0x3e9729f6, 0x00000000],
    [0xbf737868, 0x3e265bea, 0x3e86965f],
    [0xbf800000, 0x00000000, 0x00000000],
    [0xbf59c444, 0x00000000, 0x3f06964f],
    [0xbf74969a, 0xbe9729f6, 0x00000000],
    [0xbf737868, 0xbe265bea, 0x3e86965f],
    [0xbf5d3b6d, 0x3ee2bef0, 0xbe7496ab],
    [0xbf737868, 0x3e265bea, 0xbe86965f],
    [0xbf4f1bbd, 0x3e9e377a, 0xbf000000],
    [0xbf5d3b6d, 0xbee2bef0, 0xbe7496ab],
    [0xbf737868, 0xbe265bea, 0xbe86965f],
    [0xbf4f1bbd, 0xbe9e377a, 0xbf000000],
    [0xbf2e8512, 0x3e1729f6, 0xbf3770ef],
    [0xbf2e8512, 0xbe1729f6, 0xbf3770ef],
    [0xbf59c444, 0x00000000, 0xbf06964f],
    [0xbf302d49, 0x3f167914, 0xbed9c433],
    [0xbf167914, 0x3ed9c433, 0xbf302d49],
    [0xbed9c433, 0x3f302d49, 0xbf167914],
    [0xbed9c433, 0xbf302d49, 0xbf167914],
    [0xbf167914, 0xbed9c433, 0xbf302d49],
    [0xbf302d49, 0xbf167914, 0xbed9c433],
];
