//! Preserved compiled emitters, before engine light selection and shadowing.
use super::{AmbientLightSource, LightmapSource, MapBsp};
use vformats::bsp::{WorldLightError, WorldLightKind};

pub type MapWorldLight = vformats::bsp::WorldLight;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapLightStyleEntity {
    pub original_index: usize,
    pub style: Option<i32>,
    pub targetname: Option<String>,
    pub pattern: Option<String>,
    pub default_style: Option<i32>,
    pub spawnflags: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapWorldLightSet {
    pub lights: Vec<MapWorldLight>,
    pub error: Option<WorldLightError>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapWorldLighting {
    /// Follows selected compiled scene lighting; no cross-mode substitution.
    pub source: Option<LightmapSource>,
    pub ldr: MapWorldLightSet,
    pub hdr: MapWorldLightSet,
    /// Authored switchable-light inputs, kept separate from atlas defaults.
    pub light_style_entities: Vec<MapLightStyleEntity>,
}

impl MapWorldLighting {
    pub const fn selected(&self) -> Option<&MapWorldLightSet> {
        match self.source {
            Some(LightmapSource::Ldr) => Some(&self.ldr),
            Some(LightmapSource::Hdr) => Some(&self.hdr),
            None => None,
        }
    }
}

impl MapWorldLightSet {
    fn decode(result: &Result<Vec<MapWorldLight>, WorldLightError>, label: &str) -> Self {
        match result {
            Ok(lights) => {
                for light in lights {
                    if let WorldLightKind::Unknown(kind) = light.kind {
                        log::warn!(
                            "map {label} world-light {} has unsupported emitter type {kind}",
                            light.original_index
                        );
                    }
                }
                Self {
                    lights: lights.clone(),
                    error: None,
                }
            }
            Err(error) => {
                log::warn!("map {label} world lights unavailable: {error}");
                Self {
                    lights: Vec::new(),
                    error: Some(error.clone()),
                }
            }
        }
    }
}

pub(super) fn map_world_lighting(
    bsp: &MapBsp,
    lightmap_source: Option<LightmapSource>,
    ambient_source: AmbientLightSource,
) -> MapWorldLighting {
    let source = lightmap_source.or(match ambient_source {
        AmbientLightSource::Ldr => Some(LightmapSource::Ldr),
        AmbientLightSource::Hdr => Some(LightmapSource::Hdr),
        AmbientLightSource::Neutral => None,
    });
    MapWorldLighting {
        source,
        ldr: MapWorldLightSet::decode(&bsp.world_lights, "LDR"),
        hdr: MapWorldLightSet::decode(&bsp.world_lights_hdr, "HDR"),
        light_style_entities: bsp
            .entities
            .iter()
            .enumerate()
            .filter(|(_, entity)| {
                matches!(
                    entity.prop("classname"),
                    Some("light" | "light_spot" | "light_environment")
                )
            })
            .map(|(original_index, entity)| MapLightStyleEntity {
                original_index,
                style: entity.prop("style").and_then(|value| value.parse().ok()),
                targetname: entity.prop("targetname").map(str::to_owned),
                pattern: entity.prop("pattern").map(str::to_owned),
                default_style: entity
                    .prop("defaultstyle")
                    .and_then(|value| value.parse().ok()),
                spawnflags: entity
                    .prop("spawnflags")
                    .and_then(|value| value.parse().ok()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::map::{MapAmbientLighting, MapLeaf, MapLeafLocator, Vec3, lighting};
    use vformats::{Limits, bsp::lump_ids};

    fn bsp_fixture_header() -> Vec<u8> {
        let mut bytes = vec![0; 1036];
        bytes[..4].copy_from_slice(b"VBSP");
        bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
        bytes
    }

    fn append_lump(bytes: &mut Vec<u8>, lump: usize, data: &[u8], version: u32) {
        let offset = bytes.len() as u32;
        let at = 8 + lump * 16;
        bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(data);
    }

    fn record(colour: f32) -> Vec<u8> {
        let mut bytes = vec![0; 88];
        bytes[12..16].copy_from_slice(&colour.to_le_bytes());
        bytes[40..44].copy_from_slice(&1_i32.to_le_bytes());
        bytes
    }

    #[test]
    fn world_light_channels_preserve_identity_and_follow_selected_scene_mode() {
        let mut bytes = bsp_fixture_header();
        append_lump(
            &mut bytes,
            lump_ids::WORLDLIGHTS,
            &[record(0.5), record(2.0)].concat(),
            0,
        );
        append_lump(&mut bytes, lump_ids::WORLDLIGHTS_HDR, &record(4.0), 0);
        let bsp = MapBsp::parse(&bytes, &Limits::default(), None).unwrap();
        let map = map_world_lighting(&bsp, Some(LightmapSource::Hdr), AmbientLightSource::Ldr);
        assert_eq!(map.selected().unwrap().lights[0].intensity, [4.0, 0.0, 0.0]);
        assert_eq!(map.ldr.lights[1].original_index, 1);
        assert_eq!(map.ldr.lights[1].intensity, [2.0, 0.0, 0.0]);
        assert_eq!(
            map_world_lighting(&bsp, None, AmbientLightSource::Ldr).source,
            Some(LightmapSource::Ldr)
        );
        assert!(
            map_world_lighting(&bsp, None, AmbientLightSource::Neutral)
                .selected()
                .is_none()
        );
    }

    #[test]
    fn malformed_optional_channel_is_diagnosed_without_borrowing_other_mode() {
        let mut bytes = bsp_fixture_header();
        append_lump(&mut bytes, lump_ids::WORLDLIGHTS, &record(1.0), 0);
        let mut malformed = record(2.0);
        malformed[72..76].copy_from_slice(&f32::NAN.to_le_bytes());
        append_lump(&mut bytes, lump_ids::WORLDLIGHTS_HDR, &malformed, 0);
        let bsp = MapBsp::parse(&bytes, &Limits::default(), None).unwrap();
        let map = map_world_lighting(&bsp, Some(LightmapSource::Hdr), AmbientLightSource::Hdr);
        assert_eq!(map.ldr.lights.len(), 1);
        assert!(map.selected().unwrap().lights.is_empty());
        assert!(matches!(
            map.hdr.error,
            Some(WorldLightError::NonFinite {
                index: 0,
                field: "quadratic attenuation"
            })
        ));
        let loaded = crate::scene::map::load_map(&bytes).unwrap();
        assert!(loaded.world_lighting.hdr.error.is_some());
    }

    #[test]
    fn raw_ambient_access_retains_leaf_sample_positions_and_all_six_directions() {
        let cube = lighting::AmbientCube {
            colors: std::array::from_fn(|i| Vec3::new(i as f32, 2.0, 3.0)),
        };
        let ambient = MapAmbientLighting {
            source: AmbientLightSource::Ldr,
            locator: MapLeafLocator::default(),
            samples: vec![lighting::MapAmbientSample {
                position: Vec3::new(2.0, 3.0, 4.0),
                encoded_position: [17, 23, 31],
                cube,
                cluster: 7,
            }],
            leaf_sample_ranges: vec![lighting::AmbientSampleRange { start: 0, count: 1 }],
        };
        let sample = &ambient.samples_for_leaf(0)[0];
        assert_eq!(sample.position, Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(sample.cluster, 7);
        assert_eq!(sample.encoded_position, [17, 23, 31]);
        assert_eq!(sample.cube.colors, cube.colors);
        assert!(ambient.samples_for_leaf(1).is_empty());
        assert_eq!(ambient.samples(), ambient.samples_for_leaf(0));
    }

    #[test]
    fn leaf_interpolation_matches_all_six_independent_native_direction_readouts() {
        let leaf = MapLeaf {
            cluster: 392,
            mins: [-3072, -1408, -96],
            maxs: [-2932, -1056, -16],
        };
        let mut samples = Vec::new();
        for sample in 0..16 {
            let second = sample >= 8;
            let encoded_position = [if second { 192 } else { 64 }, 128, 128];
            let cube = lighting::AmbientCube {
                colors: std::array::from_fn(|axis| {
                    let (r, g, b) = if second {
                        (64 + 2 * axis, 48 + 3 * axis, 40 + 2 * axis)
                    } else {
                        (16 + 3 * axis, 24 + 2 * axis, 32 + axis)
                    };
                    Vec3::new(r as f32, g as f32, b as f32) / 256.0
                }),
            };
            samples.push(lighting::MapAmbientSample {
                position: lighting::ambient_sample_position(encoded_position, &leaf),
                encoded_position,
                cube,
                cluster: leaf.cluster,
            });
        }
        let ambient = MapAmbientLighting {
            source: AmbientLightSource::Ldr,
            locator: MapLeafLocator {
                leaves: vec![leaf],
                ..MapLeafLocator::default()
            },
            samples,
            leaf_sample_ranges: vec![lighting::AmbientSampleRange {
                start: 0,
                count: 16,
            }],
        };
        assert_eq!(
            ambient.leaf_bounds(0),
            Some(([-3072, -1408, -96], [-2932, -1056, -16]))
        );
        // Native model-lighting-order-forward-game-reference01 x_28. The audited
        // cache resolves its requested point to this candidate cell center.
        let actual = ambient
            .interpolate_leaf_at(0, Vec3::new(-3004.0, -1228.0, -56.0))
            .unwrap();
        let expected = [
            [0.144_279, 0.134_639_52, 0.138_629_81],
            [0.154_294_03, 0.144_155_74, 0.144_239_86],
            [0.164_309_07, 0.153_671_95, 0.149_849_8],
            [0.174_324_1, 0.163_188_2, 0.155_459_78],
            [0.184_339_1, 0.172_704_4, 0.161_069_78],
            [0.194_354_15, 0.182_220_65, 0.166_679_74],
        ];
        for (color, expected) in actual.colors.into_iter().zip(expected) {
            assert!(color.distance(Vec3::from(expected)) < 2.0e-7);
        }
        let other = ambient
            .interpolate_leaf_at(0, Vec3::new(-3012.0, -1228.0, -56.0))
            .unwrap();
        assert!(actual.colors[0].distance(other.colors[0]) > 0.04);
        assert!(ambient.interpolate_leaf_at(1, Vec3::ZERO).is_none());
        assert!(
            ambient
                .interpolate_leaf_at(0, Vec3::splat(f32::NAN))
                .is_none()
        );
        assert!(
            MapAmbientLighting::neutral()
                .interpolate_leaf_at(0, Vec3::ZERO)
                .is_none()
        );
    }
}
