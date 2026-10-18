use super::{MaterialResolver, ModelData, StaticPropPlacement, Vec3};
use std::{collections::HashMap, sync::OnceLock};
use vformats::{
    Limits,
    mdl::{ModelLightingMesh, ModelLightingVertex, VhvError, Vtx, parse_vhv, parse_vtx},
};

#[derive(Debug)]
pub(in super::super) struct CompiledPropLighting {
    pub(super) illumination: Vec<Vec<Vec3>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in super::super) struct PropLightingStats {
    pub(in super::super) compiled: usize,
    pub(in super::super) missing: usize,
    pub(in super::super) disabled: usize,
    pub(in super::super) incompatible: usize,
}

pub(super) fn load_compiled_lighting(
    placement: &StaticPropPlacement,
    model: &ModelData,
    resolver: &MaterialResolver,
    hdr: bool,
) -> Result<Option<CompiledPropLighting>, VhvError> {
    let Some(index) = placement
        .static_prop_index
        .filter(|_| !placement.entity && placement.flags & 0x40 == 0)
    else {
        return Ok(None);
    };
    let Some(bytes) = resolver.packed_prop_lighting(index, hdr) else {
        return Ok(None);
    };
    let metadata = model.lighting.as_ref().ok_or(VhvError::ModelIncompatible)?;
    let vhv = parse_vhv(&bytes, &Limits::default())?;
    vhv.validate_layout(
        metadata.checksum,
        metadata.compatible && metadata.compiler_layout_verified,
        &metadata.meshes,
    )?;
    if metadata.vertices.len() != model.meshes.len() {
        return Err(VhvError::Corrupt {
            part: "model mesh correspondence",
        });
    }
    for &(first, second) in &metadata.compiler_aliases {
        if vhv.color_rgba(first).is_none() || vhv.color_rgba(first) != vhv.color_rgba(second) {
            return Err(VhvError::Corrupt {
                part: "ambiguous compiler vertex lighting",
            });
        }
    }
    let illumination = model
        .meshes
        .iter()
        .zip(&metadata.vertices)
        .map(|(mesh, mapping)| {
            if mapping.len() != mesh.vertices.len() {
                return Err(VhvError::Corrupt {
                    part: "model vertex correspondence",
                });
            }
            mapping
                .iter()
                .map(|vertex| {
                    vhv.color_rgba(*vertex)
                        .map(decode_vhv_color)
                        .ok_or(VhvError::Corrupt {
                            part: "hardware vertex correspondence",
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(CompiledPropLighting { illumination }))
}

pub(super) fn decode_vhv_color(color: [u8; 4]) -> Vec3 {
    static LINEAR: OnceLock<[f32; 256]> = OnceLock::new();
    let table =
        LINEAR.get_or_init(|| std::array::from_fn(|byte| (byte as f32 / 255.0 * 2.0).powf(2.2)));
    // Source converts the static specular colour stream to linear illumination
    // in its vertex shader. Decoding before interpolation preserves that order.
    Vec3::new(
        table[usize::from(color[0])],
        table[usize::from(color[1])],
        table[usize::from(color[2])],
    )
}

#[derive(Debug, PartialEq, Eq)]
struct HardwarePool {
    key: (usize, usize, usize, usize),
    vertices: Vec<u16>,
}

fn hardware_pools(vtx: &Vtx) -> Vec<HardwarePool> {
    let mut pools = Vec::new();
    for (part_index, part) in vtx.body_parts.iter().enumerate() {
        for (model_index, model) in part.models.iter().enumerate() {
            for (lod_index, lod) in model.lods.iter().enumerate() {
                for (mesh_index, mesh) in lod.meshes.iter().enumerate() {
                    for group in &mesh.strip_groups {
                        pools.push(HardwarePool {
                            key: (part_index, model_index, lod_index, mesh_index),
                            vertices: group
                                .vertices
                                .iter()
                                .map(|vertex| vertex.original_mesh_vertex_id)
                                .collect(),
                        });
                    }
                }
            }
        }
    }
    pools
}

/// VRAD compiles VHV in DX80 strip-group order. Different render companions
/// need an original-VVD-identity mapping, even when counts and checksums agree.
pub(super) fn prepare_compiler_layout(
    model: &mut ModelData,
    rendered_bytes: &[u8],
    compiler_bytes: Option<&[u8]>,
) -> Result<(), String> {
    let Some(metadata) = model.lighting.as_mut() else {
        return Err("model has no hardware lighting metadata".to_owned());
    };
    let metadata = std::sync::Arc::make_mut(metadata);
    metadata.compiler_layout_verified = false;
    if !metadata.compatible {
        return Err("model companions have incompatible checksums or meshes".to_owned());
    }
    let compiler_bytes =
        compiler_bytes.ok_or_else(|| "missing DX80 compiler companion".to_owned())?;
    let limits = Limits::default();
    let rendered = parse_vtx(rendered_bytes, &limits).map_err(|error| error.to_string())?;
    let compiler = parse_vtx(compiler_bytes, &limits).map_err(|error| error.to_string())?;
    if rendered.checksum != metadata.checksum || compiler.checksum != metadata.checksum {
        return Err("DX80 compiler companion has a different model checksum".to_owned());
    }
    let rendered_pools = hardware_pools(&rendered);
    let compiler_pools = hardware_pools(&compiler);
    remap_compiler_pools(metadata, &rendered_pools, &compiler_pools)
}

fn remap_compiler_pools(
    metadata: &mut crate::media::preview_model::ModelLightingData,
    rendered_pools: &[HardwarePool],
    compiler_pools: &[HardwarePool],
) -> Result<(), String> {
    metadata.compiler_layout_verified = false;
    metadata.compiler_aliases.clear();
    if rendered_pools == compiler_pools {
        metadata.compiler_layout_verified = true;
        return Ok(());
    }
    let mut lookup = HashMap::new();
    let mut aliases = Vec::new();
    for (mesh, pool) in compiler_pools.iter().enumerate() {
        for (vertex, &original) in pool.vertices.iter().enumerate() {
            let identity = (pool.key, original);
            let location = ModelLightingVertex { mesh, vertex };
            if let Some(&first) = lookup.get(&identity) {
                aliases.push((first, location));
            } else {
                lookup.insert(identity, location);
            }
        }
    }
    let vertices = metadata
        .vertices
        .iter()
        .map(|mesh| {
            mesh.iter()
                .map(|location| {
                    let pool = rendered_pools
                        .get(location.mesh)
                        .ok_or_else(|| "invalid render hardware mesh index".to_owned())?;
                    let original = *pool
                        .vertices
                        .get(location.vertex)
                        .ok_or_else(|| "invalid render hardware vertex index".to_owned())?;
                    lookup.get(&(pool.key, original)).copied().ok_or_else(|| {
                        "render vertex is absent from the compiler companion".to_owned()
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    metadata.meshes = compiler_pools
        .iter()
        .map(|pool| ModelLightingMesh {
            lod: pool.key.2 as u32,
            vertex_count: pool.vertices.len(),
        })
        .collect();
    metadata.vertices = vertices;
    metadata.compiler_aliases = aliases;
    metadata.compiler_layout_verified = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{
        file_preview_decode::tests::{prop_placement, test_prop_asset},
        preview_model::ModelLightingData,
    };
    use std::sync::Arc;

    fn fixture_model() -> ModelData {
        let mut model = (*test_prop_asset(vec![0]).model).clone();
        model.lighting = Some(Arc::new(ModelLightingData {
            checksum: 123,
            compatible: true,
            compiler_layout_verified: true,
            compiler_aliases: Vec::new(),
            meshes: vec![ModelLightingMesh {
                lod: 0,
                vertex_count: 3,
            }],
            vertices: vec![vec![
                ModelLightingVertex { mesh: 0, vertex: 2 },
                ModelLightingVertex { mesh: 0, vertex: 0 },
                ModelLightingVertex { mesh: 0, vertex: 1 },
            ]],
        }));
        model
    }

    fn fixture(colors: &[[u8; 4]]) -> Vec<u8> {
        let mut bytes = vec![0; 512 + colors.len() * 4];
        for (offset, value) in [
            (0, 2),
            (4, 123),
            (8, 4),
            (12, 4),
            (16, colors.len() as u32),
            (20, 1),
            (40, 0),
            (44, colors.len() as u32),
            (48, 512),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (index, color) in colors.iter().enumerate() {
            bytes[512 + index * 4..516 + index * 4]
                .copy_from_slice(&[color[2], color[1], color[0], color[3]]);
        }
        bytes
    }

    #[test]
    fn compiled_lighting_uses_original_indices_hdr_source_and_vertex_order() {
        let colors = [[0, 32, 64, 255], [64, 128, 255, 255], [255, 64, 32, 255]];
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                ("sp_7.vhv", fixture(&colors)),
                ("sp_hdr_7.vhv", fixture(&[[128; 4]; 3])),
            ],
            vec![],
        );
        let model = fixture_model();
        let mut placement = prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0);
        placement.static_prop_index = Some(7);
        let compiled = load_compiled_lighting(&placement, &model, &resolver, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            compiled.illumination[0],
            [
                decode_vhv_color(colors[2]),
                decode_vhv_color(colors[0]),
                decode_vhv_color(colors[1])
            ]
        );
        let hdr = load_compiled_lighting(&placement, &model, &resolver, true)
            .unwrap()
            .unwrap();
        assert_eq!(hdr.illumination[0], [decode_vhv_color([128; 4]); 3]);
        placement.static_prop_index = Some(8);
        assert!(
            load_compiled_lighting(&placement, &model, &resolver, false)
                .unwrap()
                .is_none()
        );
        placement.static_prop_index = Some(7);
        placement.flags = 0x40;
        assert!(
            load_compiled_lighting(&placement, &model, &resolver, false)
                .unwrap()
                .is_none()
        );
        placement.flags = 0;
        placement.entity = true;
        assert!(
            load_compiled_lighting(&placement, &model, &resolver, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_or_unverified_lighting_is_rejected_before_baking() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("sp_0.vhv", fixture(&[[64; 4]; 3]))],
            vec![],
        );
        let mut placement = prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0);
        placement.static_prop_index = Some(0);
        let mut model = fixture_model();
        Arc::make_mut(model.lighting.as_mut().unwrap()).checksum = 999;
        assert!(matches!(
            load_compiled_lighting(&placement, &model, &resolver, false),
            Err(VhvError::ChecksumMismatch { .. })
        ));
        let metadata = Arc::make_mut(model.lighting.as_mut().unwrap());
        metadata.checksum = 123;
        metadata.compiler_layout_verified = false;
        assert!(matches!(
            load_compiled_lighting(&placement, &model, &resolver, false),
            Err(VhvError::ModelIncompatible)
        ));
        let metadata = Arc::make_mut(model.lighting.as_mut().unwrap());
        metadata.compiler_layout_verified = true;
        metadata.vertices[0][0].vertex = 99;
        assert!(matches!(
            load_compiled_lighting(&placement, &model, &resolver, false),
            Err(VhvError::Corrupt { .. })
        ));
    }

    #[test]
    fn compiler_remapping_uses_original_vertex_ids_not_counts_or_pool_positions() {
        let mut model = fixture_model();
        let metadata = Arc::make_mut(model.lighting.as_mut().unwrap());
        let rendered = [HardwarePool {
            key: (0, 0, 0, 0),
            vertices: vec![10, 20, 30],
        }];
        let compiler = [HardwarePool {
            key: (0, 0, 0, 0),
            vertices: vec![30, 10, 20],
        }];
        remap_compiler_pools(metadata, &rendered, &compiler).unwrap();
        assert!(metadata.compiler_layout_verified);
        assert_eq!(
            metadata.vertices[0]
                .iter()
                .map(|vertex| vertex.vertex)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(metadata.compiler_aliases.is_empty());

        let mut model = fixture_model();
        let metadata = Arc::make_mut(model.lighting.as_mut().unwrap());
        let missing = [HardwarePool {
            key: (0, 0, 0, 1),
            vertices: vec![30, 10, 20],
        }];
        assert!(remap_compiler_pools(metadata, &rendered, &missing).is_err());
        assert!(!metadata.compiler_layout_verified);
    }

    #[test]
    fn remapped_duplicate_compiler_vertices_require_identical_lighting() {
        let mut model = fixture_model();
        let metadata = Arc::make_mut(model.lighting.as_mut().unwrap());
        let rendered = [HardwarePool {
            key: (0, 0, 0, 0),
            vertices: vec![10, 20, 30],
        }];
        let compiler = [HardwarePool {
            key: (0, 0, 0, 0),
            vertices: vec![10, 20, 30, 10],
        }];
        remap_compiler_pools(metadata, &rendered, &compiler).unwrap();
        assert_eq!(metadata.compiler_aliases.len(), 1);
        let mut placement = prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0);
        placement.static_prop_index = Some(0);
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("sp_0.vhv", fixture(&[[64; 4]; 4]))],
            vec![],
        );
        assert!(
            load_compiled_lighting(&placement, &model, &resolver, false)
                .unwrap()
                .is_some()
        );
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("sp_0.vhv", fixture(&[[64; 4], [64; 4], [64; 4], [128; 4]]))],
            vec![],
        );
        assert!(matches!(
            load_compiled_lighting(&placement, &model, &resolver, false),
            Err(VhvError::Corrupt {
                part: "ambiguous compiler vertex lighting"
            })
        ));
    }

    #[test]
    fn encoded_lighting_decodes_before_interpolation_and_preserves_overbright() {
        assert_eq!(decode_vhv_color([0; 4]), Vec3::ZERO);
        let high = decode_vhv_color([255; 4]);
        assert!((high.x() - 4.594_793_4).abs() < 1e-6);
        let first = decode_vhv_color([32; 4]);
        let second = decode_vhv_color([128; 4]);
        let interpolated = (first + second) * 0.5;
        assert!((interpolated.x() - decode_vhv_color([80; 4]).x()).abs() > 0.1);
    }
}
