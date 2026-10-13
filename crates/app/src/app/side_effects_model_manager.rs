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
    pub(super) fn capture_model_stage(&mut self) {
        if !self.state.features.file_preview.is_open() {
            return;
        }
        if let Some((position, angles)) = self.state.features.file_preview.staged_placement() {
            let skin = self.state.features.file_preview.selected_skin();
            let bodygroups = self
                .state
                .features
                .file_preview
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
            let skin = self.state.features.file_preview.selected_skin();
            let bodygroups = self
                .state
                .features
                .file_preview
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
            Effect::PickLodSource(generation, directory) => {
                let title = self.state.i18n.tr("models-lod-source");
                Task::future(async move {
                    let mut dialog = rfd::AsyncFileDialog::new()
                        .set_title(title)
                        .add_filter("SMD, FBX, glTF, GLB", &["smd", "fbx", "gltf", "glb"]);
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
                        .add_filter(filter, &["qc", "toml", "smd", "fbx", "gltf", "glb", "mdl"])
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
                .run_blocking("model-export", move |_| export_model(request, &cancel))
                .map(move |result| {
                    RootMessage::ModelManager(Message::ExportCompleted(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::Reveal(path) => self.reveal_path_task(path),
            Effect::Copy(text) => self.copy_text_task(text),
            Effect::Preview(generation, result) => self
                .environment
                .ctx
                .run_blocking("model-preview-index", move |_| {
                    prepare_preview(generation, &result)
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::PreviewPrepared(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::OpenPreview(request) => {
                self.apply_file_preview_message(file_preview::Message::OpenRequested(request))
            }
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
                    model_compile::inspect_model(&source).map_err(|error| error.to_string())
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
                    .run_blocking("model-qc-save", move |_| {
                        model_project::save_model_qc(&compile_request(request), &path)
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
            Effect::BuildPreview(generation, request) => self
                .environment
                .ctx
                .run_blocking("model-preview-build", move |_| {
                    build_preview(generation, request)
                })
                .map(move |result| {
                    RootMessage::ModelManager(Message::PreviewBuilt(
                        generation,
                        result.unwrap_or_else(|error| Err(error.to_string())),
                    ))
                }),
            Effect::PrepareStage(generation, result, map) => self
                .environment
                .ctx
                .run_blocking("model-stage-index", move |_| {
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
                }),
            Effect::OpenStage { model, map } => {
                self.apply_file_preview_message(file_preview::Message::StageRequested {
                    model: Box::new(model),
                    map: Box::new(map),
                })
            }
            Effect::TestInGame {
                generation,
                result,
                map,
                skin,
                bodygroups,
                placement,
            } => {
                let gmod = self
                    .environment
                    .ctx
                    .settings_and_paths_snapshot()
                    .1
                    .gmod_dir;
                self.environment
                    .ctx
                    .run_blocking("model-engine-test", move |_| {
                        let gmod = gmod.ok_or_else(|| "Garry's Mod was not detected".to_owned())?;
                        let map = map.file_stem().unwrap_or_default().to_string_lossy();
                        let placement = placement.map(|stage| (stage.position, stage.angles));
                        super::model_stage::prepare_engine_test(
                            &gmod,
                            &result,
                            &map,
                            skin,
                            &bodygroups,
                            placement,
                        )
                        .map(|launch| model_manager::EngineTestResult {
                            url: launch.url,
                            addon_directory: launch.addon_directory,
                            console_command: launch.console_command,
                        })
                    })
                    .map(move |result| {
                        RootMessage::ModelManager(Message::EngineTestPrepared(
                            generation,
                            result.unwrap_or_else(|error| Err(error.to_string())),
                        ))
                    })
            }
            Effect::LaunchGame(url) => self.open_url_task(url),
        }
    }
}

fn export_model(
    request: model_manager::ExportRequest,
    cancel: &AtomicBool,
) -> Result<model_manager::ExportResult, String> {
    if request.kind == model_manager::SourceKind::Compiled {
        let destination = decompile_destination(&request.source, &request.destination);
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
    let options = compile_request(request);
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

fn compile_request(request: model_manager::ExportRequest) -> model_compile::ModelCompileRequest {
    let mut options = model_compile::ModelCompileRequest::new(request.source, request.destination);
    options.qc_text = request.qc_text;
    options.model_name = request.model_name;
    options.material_directory = request.material_directory;
    options.static_prop = request.static_prop;
    options.generate_collision = request.generate_collision;
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
    options
}

fn build_preview(
    generation: Generation,
    mut request: model_manager::ExportRequest,
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
    let result = export_model(request, &AtomicBool::new(false))?;
    Ok(model_manager::PreviewBuild {
        result,
        _directory: directory,
    })
}

fn decompile_destination(source: &Path, root: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let base = root.join(format!("{stem}_source"));
    if !base.exists() {
        return base;
    }
    for index in 2.. {
        let candidate = root.join(format!("{stem}_source_{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
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
    fn repeated_decompiles_select_new_directories() {
        let temp = tempfile::tempdir().expect("directory");
        std::fs::create_dir(temp.path().join("prop_source")).expect("first export");
        assert_eq!(
            decompile_destination(Path::new("prop.mdl"), temp.path()),
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
