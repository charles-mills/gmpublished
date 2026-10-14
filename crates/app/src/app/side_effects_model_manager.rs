use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gmpublished_backend::{model_compile, model_decompile, model_project};

use super::{App, RootMessage, Task, file_preview, model_manager};
use crate::bridge::archive::PreviewArchiveSource;
use crate::bridge::content_path::normalize_archive_path;
use crate::generation::Generation;
use crate::media::preview_model::PreviewRequest;

impl App {
    pub(super) fn apply_model_manager_message(
        &mut self,
        message: model_manager::Message,
    ) -> Task<RootMessage> {
        if let model_manager::Message::Viewer(message) = message {
            let skin_selected = matches!(message, file_preview::Message::SkinSelected(_));
            let task = self.apply_model_preview_message(message);
            self.capture_model_stage_selection(skin_selected);
            self.sync_model_workspace();
            return task;
        }
        if matches!(
            message,
            model_manager::Message::PreviewRequested
                | model_manager::Message::ApplyMaterialTint
                | model_manager::Message::ApplyReplacementMaterial
                | model_manager::Message::MaterialReset
                | model_manager::Message::SkinTextureSelected(..)
                | model_manager::Message::StageRequested
                | model_manager::Message::InstallRequested
                | model_manager::Message::SaveQcRequested
                | model_manager::Message::ExportRequested
                | model_manager::Message::BlenderExportRequested
        ) {
            let desired_selection = self
                .state
                .features
                .model_manager
                .preview_skin_materials()
                .is_some()
                .then(|| self.state.features.model_manager.preview_selection())
                .or_else(|| {
                    self.state
                        .features
                        .model_manager
                        .preview_dirty()
                        .then(|| self.state.features.model_manager.preview_selection())
                });
            self.capture_model_stage();
            if let Some((skin, bodygroups)) = desired_selection {
                let message = if self
                    .state
                    .features
                    .model_preview
                    .staged_placement()
                    .is_some()
                    && let Some((position, angles)) =
                        self.state.features.model_manager.saved_stage_placement()
                {
                    model_manager::Message::StageSnapshotCaptured {
                        position,
                        angles,
                        skin,
                        bodygroups,
                    }
                } else {
                    model_manager::Message::PreviewSelectionCaptured { skin, bodygroups }
                };
                model_manager::update(&mut self.state.features.model_manager, message);
            }
        }
        let old_source = self
            .state
            .features
            .model_manager
            .source_path()
            .map(Path::to_path_buf);
        let skin_selected = matches!(message, model_manager::Message::SkinSelected(_));
        let inspected = matches!(message, model_manager::Message::Inspected(_, Ok(_)));
        let effects = model_manager::update(&mut self.state.features.model_manager, message);
        let mut tasks = Vec::new();
        if self.state.features.model_manager.source_path() != old_source.as_deref() {
            tasks.push(self.apply_model_preview_message(file_preview::Message::CloseFinished));
        }
        if skin_selected {
            let (skin, _) = self.state.features.model_manager.preview_selection();
            tasks.push(self.apply_model_preview_message(file_preview::Message::SkinSelected(skin)));
        }
        tasks.push(self.batch_effects(effects, Self::run_model_manager_effect));
        if inspected && self.state.features.model_manager.needs_initial_preview() {
            tasks.push(self.apply_model_manager_message(model_manager::Message::PreviewRequested));
        }
        self.sync_model_workspace();
        Task::batch(tasks)
    }

    pub(super) fn capture_model_stage(&mut self) {
        self.capture_model_stage_selection(false);
    }

    fn capture_model_stage_selection(&mut self, skin_selected: bool) {
        if !self.state.features.model_preview.is_open()
            || self.state.features.model_preview.current_model().is_none()
        {
            return;
        }
        let skin = if !skin_selected
            && self
                .state
                .features
                .model_manager
                .preview_skin_materials()
                .is_some()
        {
            self.state.features.model_manager.preview_selection().0
        } else {
            self.state.features.model_preview.selected_skin()
        };
        if let Some((position, angles)) = self.state.features.model_preview.staged_placement() {
            let displayed_map = self
                .state
                .features
                .model_preview
                .request()
                .and_then(|request| request.archive.entry_disk_path(&request.entry_path));
            if displayed_map != self.state.features.model_manager.selected_map_path() {
                return;
            }
            let bodygroups = self
                .state
                .features
                .model_preview
                .bodygroup_choices()
                .to_vec();
            model_manager::update(
                &mut self.state.features.model_manager,
                model_manager::Message::StageSnapshotCaptured {
                    position,
                    angles,
                    skin,
                    bodygroups,
                },
            );
        } else if self.state.features.shell.route() == super::shell::Route::Models {
            let bodygroups = self
                .state
                .features
                .model_preview
                .bodygroup_choices()
                .to_vec();
            model_manager::update(
                &mut self.state.features.model_manager,
                model_manager::Message::PreviewSelectionCaptured { skin, bodygroups },
            );
        }
    }

    pub(super) fn run_model_manager_effect(
        &mut self,
        effect: model_manager::Effect,
    ) -> Task<RootMessage> {
        use model_manager::{Effect, Message};
        match effect {
            Effect::PickCollisionSource(generation, directory) => {
                let title = self.state.i18n.tr("models-collision-custom-file");
                Task::future(async move {
                    let mut dialog = rfd::AsyncFileDialog::new().set_title(title).add_filter(
                        "SMD, DMX, FBX, glTF, GLB",
                        &["smd", "dmx", "fbx", "gltf", "glb"],
                    );
                    if let Some(directory) = directory {
                        dialog = dialog.set_directory(directory);
                    }
                    let path = dialog
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::CollisionSourceSelected(generation, path))
                })
            }
            Effect::InspectCollision(generation, source) => self
                .environment
                .ctx
                .run_blocking("model-collision-inspect", move |_| {
                    model_compile::inspect_model(&source)
                        .map(Box::new)
                        .map_err(|error| error.to_string())
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::CollisionSourceInspected(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::PickLodSource(generation, directory) => {
                let title = self.state.i18n.tr("models-lod-source");
                Task::future(async move {
                    let mut dialog = rfd::AsyncFileDialog::new().set_title(title).add_filter(
                        "SMD, DMX, FBX, glTF, GLB",
                        &["smd", "dmx", "fbx", "gltf", "glb"],
                    );
                    if let Some(directory) = directory {
                        dialog = dialog.set_directory(directory);
                    }
                    let path = dialog
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::LodSourceSelected(generation, path))
                })
            }
            Effect::PickSource(generation) => {
                let title = self.state.i18n.tr("models-dialog-source");
                let filter = self.state.i18n.tr("models");
                Task::future(async move {
                    let path = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .add_filter(
                            filter,
                            &["qc", "toml", "smd", "dmx", "fbx", "gltf", "glb", "mdl"],
                        )
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::ImportSelected(generation, path))
                })
            }
            Effect::PickDestination(generation, directory) => {
                let title = self.state.i18n.tr("models-dialog-destination");
                Task::future(async move {
                    let mut dialog = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .set_can_create_directories(true);
                    if let Some(directory) = directory {
                        dialog = dialog.set_directory(directory);
                    }
                    let path = dialog
                        .pick_folder()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::DestinationSelected(generation, path))
                })
            }
            Effect::Export(generation, request, cancel) => self
                .environment
                .ctx
                .run_blocking("model-export", move |services| {
                    let game = services.settings_and_paths_snapshot().1.gmod_dir;
                    export_model(request, &cancel, game.as_deref())
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::ExportCompleted(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::Reveal(path) => self.reveal_path_task(path),
            Effect::OpenBlender(path) => {
                let missing = self.state.i18n.tr("models-blender-missing");
                let completed_path = path.clone();
                self.environment
                    .ctx
                    .run_blocking("model-open-blender", move |_| {
                        open_in_blender(&path).map_err(|error| {
                            if error.kind() == std::io::ErrorKind::NotFound {
                                missing
                            } else {
                                error.to_string()
                            }
                        })
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::BlenderOpened(
                            completed_path.clone(),
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::Copy(text) => self.copy_text_task(text),
            Effect::Preview(generation, result) => {
                let directory = self.state.features.model_manager.preview_directory();
                self.environment
                    .ctx
                    .run_blocking("model-preview-index", move |_| {
                        let _directory = directory;
                        prepare_preview(generation, &result)
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::PreviewPrepared(
                            generation,
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::OpenPreview(request) => self.apply_model_preview_message(
                file_preview::Message::WorkspaceOpenRequested(request),
            ),
            Effect::LoadQc(generation, source) => self
                .environment
                .ctx
                .run_blocking("model-qc-load", move |_| {
                    model_compile::read_qc_text(&source).map_err(|error| error.to_string())
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::QcLoaded(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::ValidateQc(generation, source, text) => self
                .environment
                .ctx
                .run_blocking("model-qc-validate", move |_| {
                    model_compile::inspect_qc_text(&source, &text)
                        .map(Box::new)
                        .map_err(|error| error.to_string())
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::Inspected(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::Inspect(generation, source) => self
                .environment
                .ctx
                .run_blocking("model-inspect", move |_| {
                    model_compile::inspect_model(&source)
                        .map(Box::new)
                        .map_err(|error| error.to_string())
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::Inspected(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::PickQcPath(generation, source) => {
                let title = self.state.i18n.tr("models-save-qc");
                Task::future(async move {
                    let name = source
                        .as_ref()
                        .and_then(|source| source.file_stem())
                        .unwrap_or_default()
                        .to_string_lossy();
                    let path = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .add_filter("QC", &["qc"])
                        .set_file_name(format!("{name}.qc"))
                        .save_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::QcPathSelected(generation, path))
                })
            }
            Effect::SaveQc(generation, path, request) => {
                let selected = path.clone();
                self.environment
                    .ctx
                    .run_blocking("model-qc-save", move |services| {
                        let game = services.settings_and_paths_snapshot().1.gmod_dir;
                        model_project::save_model_qc(
                            &compile_request(request, game.as_deref())?,
                            &path,
                        )
                        .map_err(|error| error.to_string())
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::QcSaved(
                            generation,
                            selected.clone(),
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::PickSkinTexture(generation) => {
                let title = self.state.i18n.tr("models-skin-texture");
                Task::future(async move {
                    let path = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .add_filter(
                            "PNG, JPEG, TGA, BMP, VTF",
                            &["png", "jpg", "jpeg", "tga", "bmp", "vtf"],
                        )
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::SkinTextureSelected(generation, path))
                })
            }
            Effect::DiscoverMaps(generation) => {
                let gmod = self
                    .environment
                    .ctx
                    .settings_and_paths_snapshot()
                    .1
                    .gmod_dir;
                self.environment
                    .ctx
                    .run_blocking("model-stage-maps", move |_| {
                        gmod.as_deref()
                            .map(super::model_stage::list_maps)
                            .unwrap_or_default()
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::MapsLoaded(
                            generation,
                            result
                                .unwrap_or_default()
                                .into_iter()
                                .map(model_manager::MapChoice)
                                .collect(),
                        ))
                    })
            }
            Effect::PickMap(generation) => {
                let title = self.state.i18n.tr("models-stage-map");
                Task::future(async move {
                    let path = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .add_filter("BSP", &["bsp"])
                        .pick_file()
                        .await
                        .map(|file| file.path().to_path_buf());
                    RootMessage::ModelManager(Message::MapBrowseSelected(generation, path))
                })
            }
            Effect::DebouncePreview(revision) => Task::future(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                RootMessage::ModelManager(Message::PreviewUpdateDue(revision))
            }),
            Effect::BuildPreview(generation, request, cancel) => self
                .environment
                .ctx
                .run_blocking("model-preview-build", move |services| {
                    let game = services.settings_and_paths_snapshot().1.gmod_dir;
                    build_preview(generation, request, game.as_deref(), &cancel)
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::PreviewBuilt(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::PrepareStage(generation, result, map) => {
                let directory = self.state.features.model_manager.preview_directory();
                self.environment
                    .ctx
                    .run_blocking("model-stage-index", move |_| {
                        let _directory = directory;
                        Ok(Box::new((
                            prepare_preview(generation, &result)?,
                            super::model_stage::prepare_map_preview(generation, &map)?,
                        )))
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::StagePrepared(
                            generation,
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::OpenStage { model, map } => {
                self.apply_model_preview_message(file_preview::Message::WorkspaceStageRequested {
                    model: Box::new(model),
                    map: Box::new(map),
                })
            }
            Effect::InstallModel { generation, result } => {
                let lease = self.state.features.model_manager.preview_directory();
                let gmod = self
                    .environment
                    .ctx
                    .settings_and_paths_snapshot()
                    .1
                    .gmod_dir;
                self.environment
                    .ctx
                    .run_blocking("model-install", move |_| {
                        let _lease = lease;
                        let gmod = gmod.ok_or_else(|| "Garry's Mod was not detected".to_owned())?;
                        super::model_stage::install_model(&gmod, &result)
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::ModelInstalled(
                            generation,
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::ListInstalledModels(generation) => {
                let gmod = self
                    .environment
                    .ctx
                    .settings_and_paths_snapshot()
                    .1
                    .gmod_dir;
                self.environment
                    .ctx
                    .run_blocking("model-installed-list", move |_| {
                        let gmod = gmod.ok_or_else(|| "Garry's Mod was not detected".to_owned())?;
                        super::model_stage::list_models(&gmod)
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::InstalledModelsLoaded(
                            generation,
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::RemoveInstalledModel(generation, directory) => {
                let gmod = self
                    .environment
                    .ctx
                    .settings_and_paths_snapshot()
                    .1
                    .gmod_dir;
                let selected = directory.clone();
                self.environment
                    .ctx
                    .run_blocking("model-installed-remove", move |_| {
                        let gmod = gmod.ok_or_else(|| "Garry's Mod was not detected".to_owned())?;
                        super::model_stage::remove_model(&gmod, &directory)
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::InstalledModelRemoved(
                            generation,
                            selected.clone(),
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
        }
    }
}

fn export_model(
    request: model_manager::ExportRequest,
    cancel: &AtomicBool,
    game_directory: Option<&Path>,
) -> Result<model_manager::ExportResult, String> {
    if request.kind == model_manager::SourceKind::Compiled {
        if request.target == model_manager::ExportTarget::Blender {
            let destination = export_destination(&request.source, &request.destination, "blender");
            let resolver = std::cell::LazyCell::new(|| {
                crate::bridge::materials::MaterialResolver::new(
                    PreviewArchiveSource::from_folder(Vec::new()),
                    game_directory.map(Path::to_path_buf),
                )
            });
            let report = model_decompile::export_blender_model_with_cancel(
                &request.source,
                &destination,
                cancel,
                |path| resolver.entry_bytes(path),
            )
            .map_err(|error| error.to_string())?;
            return Ok(model_manager::ExportResult {
                directory: report.output_directory,
                files: report.files,
                bytes: report.bytes_written,
                warnings: report.warnings,
                compiled: false,
            });
        }
        let destination = export_destination(&request.source, &request.destination, "source");
        let report =
            model_decompile::decompile_model_with_cancel(&request.source, &destination, cancel)
                .map_err(|error| error.to_string())?;
        return Ok(model_manager::ExportResult {
            directory: report.output_directory,
            files: report.files,
            bytes: report.bytes_written,
            warnings: report.warnings,
            compiled: false,
        });
    }
    let destination = request.destination.clone();
    let options = compile_request(request, game_directory)?;
    let output = model_compile::compile_model_with_cancel(&options, cancel)
        .map_err(|error| error.to_string())?;
    let directory = std::fs::canonicalize(&destination).map_err(|error| error.to_string())?;
    Ok(model_manager::ExportResult {
        directory,
        bytes: output.files.iter().map(|file| file.bytes).sum(),
        files: output.files.into_iter().map(|file| file.path).collect(),
        warnings: output.warnings,
        compiled: true,
    })
}

fn compile_request(
    request: model_manager::ExportRequest,
    game_directory: Option<&Path>,
) -> Result<model_compile::ModelCompileRequest, String> {
    let load_surfaces = request.kind == model_manager::SourceKind::Description;
    let mut options = model_compile::ModelCompileRequest::new(request.source, request.destination);
    options.qc_text = request.qc_text;
    options.model_name = request.model_name;
    options.material_directory = request.material_directory;
    options.static_prop = request.static_prop;
    options.static_prop_override =
        (request.kind == model_manager::SourceKind::Mesh).then_some(request.static_prop);
    options.collision = request.collision;
    options.collision_mass = request.collision_mass;
    options.surface_property = request.surface_property;
    options.scale = request.scale;
    options.export_materials = request.export_materials;
    options.automatic_axes = request.automatic_axes;
    options.overwrite = request.overwrite;
    options.orientation = request.orientation;
    options.skins = request.skins;
    options.bodygroups = request.bodygroups;
    options.lods = request.lods;
    options.animation_settings = request.animation_settings;
    if load_surfaces && let Some(game_directory) = game_directory {
        let resolver = std::cell::LazyCell::new(|| {
            crate::bridge::materials::MaterialResolver::new(
                PreviewArchiveSource::from_folder(Vec::new()),
                Some(game_directory.to_path_buf()),
            )
        });
        model_compile::load_surface_properties(&mut options, |path| resolver.entry_bytes(path))
            .map_err(|error| error.to_string())?;
    }
    Ok(options)
}

fn build_preview(
    generation: Generation,
    mut request: model_manager::ExportRequest,
    game_directory: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<model_manager::PreviewBuild, String> {
    let directory = std::sync::Arc::new(tempfile::tempdir().map_err(|error| error.to_string())?);
    if request.kind == model_manager::SourceKind::Compiled {
        let (_, result) =
            super::model_stage::prepare_imported_model_preview(generation, &request.source)?;
        return Ok(model_manager::PreviewBuild {
            result,
            _directory: directory,
        });
    }
    request.destination = directory.path().to_path_buf();
    request.overwrite = false;
    request.export_materials = true;
    let result = export_model(request, cancel, game_directory)?;
    Ok(model_manager::PreviewBuild {
        result,
        _directory: directory,
    })
}

fn export_destination(source: &Path, root: &Path, suffix: &str) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let base = root.join(format!("{stem}_{suffix}"));
    if !base.exists() {
        return base;
    }
    for index in 2.. {
        let candidate = root.join(format!("{stem}_{suffix}_{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

const BLENDER_IMPORT_EXPRESSION: &str = r"import bpy, math, sys
bpy.ops.wm.read_factory_settings(use_empty=True)
options = {'import_scene_as_collection': True} if 'import_scene_as_collection' in bpy.ops.import_scene.gltf.get_rna_type().properties else {}
bpy.ops.import_scene.gltf(filepath=sys.argv[-1], **options)
# Blender's glTF exporter reads material variants from the first data scene.
variant_property = 'gltf2_KHR_materials_variants_variants'
variant_source = next((scene for scene in bpy.data.scenes if getattr(scene, variant_property, None)), None)
if variant_source is not None and variant_source != bpy.data.scenes[0]:
    variants = [(variant.variant_idx, variant.name) for variant in getattr(variant_source, variant_property)]
    target = getattr(bpy.data.scenes[0], variant_property)
    target.clear()
    for index, name in variants:
        variant = target.add()
        variant.variant_idx = index
        variant.name = name
frame_end = max(1, math.ceil(max((action.frame_range[1] for action in bpy.data.actions), default=1)))
for scene in bpy.data.scenes:
    scene.frame_start = 0
    scene.frame_end = frame_end
    scene.frame_set(0)
    scene['glTF2ExportSettings'] = {'export_extras': True}
if not bpy.app.background:
    for window in bpy.context.window_manager.windows:
        layer = window.view_layer
        meshes = [obj for obj in layer.objects if obj.type == 'MESH' and obj.visible_get(view_layer=layer)]
        if not meshes:
            continue
        for obj in layer.objects:
            obj.select_set(False, view_layer=layer)
        for obj in meshes:
            obj.select_set(True, view_layer=layer)
        layer.objects.active = meshes[0]
        for area in window.screen.areas:
            if area.type != 'VIEW_3D':
                continue
            region = next((region for region in area.regions if region.type == 'WINDOW'), None)
            if region is not None:
                with bpy.context.temp_override(window=window, area=area, region=region):
                    if bpy.ops.view3d.view_selected.poll():
                        bpy.ops.view3d.view_selected(use_all_regions=False)
";

fn blender_executable() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        let roots: std::collections::BTreeSet<_> =
            ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from)
                .collect();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root.join("Blender Foundation")) else {
                continue;
            };
            let entries: Vec<_> = entries.take(129).collect();
            if entries.len() > 128 {
                continue;
            }
            for entry in entries.into_iter().flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.to_ascii_lowercase().starts_with("blender") {
                    continue;
                }
                let executable = entry.path().join("blender.exe");
                if executable.is_file() {
                    let version: Vec<u32> = name
                        .split(|character: char| !character.is_ascii_digit())
                        .filter_map(|component| component.parse().ok())
                        .collect();
                    candidates.push((version, executable));
                }
            }
        }
        if let Some((_, path)) = candidates.into_iter().max() {
            return path;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let system = PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender");
        if system.is_file() {
            return system;
        }
        if let Some(home) = std::env::var_os("HOME") {
            let user = PathBuf::from(home).join("Applications/Blender.app/Contents/MacOS/Blender");
            if user.is_file() {
                return user;
            }
        }
    }
    PathBuf::from("blender")
}

fn blender_import_command(path: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(blender_executable());
    command
        .args(["--python-expr", BLENDER_IMPORT_EXPRESSION, "--"])
        .arg(path);
    command
}

fn open_in_blender(path: &Path) -> std::io::Result<()> {
    let mut child = blender_import_command(path).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn prepare_preview(
    generation: Generation,
    result: &model_manager::ExportResult,
) -> Result<PreviewRequest, String> {
    let model = result
        .files
        .iter()
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"))
        })
        .ok_or_else(|| "The export contains no model file".to_owned())?;
    let entry_path = normalize_archive_path(&relative_path(&result.directory, model)?)
        .ok_or_else(|| "The exported model has an invalid preview path".to_owned())?;
    let mut files = Vec::with_capacity(result.files.len());
    let mut checksum = crc32fast::Hasher::new();
    let mut buffer = [0_u8; 8192];
    for path in &result.files {
        let size = std::fs::metadata(path)
            .map_err(|error| error.to_string())?
            .len();
        let relative = relative_path(&result.directory, path)?;
        checksum.update(relative.as_bytes());
        checksum.update(&size.to_le_bytes());
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        loop {
            let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            checksum.update(&buffer[..count]);
        }
        files.push((relative, size, path.clone()));
    }
    let size_bytes = std::fs::metadata(model)
        .map_err(|error| error.to_string())?
        .len();
    Ok(PreviewRequest {
        request_id: generation,
        archive: PreviewArchiveSource::from_folder(files),
        entry_path,
        display_name: model
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        size_bytes,
        crc32: checksum.finalize(),
        bypass_size_limits: false,
    })
}

fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blender_filenames_are_arguments_and_never_executable_python() {
        for filename in [
            "exports/charles' model.glb",
            "C:\\Models\\\"quoted\"\\model.glb",
            "exports/车辆🦊/model.glb",
            "exports/\"); __import__('os').system('unwanted'); #.glb",
            "--python-expr=unwanted.glb",
        ] {
            let command = blender_import_command(Path::new(filename));
            let arguments: Vec<_> = command.get_args().collect();
            assert_eq!(arguments.len(), 4);
            assert_eq!(arguments[0], "--python-expr");
            assert_eq!(arguments[2], "--");
            assert_eq!(arguments[3], filename);
            assert!(
                arguments[1]
                    .to_str()
                    .unwrap()
                    .contains("filepath=sys.argv[-1]")
            );
            assert!(!arguments[1].to_str().unwrap().contains(filename));
        }
    }

    #[test]
    fn repeated_decompiles_select_new_directories() {
        let temp = tempfile::tempdir().expect("directory");
        std::fs::create_dir(temp.path().join("prop_source")).expect("first export");
        assert_eq!(
            export_destination(Path::new("prop.mdl"), temp.path(), "source"),
            temp.path().join("prop_source_2")
        );
    }

    #[test]
    fn exported_models_preview_with_their_companions_and_materials() {
        let temp = tempfile::tempdir().expect("directory");
        let model = temp
            .path()
            .join("Models/Vehicles/Mercedes+Benz+GLS+580.mdl");
        let texture = temp.path().join("Materials/Vehicles/Paint.VTF");
        for path in [&model, &texture] {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
            std::fs::write(path, b"fixture").expect("file");
        }
        let result = model_manager::ExportResult {
            directory: temp.path().into(),
            files: vec![model, texture],
            bytes: 14,
            warnings: vec![],
            compiled: true,
        };
        let request = prepare_preview(Generation::INITIAL, &result).expect("preview");
        assert_eq!(
            request.entry_path,
            "models/vehicles/mercedes+benz+gls+580.mdl"
        );
        assert_eq!(request.display_name, "Mercedes+Benz+GLS+580.mdl");
        assert_eq!(
            request.archive.entry_bytes(&request.entry_path).unwrap(),
            b"fixture"
        );
        assert!(
            request
                .archive
                .entry("materials/vehicles/paint.vtf")
                .is_ok()
        );
        assert!(!request.archive.supports_entry_extraction());
        std::fs::write(&result.files[1], b"changed").expect("edit texture with same size");
        let edited = prepare_preview(Generation::INITIAL, &result).expect("edited preview");
        assert_ne!(request.crc32, edited.crc32);
        assert_eq!(request.size_bytes, edited.size_bytes);
    }

    #[test]
    fn mixed_case_compiled_model_loads_geometry_through_export_preview() {
        use crate::media::file_preview_decode::load_preview;
        use crate::media::preview_model::PreviewContent;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("cube.smd");
        std::fs::write(
            &source,
            include_bytes!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
        )
        .unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([80, 140, 200, 255]))
            .save(temp.path().join("material.png"))
            .unwrap();
        let directory = temp.path().join("export");
        let mut compile = model_compile::ModelCompileRequest::new(source, directory.clone());
        compile.model_name = Some("Vehicles/Mercedes+Benz+GLS+580.mdl".into());
        compile.static_prop = true;
        let output = model_compile::compile_model(&compile).unwrap();
        let triangles = output.triangles;
        let result = model_manager::ExportResult {
            directory,
            bytes: output.files.iter().map(|file| file.bytes).sum(),
            files: output.files.into_iter().map(|file| file.path).collect(),
            warnings: output.warnings,
            compiled: true,
        };
        let request = prepare_preview(Generation::INITIAL, &result).unwrap();
        let cpu = gmpublished_backend::CpuExecutor::build(1).unwrap();
        let loaded = load_preview(
            &request,
            &crate::theme::Tokens::dark(),
            None,
            &cpu,
            &mut |_| {},
        )
        .unwrap();
        let PreviewContent::Model(model) = loaded.content else {
            panic!("expected model geometry");
        };
        assert_eq!(triangles, 12);
        assert_eq!(model.scene.stats.triangle_count as usize, triangles);
        assert_eq!(model.scene.stats.resolved_material_count, 1);
    }
}
