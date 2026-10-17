use super::bindings::{PageNeed, push_page};
use super::images::{ImageLease, ImageView};
use super::resources::RenderResources;
use super::samplers::SamplerDiagnostic;
use super::upload::UploadedSkybox;
use super::{PreviewScene, wgpu};
use crate::bridge::materials::{
    DecodedTextureFrames, ReloadableTextureSource, ResidentTexture, ResolvedTexture,
    ScreenRefraction, TextureFrameRequest, TextureLoadOutcome, TextureReadTransfer, TextureRole,
};
use std::sync::Arc;

struct SkyTexture<'a> {
    resident: Option<&'a ResidentTexture>,
    on_demand: Option<(&'a Arc<ReloadableTextureSource>, &'a TextureFrameRequest)>,
    flags: u32,
    outcome: TextureLoadOutcome,
}
impl<'a> From<&'a ResolvedTexture> for SkyTexture<'a> {
    fn from(texture: &'a ResolvedTexture) -> Self {
        Self {
            resident: texture.resident().map(AsRef::as_ref),
            on_demand: texture.on_demand(),
            flags: texture.flags().unwrap_or(0),
            outcome: texture.load_outcome(),
        }
    }
}
impl<'a> From<&'a DecodedTextureFrames> for SkyTexture<'a> {
    fn from(texture: &'a DecodedTextureFrames) -> Self {
        Self {
            resident: texture.resident().map(AsRef::as_ref),
            on_demand: texture.on_demand(),
            flags: texture.flags(),
            outcome: texture.load_outcome(),
        }
    }
}

pub(super) fn sky_pages(source: &PreviewScene) -> Vec<PageNeed> {
    let mut pages = Vec::new();
    if let Some(skybox) = source.map().and_then(|map| map.skybox.as_ref()) {
        for (index, material) in skybox.materials.iter().enumerate() {
            let Some(material) = material else { continue };
            if material.no_draw {
                continue;
            }
            if material.is_painted() {
                let texture = material
                    .painted
                    .as_ref()
                    .and_then(|paint| paint.texture.as_deref());
                push_page(
                    &mut pages,
                    texture.and_then(DecodedTextureFrames::on_demand),
                    0,
                    TextureReadTransfer::Unmodified,
                );
            } else {
                push_page(
                    &mut pages,
                    skybox.faces[index]
                        .as_ref()
                        .and_then(|texture| texture.on_demand()),
                    0,
                    TextureReadTransfer::SrgbToLinear,
                );
            }
        }
    }
    pages
}

pub(super) fn visit_sky_residents(
    source: &PreviewScene,
    visit: &mut impl FnMut(&ResidentTexture, TextureReadTransfer) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(skybox) = source.map().and_then(|map| map.skybox.as_ref()) {
        for (index, material) in skybox.materials.iter().enumerate() {
            let Some(material) = material.as_ref().filter(|material| !material.no_draw) else {
                continue;
            };
            let (choice, transfer) = if material.is_painted() {
                (
                    material
                        .painted
                        .as_ref()
                        .and_then(|paint| paint.texture.as_deref())
                        .map(SkyTexture::from),
                    TextureReadTransfer::Unmodified,
                )
            } else {
                (
                    skybox.faces[index].as_deref().map(SkyTexture::from),
                    TextureReadTransfer::SrgbToLinear,
                )
            };
            if let Some(choice) = choice.filter(|choice| !choice.outcome.is_unavailable())
                && let Some(resident) = choice.resident
            {
                visit(resident, transfer)?;
            }
        }
    }
    Ok(())
}

pub(super) fn sky(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &RenderResources,
    source: &PreviewScene,
    images: &mut ImageLease<'_>,
    samplers: &mut Vec<SamplerDiagnostic>,
) -> Option<UploadedSkybox> {
    let skybox = source.map()?.skybox.as_ref()?;
    let face_bind_groups = std::array::from_fn(|index| {
        let material = skybox.materials[index].as_ref()?;
        if material.no_draw {
            return None;
        }
        let (choice, role, transfer) = if material.is_painted() {
            (
                material
                    .painted
                    .as_ref()
                    .and_then(|paint| paint.texture.as_deref())
                    .map(SkyTexture::from),
                "stars",
                TextureReadTransfer::Unmodified,
            )
        } else {
            (
                skybox.faces[index].as_deref().map(SkyTexture::from),
                "face",
                TextureReadTransfer::SrgbToLinear,
            )
        };
        let mut flags = 0;
        let view = choice
            .and_then(|choice| {
                if choice.outcome.is_unavailable() {
                    images.reject("<sky>", role, format!("{:?}", choice.outcome));
                    return None;
                }
                flags = choice.flags;
                let result = if let Some((_, recipe)) = choice.on_demand {
                    let mut request = recipe.clone();
                    request.frame = 0;
                    images.cached_page_view(&request, transfer, ImageView::D2(0))
                } else {
                    choice.resident.map(|resident| {
                        images.view(device, queue, resident, transfer, ImageView::D2(0))
                    })
                };
                match result {
                    Some(Ok(view)) => Some(view),
                    Some(Err(error)) => {
                        images.reject_upload("<sky>", role, &error);
                        None
                    }
                    None if choice.on_demand.is_some() => {
                        images.reject("<sky>", role, "requested sky page is not resident");
                        None
                    }
                    None => None,
                }
            })
            .unwrap_or_else(|| resources.sky_black_texture.clone());
        Some(resources.create_sky_material(
            device,
            &view,
            super::sky::SkyUniform::new(material),
            flags,
            samplers,
        ))
    });
    Some(UploadedSkybox { face_bind_groups })
}

pub(super) fn immersion_pages(effect: &ScreenRefraction) -> Vec<PageNeed> {
    let mut pages = Vec::new();
    if let Ok(frame) = super::frame_selection::immersion_frame(effect) {
        push_page(
            &mut pages,
            effect.normal_map.on_demand(),
            frame,
            TextureRole::Normal.read_transfer(),
        );
    }
    pages
}

pub(super) fn immersion(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &RenderResources,
    effect: &ScreenRefraction,
    images: &mut ImageLease<'_>,
    samplers: &mut Vec<SamplerDiagnostic>,
) -> Arc<super::screen_refraction::Texture> {
    let view = match super::frame_selection::immersion_frame(effect) {
        Ok(frame) => RenderResources::create_data_frame_view(
            device,
            queue,
            &effect.normal_map,
            frame,
            TextureRole::Normal,
            images,
            &effect.name,
        ),
        Err(error) => {
            images.reject(&effect.name, "underwater_normal", error);
            None
        }
    };
    let flags = view.as_ref().and_then(|_| {
        effect
            .normal_map
            .source_metadata()
            .and_then(|metadata| metadata.flags)
    });
    let sampler = resources.samplers.texture(
        &effect.name,
        "underwater_normal",
        flags,
        super::samplers::TextureKind::Texture2d,
        resources.sampler_quality,
        samplers,
    );
    Arc::new(super::screen_refraction::Texture {
        view: view.unwrap_or_else(|| resources.flat_normal.clone()),
        sampler,
    })
}
