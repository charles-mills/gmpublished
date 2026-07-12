use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{App, RootMessage, file_preview, iced_mpsc, send_root_message, stream};

use crate::features::file_preview::model::{MeshData, ModelData, ModelVertex};
#[cfg(test)]
use gmpublished_backend::scene::map::MapVisibilityBucket;
use gmpublished_backend::scene::map::{
    AmbientCube, ConvexHull, MapAmbientLighting, MapDetailSprite, MapDoor, MapDoorGeometry,
    MapEnvironmentLighting, MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility, MapOverlay,
    MapPropVisibility, MapVisibility, MapWalkCollision, MapWalkPropModel,
    MapWalkPropModelPlacement, StaticPropPlacement,
};
use iced::Task;
use iced::widget::image as iced_image;
use rodio::Source as _;
use vformats::phy::{ConvexLedge, ReadStats, SkipReason};

use crate::backend::materials::{
    ContentSourceTier, DecodedTextureBudget, MaterialResolver, RenderMode,
    ResolvedMaterialTextures, ResolvedPrimaryMaterial, ResolvedSoundReference, srgb_byte_to_linear,
};
use crate::features::file_preview::{
    DetailSprite, DoorInstance, DoorSound, DoorSoundSourceTier, DoorSoundWave, DoorSounds,
    InfoReason, LightmapSlot, MAX_PREVIEW_LINES, MapFog, MapSkyCamera, MapSpawn, MapStats,
    MaterialSlot, ModelPreview, ModelStats, OverlayPrimitive, OverlayVertex,
    PHY_DEBUG_MATERIAL_NAME, ParticleMaterialSlot, ParticlePreview, ParticleSystemInfo,
    PreviewContent, PreviewData, PreviewLoadError, PreviewLoadStage, PreviewRequest, Skybox,
    SkyboxFace, normalize_particle_material,
};
use crate::theme::Tokens;

mod map_preview;
mod materials;
mod routing;
mod syntax;

#[cfg(test)]
use crate::features::file_preview::{CodeLine, RelatedPreviewKind, RelatedPreviewTarget};
#[cfg(test)]
use map_preview::{
    LoadedPropModel, LoadedPropPhysics, PropBakeSkipStats, PropMaterialState, PropModelAsset,
    PropPlacementLighting, PropSunLighting, StaticPropLightingInputs,
    bake_map_doors_with_prop_model_loader, bake_prop_placement, bake_static_props,
    bake_static_props_with_loaded_model_cache, bake_static_props_with_loader,
    bake_static_props_with_loader_serial, map_preview_data_with_prop_model_loader,
    normalize_map_uv, transform_prop_normal, transform_prop_position,
    unresolved_material_names_for_debug,
};
use map_preview::{PhyDebugMeshBuilder, map_preview_data, parse_phy_bytes};
#[cfg(test)]
use materials::{
    resolve_map_material_slots_parallel, resolve_map_material_slots_serial, resolve_skybox,
    skybox_face_material_path,
};
#[cfg(test)]
use routing::model_companion_parent_path;
use routing::{
    CodeSyntax, EntryClass, ImageClass, classify_entry_path, model_companion_preview_request,
    related_preview_target,
};
#[cfg(test)]
use syntax::{CodeHighlightPalette, VmtHighlightPalette};
use syntax::{glua_highlighted_lines, json_highlighted_lines, plain_lines, vmt_highlighted_lines};

const TEXT_TRUNCATE_BYTES: usize = 512 * 1024;
const TEXT_TOO_LARGE_BYTES: usize = 4 * 1024 * 1024;
const IMAGE_TOO_LARGE_BYTES: usize = 32 * 1024 * 1024;
const AUDIO_TOO_LARGE_BYTES: usize = 64 * 1024 * 1024;
const MAP_TOO_LARGE_BYTES: usize = gmpublished_backend::scene::map::MAX_BSP_BYTES;
const PARTICLE_TOO_LARGE_BYTES: usize = 16 * 1024 * 1024;
const MAP_TEXTURE_DECODE_BUDGET_BYTES: usize = 1024 * 1024 * 1024;
const MAP_TEXTURE_MAX_DIMENSION: u32 = 512;
const MAP_FALLBACK_TEXTURE_DIMENSION: u32 = MAP_TEXTURE_MAX_DIMENSION;
const MAP_PROP_PLACEMENT_CAP: usize = 65_536;
const MAP_PROP_TRIANGLE_CAP: usize = 7_000_000;
const PHY_DEBUG_TRIANGLE_CAP: usize = 1_000_000;
const MAP_DETAIL_SPRITE_PLACEMENT_CAP: usize = 65_536;
const SKYBOX_FACE_DIMENSION_CAP: u32 = 2048;
const PHY_DEBUG_VERTEX_COLOR: [f32; 3] = [0.5, 0.5, 0.5];

impl App {
    pub(super) fn apply_file_preview_message(
        &mut self,
        message: file_preview::Message,
    ) -> Task<RootMessage> {
        let effects = file_preview::update(&mut self.state.file_preview, message);
        self.batch_effects(effects, Self::run_file_preview_effect)
    }

    fn run_file_preview_effect(&mut self, effect: file_preview::Effect) -> Task<RootMessage> {
        match effect {
            file_preview::Effect::ModalCloseRequested => self.file_preview_close_finished_task(),
            file_preview::Effect::LoadRequested(request) => self.file_preview_load_task(request),
            file_preview::Effect::ExtractRequested { entry_path } => self
                .state
                .preview_gma
                .entry_extraction_request(&entry_path)
                .map_or_else(Task::none, |request| {
                    self.preview_gma_entry_extraction_task(request)
                }),
            file_preview::Effect::AudioPlayRequested { bytes, resume_at } => {
                self.file_preview_audio_play_task(bytes, resume_at)
            }
            file_preview::Effect::AudioPauseRequested => self.file_preview_audio_pause_task(),
            file_preview::Effect::AudioStopRequested => self.file_preview_audio_stop_task(),
            file_preview::Effect::AudioPositionPollRequested => {
                self.file_preview_audio_position_poll_task()
            }
            file_preview::Effect::DoorAudioEvent(event) => {
                self.file_preview_door_audio_event_task(event)
            }
            file_preview::Effect::DoorAudioStopRequested => {
                self.file_preview_door_audio_stop_task()
            }
        }
    }

    pub(super) fn file_preview_close_finished_task(&mut self) -> Task<RootMessage> {
        self.apply_file_preview_message(file_preview::Message::CloseFinished)
    }

    fn file_preview_load_task(&self, request: PreviewRequest) -> Task<RootMessage> {
        let request_id = request.request_id;
        let tokens = self.state.tokens;
        let gmod_dir = self.ctx.settings_and_paths_snapshot().1.gmod_dir;
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let schedule = ctx.spawn_blocking_detached("file-preview-load", move |_app| {
                run_file_preview_load(&request, &tokens, gmod_dir, output);
            });
            if let Err(error) = schedule {
                log::warn!("failed to schedule file-preview worker: {error}");
                let _ = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::FilePreview(file_preview::Message::Loaded(
                        request_id,
                        Err(error.into()),
                    )),
                );
            }
        }))
    }
}

fn run_file_preview_load(
    request: &PreviewRequest,
    tokens: &Tokens,
    gmod_dir: Option<std::path::PathBuf>,
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let request_id = request.request_id;
    let result = catch_preview_build_result(request, || {
        load_preview_data(request.clone(), tokens, gmod_dir, &mut output)
    });
    let _ = send_root_message(
        &mut output,
        RootMessage::FilePreview(file_preview::Message::Loaded(request_id, result)),
    );
}

fn load_preview_data(
    mut request: PreviewRequest,
    tokens: &Tokens,
    gmod_dir: Option<std::path::PathBuf>,
    output: &mut iced_mpsc::Sender<RootMessage>,
) -> Result<PreviewData, PreviewLoadError> {
    let classified = classify_entry_path(&request.entry_path);
    let entry_class = if let EntryClass::ModelCompanion = classified {
        if let Some(parent_request) = model_companion_preview_request(&request) {
            request = parent_request;
            EntryClass::Model
        } else {
            send_load_stage(output, request.request_id, PreviewLoadStage::ReadingArchive);
            log::debug!(
                "file preview model companion {} missing parent .mdl",
                request.entry_path
            );
            return Ok(info_preview_data(&request, InfoReason::DecodeFailed));
        }
    } else {
        classified
    };
    send_load_stage(output, request.request_id, initial_load_stage(entry_class));
    let bytes = request.archive.entry_bytes(&request.entry_path)?;
    // Loose-folder entries carry crc32 = 0, which would collide the
    // size-derived content_id for same-size files; derive the real one now
    // that the bytes are in hand.
    if request.crc32 == 0 {
        request.crc32 = crc32fast::hash(&bytes);
    }

    Ok(preview_data_from_bytes_with_stages(
        &request,
        &bytes,
        tokens,
        gmod_dir,
        entry_class,
        &mut |stage| send_load_stage(output, request.request_id, stage),
    ))
}

fn send_load_stage(
    output: &mut iced_mpsc::Sender<RootMessage>,
    request_id: u64,
    stage: PreviewLoadStage,
) {
    let _ = send_root_message(
        output,
        RootMessage::FilePreview(file_preview::Message::LoadStageChanged(request_id, stage)),
    );
}

const fn initial_load_stage(entry_class: EntryClass) -> PreviewLoadStage {
    match entry_class {
        EntryClass::Map => PreviewLoadStage::ReadingBsp,
        EntryClass::Code { .. }
        | EntryClass::Image(_)
        | EntryClass::Audio
        | EntryClass::Model
        | EntryClass::ModelCompanion
        | EntryClass::Particle
        | EntryClass::Info => PreviewLoadStage::ReadingArchive,
    }
}

fn catch_preview_build_result(
    request: &PreviewRequest,
    build: impl FnOnce() -> Result<PreviewData, PreviewLoadError>,
) -> Result<PreviewData, PreviewLoadError> {
    catch_asset_decode(&request.entry_path, build)
        .unwrap_or_else(|| Ok(info_preview_data(request, InfoReason::DecodeFailed)))
}

fn catch_preview_build_data(
    request: &PreviewRequest,
    build: impl FnOnce() -> PreviewData,
) -> PreviewData {
    catch_asset_decode(&request.entry_path, build)
        .unwrap_or_else(|| info_preview_data(request, InfoReason::DecodeFailed))
}

fn catch_asset_decode<T>(context: &str, decode: impl FnOnce() -> T) -> Option<T> {
    match panic::catch_unwind(AssertUnwindSafe(decode)) {
        Ok(value) => Some(value),
        Err(payload) => {
            log::debug!(
                "file preview decode panicked for {context}: {}",
                panic_payload_message(payload.as_ref())
            );
            None
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .map_or_else(|| "non-string panic payload".to_owned(), String::clone)
        },
        |message| (*message).to_owned(),
    )
}

#[cfg(test)]
pub fn preview_data_from_bytes(
    request: &PreviewRequest,
    bytes: &[u8],
    tokens: &Tokens,
    gmod_dir: Option<std::path::PathBuf>,
) -> PreviewData {
    let entry_class = classify_entry_path(&request.entry_path);
    catch_preview_build_data(request, || {
        preview_data_from_bytes_inner(request, bytes, tokens, gmod_dir, entry_class, &mut |_| {})
    })
}

fn preview_data_from_bytes_with_stages(
    request: &PreviewRequest,
    bytes: &[u8],
    tokens: &Tokens,
    gmod_dir: Option<std::path::PathBuf>,
    entry_class: EntryClass,
    emit_stage: &mut impl FnMut(PreviewLoadStage),
) -> PreviewData {
    catch_preview_build_data(request, || {
        preview_data_from_bytes_inner(request, bytes, tokens, gmod_dir, entry_class, emit_stage)
    })
}

fn preview_data_from_bytes_inner(
    request: &PreviewRequest,
    bytes: &[u8],
    tokens: &Tokens,
    gmod_dir: Option<std::path::PathBuf>,
    entry_class: EntryClass,
    emit_stage: &mut impl FnMut(PreviewLoadStage),
) -> PreviewData {
    let mut data = match entry_class {
        EntryClass::Code { syntax } => code_preview_data(request, bytes, syntax, tokens),
        EntryClass::Image(ImageClass::Encoded) => encoded_image_preview_data(request, bytes),
        EntryClass::Image(ImageClass::Vtf) => vtf_preview_data(request, bytes),
        EntryClass::Audio => audio_preview_data(request, bytes),
        EntryClass::Model => model_preview_data(request, bytes, gmod_dir),
        EntryClass::ModelCompanion => info_preview_data(request, InfoReason::DecodeFailed),
        EntryClass::Map => map_preview_data(request, bytes, gmod_dir, emit_stage),
        EntryClass::Particle => particle_preview_data(request, bytes, gmod_dir, emit_stage),
        EntryClass::Info => info_preview_data(request, InfoReason::Binary),
    };
    data.related_preview = related_preview_target(request, bytes);
    data
}

fn code_preview_data(
    request: &PreviewRequest,
    bytes: &[u8],
    syntax: CodeSyntax,
    tokens: &Tokens,
) -> PreviewData {
    if !request.bypass_size_limits && bytes.len() > TEXT_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    let truncated_by_bytes = bytes.len() > TEXT_TRUNCATE_BYTES;
    let text_bytes = if truncated_by_bytes {
        &bytes[..TEXT_TRUNCATE_BYTES]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(text_bytes);
    let mut truncated_by_lines = false;
    let mut source_lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_PREVIEW_LINES {
            truncated_by_lines = true;
            break;
        }
        source_lines.push(line.to_owned());
    }
    if source_lines.is_empty() && text.is_empty() {
        source_lines.push(String::new());
    }

    let lines = match syntax {
        CodeSyntax::Plain => plain_lines(&source_lines),
        CodeSyntax::Glua => glua_highlighted_lines(&source_lines, tokens),
        CodeSyntax::Json => json_highlighted_lines(&source_lines, tokens),
        CodeSyntax::Vmt => vmt_highlighted_lines(&source_lines, tokens),
    };

    PreviewData::from_request(
        request,
        PreviewContent::Code {
            lines,
            truncated: truncated_by_bytes || truncated_by_lines,
        },
    )
}

fn encoded_image_preview_data(request: &PreviewRequest, bytes: &[u8]) -> PreviewData {
    if !request.bypass_size_limits && bytes.len() > IMAGE_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    match ::image::load_from_memory(bytes) {
        Ok(image) => PreviewData::from_request(
            request,
            PreviewContent::Image {
                handle: iced_image::Handle::from_bytes(bytes.to_vec()),
                width: image.width(),
                height: image.height(),
            },
        ),
        Err(error) => {
            log::debug!("file preview image decode failed: {error}");
            info_preview_data(request, InfoReason::DecodeFailed)
        }
    }
}

fn vtf_preview_data(request: &PreviewRequest, bytes: &[u8]) -> PreviewData {
    if !request.bypass_size_limits && bytes.len() > IMAGE_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    let context = format!("vtf {}", request.entry_path);
    match catch_asset_decode(&context, || {
        crate::backend::materials::decode_vtf_rgba(bytes)
    }) {
        Some(Ok(decoded)) => PreviewData::from_request(
            request,
            PreviewContent::Image {
                handle: iced_image::Handle::from_rgba(decoded.width, decoded.height, decoded.rgba),
                width: decoded.width,
                height: decoded.height,
            },
        ),
        Some(Err(error)) => {
            log::debug!("file preview vtf decode failed: {error}");
            info_preview_data(request, InfoReason::DecodeFailed)
        }
        None => info_preview_data(request, InfoReason::DecodeFailed),
    }
}

fn audio_preview_data(request: &PreviewRequest, bytes: &[u8]) -> PreviewData {
    if !request.bypass_size_limits && bytes.len() > AUDIO_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    let bytes = std::sync::Arc::new(bytes.to_vec());
    match audio_duration_secs(std::sync::Arc::clone(&bytes)) {
        Ok(duration_secs) => PreviewData::from_request(
            request,
            PreviewContent::Audio {
                bytes,
                duration_secs,
            },
        ),
        Err(error) => {
            log::debug!("file preview audio decode failed: {error}");
            info_preview_data(request, InfoReason::DecodeFailed)
        }
    }
}

fn audio_duration_secs(
    bytes: std::sync::Arc<Vec<u8>>,
) -> Result<Option<f32>, rodio::decoder::DecoderError> {
    super::side_effects_audio::decoder_from_audio_bytes(bytes).map(|decoder| {
        decoder
            .total_duration()
            .map(|duration| duration.as_secs_f32())
    })
}

fn particle_preview_data(
    request: &PreviewRequest,
    bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
    emit_stage: &mut impl FnMut(PreviewLoadStage),
) -> PreviewData {
    use gmpublished_backend::particles::ParticleEngine;
    use gmpublished_backend::scene::pcf;

    if !request.bypass_size_limits && bytes.len() > PARTICLE_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }
    let file = match pcf::parse_pcf(bytes) {
        Ok(file) => file,
        Err(error) => {
            log::debug!(
                "file preview particle parse failed for {}: {error}",
                request.entry_path
            );
            return info_preview_data(request, InfoReason::DecodeFailed);
        }
    };
    if file.systems.is_empty() {
        return info_preview_data(request, InfoReason::DecodeFailed);
    }

    let systems: Vec<ParticleSystemInfo> = (0..file.systems.len())
        .filter_map(|index| {
            let engine = ParticleEngine::new(&file, index, 0)?;
            Some(ParticleSystemInfo {
                name: file.systems[index].name.clone(),
                coverage: engine.coverage_summary(),
                highest_control_point: engine.highest_control_point(),
            })
        })
        .collect();
    if systems.is_empty() {
        return info_preview_data(request, InfoReason::DecodeFailed);
    }

    emit_stage(PreviewLoadStage::ResolvingMaterials);
    let mut material_names: Vec<String> = file
        .systems
        .iter()
        .filter_map(|system| system.material())
        .map(normalize_particle_material)
        .filter(|name| !name.is_empty())
        .collect();
    material_names.sort();
    material_names.dedup();

    let resolver =
        MaterialResolver::new(request.archive.clone(), gmod_dir).with_bc_textures_disabled();
    let materials = material_names
        .into_iter()
        .map(|name| {
            let resolved = resolver.resolve_primary(&[], &name);
            let sheet = resolved
                .as_ref()
                .and_then(|_| particle_sprite_sheet(&resolver, &name));
            ParticleMaterialSlot {
                name,
                texture: resolved
                    .as_ref()
                    .map(|material| Arc::clone(&material.texture)),
                additive: resolved
                    .as_ref()
                    .is_some_and(|material| material.render_mode == RenderMode::Additive),
                sheet,
            }
        })
        .collect();

    PreviewData::from_request(
        request,
        PreviewContent::Particle(Arc::new(ParticlePreview {
            file,
            systems,
            materials,
        })),
    )
}

fn particle_sprite_sheet(
    resolver: &MaterialResolver,
    material: &str,
) -> Option<Arc<vformats::vtf::SpriteSheet>> {
    let vmt_bytes = resolver.entry_bytes(&format!("materials/{material}.vmt"))?;
    let vmt_text = String::from_utf8_lossy(&vmt_bytes);
    let document = vformats::vmt::parse(&vmt_text, &vformats::Limits::default()).ok()?;
    let base = document.basetexture()?;
    let vtf_bytes = resolver.entry_bytes(&format!("materials/{base}.vtf"))?;
    let texture = vformats::vtf::parse(&vtf_bytes, &vformats::Limits::default()).ok()?;
    texture
        .sprite_sheet(&vformats::Limits::default())
        .map(Arc::new)
}

fn model_preview_data(
    request: &PreviewRequest,
    mdl_bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
) -> PreviewData {
    let Some(stem) = entry_stem(&request.entry_path) else {
        return info_preview_data(request, InfoReason::DecodeFailed);
    };
    let Some((vvd_bytes, vtx_bytes)) = load_model_companions(stem, "file preview model", |path| {
        request.archive.entry_bytes(path).ok()
    }) else {
        return info_preview_data(request, InfoReason::DecodeFailed);
    };

    let Some(model) = load_model_catching_panic(
        &format!("model {}", request.entry_path),
        mdl_bytes,
        &vvd_bytes,
        &vtx_bytes,
    ) else {
        return info_preview_data(request, InfoReason::DecodeFailed);
    };

    let resolver = MaterialResolver::new(request.archive.clone(), gmod_dir);
    let mut resolved_material_count = 0_u32;
    let mut materials = model
        .material_names
        .iter()
        .map(|name| {
            let resolved = resolver.resolve_primary(&model.material_dirs, name);
            let texture = resolved
                .as_ref()
                .map(|material| Arc::clone(&material.texture));
            if texture.is_some() {
                resolved_material_count = resolved_material_count.saturating_add(1);
            }
            MaterialSlot {
                name: name.clone(),
                texture,
                texture2: None,
                force_opaque: resolved
                    .as_ref()
                    .is_none_or(|material| material.force_opaque),
                render_mode: resolved
                    .as_ref()
                    .map_or(RenderMode::Opaque, |material| material.render_mode),
            }
        })
        .collect::<Vec<_>>();
    let phy_debug_meshes = model_phy_debug_mesh(stem, &resolver, materials.len());
    if phy_debug_meshes.is_some() {
        materials.push(MaterialSlot {
            name: PHY_DEBUG_MATERIAL_NAME.to_owned(),
            texture: None,
            texture2: None,
            force_opaque: false,
            render_mode: RenderMode::Translucent,
        });
    }

    PreviewData::from_request(
        request,
        PreviewContent::Model(std::sync::Arc::new(ModelPreview {
            stats: ModelStats {
                bone_count: model.bone_count,
                sequence_count: model.sequence_count,
                vertex_count: model.vertex_count,
                triangle_count: model.triangle_count,
                mesh_count: u32::try_from(model.meshes.len()).unwrap_or(u32::MAX),
                material_count: u32::try_from(model.material_names.len()).unwrap_or(u32::MAX),
                resolved_material_count,
            },
            meshes: model.meshes,
            mesh_visibility: Vec::new(),
            map_skybox_meshes: Vec::new(),
            materials,
            lightmap: None,
            skybox: None,
            detail_sprites: Vec::new(),
            map_skybox_detail_sprites: Vec::new(),
            overlays: Vec::new(),
            map_skybox_overlays: Vec::new(),
            doors: Vec::new(),
            phy_debug_meshes: phy_debug_meshes.into_iter().collect(),
            skin_tables: model.skin_tables,
            bodygroups: model.bodygroups,
            bounds_min: model.bounds_min,
            bounds_max: model.bounds_max,
            visibility: None,
            walk_collision: None,
        })),
    )
}

/// VVD + VTX companion bytes for a `.mdl` stem, trying VTX variants in
/// order (`.dx90.vtx`, then `.dx80.vtx`, then plain `.vtx`). `log_context`
/// names the caller for the "missing companion" debug logs.
fn load_model_companions(
    stem: &str,
    log_context: &str,
    entry_bytes: impl Fn(&str) -> Option<Vec<u8>>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let vvd_path = format!("{stem}.vvd");
    let vtx_dx90_path = format!("{stem}.dx90.vtx");
    let vtx_dx80_path = format!("{stem}.dx80.vtx");
    let vtx_path = format!("{stem}.vtx");

    let Some(vvd_bytes) = entry_bytes(&vvd_path) else {
        log::debug!("{log_context} missing companion {vvd_path}");
        return None;
    };
    let Some(vtx_bytes) = entry_bytes(&vtx_dx90_path)
        .or_else(|| entry_bytes(&vtx_dx80_path))
        .or_else(|| entry_bytes(&vtx_path))
    else {
        log::debug!(
            "{log_context} missing companion {vtx_dx90_path}, {vtx_dx80_path}, or {vtx_path}"
        );
        return None;
    };
    Some((vvd_bytes, vtx_bytes))
}

fn model_phy_debug_mesh(
    stem: &str,
    resolver: &MaterialResolver,
    material_index: usize,
) -> Option<MeshData> {
    let phy_path = format!("{stem}.phy");
    let bytes = resolver.entry_bytes(&phy_path)?;
    let physics = parse_phy_bytes(&bytes, &format!("model preview physics {phy_path}")).ok()?;
    let mut builder = PhyDebugMeshBuilder::default();
    builder.push_loaded_phy(&physics, |position| position);
    if builder.truncated {
        log::debug!(
            "model preview .phy debug mesh truncated at {PHY_DEBUG_TRIANGLE_CAP} triangles"
        );
    }
    builder.finish(material_index)
}

fn load_model_catching_panic(
    context: &str,
    mdl_bytes: &[u8],
    vvd_bytes: &[u8],
    vtx_bytes: &[u8],
) -> Option<ModelData> {
    match catch_asset_decode(context, || {
        gmpublished_backend::scene::model::load_model(mdl_bytes, vvd_bytes, vtx_bytes)
    }) {
        Some(Ok(model)) => Some(ModelData::from(model)),
        Some(Err(error)) => {
            log::debug!("{context} decode failed: {error}");
            None
        }
        None => None,
    }
}

fn info_preview_data(request: &PreviewRequest, reason: InfoReason) -> PreviewData {
    PreviewData::from_request(request, PreviewContent::Info { reason })
}

fn entry_stem(path: &str) -> Option<&str> {
    path.rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
}

#[cfg(test)]
mod tests;
