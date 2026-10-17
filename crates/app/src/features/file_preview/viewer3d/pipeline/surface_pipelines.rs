use super::{draw::ViewKind, resources::RenderResources, wgpu};
use crate::{bridge::materials::RenderMode, media::preview_model::MaterialSlot};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(super) struct SurfaceRaster {
    pub no_cull: bool,
    pub ignore_z: bool,
    pub decal_bias: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SurfaceBlend {
    Opaque,
    Alpha,
    Additive,
    Modulate,
}

impl SurfaceBlend {
    pub(super) const fn color_transfer(self) -> super::targets::ColorTransfer {
        match self {
            Self::Modulate => super::targets::ColorTransfer::Encoded,
            Self::Opaque | Self::Alpha | Self::Additive => super::targets::ColorTransfer::Linear,
        }
    }
}

impl From<RenderMode> for SurfaceBlend {
    fn from(mode: RenderMode) -> Self {
        match mode {
            RenderMode::NoDraw | RenderMode::Opaque | RenderMode::Cutout => Self::Opaque,
            RenderMode::Translucent => Self::Alpha,
            RenderMode::Additive => Self::Additive,
            RenderMode::Modulate => Self::Modulate,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct SurfacePipelineKey {
    pub raster: SurfaceRaster,
    pub view: ViewKind,
    pub overlay: bool,
    pub blend: SurfaceBlend,
}

#[derive(Debug)]
pub(super) struct SurfacePipelineVariants {
    materials: Vec<SurfaceRaster>,
    pipelines: HashMap<SurfacePipelineKey, wgpu::RenderPipeline>,
}

impl SurfacePipelineVariants {
    pub(super) fn new(
        device: &wgpu::Device,
        resources: &RenderResources,
        materials: &[MaterialSlot],
    ) -> Self {
        let raster = materials
            .iter()
            .map(|material| {
                material
                    .shader
                    .surface()
                    .map_or_else(SurfaceRaster::default, |surface| SurfaceRaster {
                        no_cull: surface.no_cull,
                        ignore_z: surface.ignore_z,
                        decal_bias: surface.decal,
                    })
            })
            .collect::<Vec<_>>();
        let mut pipelines = HashMap::new();
        for (material, &authored) in materials.iter().zip(&raster) {
            if !material.render_mode.draws() {
                continue;
            }
            for state in [
                authored,
                SurfaceRaster {
                    no_cull: false,
                    ..authored
                },
            ] {
                for view in [ViewKind::Main, ViewKind::Reflection, ViewKind::Refraction] {
                    for overlay in [false, true] {
                        if !state.ignore_z && (overlay || (!state.no_cull && !state.decal_bias)) {
                            continue;
                        }
                        let key = SurfacePipelineKey {
                            raster: state,
                            view,
                            overlay,
                            blend: material.render_mode.into(),
                        };
                        pipelines
                            .entry(key)
                            .or_insert_with(|| resources.create_surface_variant(device, key));
                    }
                }
            }
        }
        Self {
            materials: raster,
            pipelines,
        }
    }

    pub(super) fn get(
        &self,
        material: usize,
        view: ViewKind,
        overlay: bool,
        blend: SurfaceBlend,
        geometry: crate::media::preview_model::MeshGeometry,
    ) -> Option<&wgpu::RenderPipeline> {
        let mut raster = self.materials.get(material).copied().unwrap_or_default();
        if overlay || geometry == crate::media::preview_model::MeshGeometry::Brush {
            raster.no_cull = false;
        }
        self.pipelines.get(&SurfacePipelineKey {
            raster,
            view,
            overlay,
            blend,
        })
    }
}
