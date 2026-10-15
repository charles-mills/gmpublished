use crate::math::Vec3;

/// Authored sample position, independent of cameras and 3D skybox projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapCubemapProbe {
    /// Integer coordinates identify the packed `cX_Y_Z.vtf` asset exactly.
    pub origin: [i32; 3],
    pub size: u8,
}

impl MapCubemapProbe {
    pub fn position(self) -> Vec3 {
        Vec3::from(self.origin.map(|coordinate| coordinate as f32))
    }
}

impl From<&vformats::bsp::CubemapSample> for MapCubemapProbe {
    fn from(sample: &vformats::bsp::CubemapSample) -> Self {
        Self {
            origin: sample.origin,
            size: sample.size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{load_map, tests::bsp_fixture_with_entities};
    use super::*;

    #[test]
    fn map_probe_positions_are_not_rescaled_by_the_sky_camera() {
        let mut bytes = bsp_fixture_with_entities(
            r#"{ "classname" "worldspawn" } { "classname" "sky_camera" "origin" "100 200 300" "scale" "32" }"#,
        );
        let offset = bytes.len();
        for coordinate in [i32::MAX, -1472, -96] {
            bytes.extend_from_slice(&coordinate.to_le_bytes());
        }
        bytes.extend_from_slice(&[7, 0, 0, 0]);
        let entry = 8 + vformats::bsp::lump_ids::CUBEMAPS * 16;
        bytes[entry..entry + 4].copy_from_slice(&(offset as u32).to_le_bytes());
        bytes[entry + 4..entry + 8].copy_from_slice(&16_u32.to_le_bytes());
        let map = load_map(&bytes).unwrap();
        assert_eq!(
            map.cubemap_probes,
            vec![MapCubemapProbe {
                origin: [i32::MAX, -1472, -96],
                size: 7
            }]
        );
        assert_eq!(map.cubemap_probes[0].position()[1], -1472.0);
        assert_eq!(map.sky_camera.unwrap().scale, 32.0);
    }
}
