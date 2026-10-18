use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use gmpublished_backend::model_compile::{
    AnimationEvent, AnimationEventView, AnimationSettings, AnimationSettingsView, ModelBodygroup,
    ModelCollision, ModelInspection,
};
use gmpublished_backend::model_project::{ModelSkin, ModelStage};

use crate::generation::Generation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Description,
    Mesh,
    Compiled,
}

impl SourceKind {
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?;
        if ["qc", "toml"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
        {
            Some(Self::Description)
        } else if ["smd", "dmx", "fbx", "gltf", "glb"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
        {
            Some(Self::Mesh)
        } else if extension.eq_ignore_ascii_case("mdl") {
            Some(Self::Compiled)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceTab {
    #[default]
    Model,
    Appearance,
    Physics,
    Export,
    Import,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExportTarget {
    #[default]
    Source,
    Blender,
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "compile options are independent toggles"
)]
pub struct ExportRequest {
    pub target: ExportTarget,
    pub qc_text: Option<String>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub kind: SourceKind,
    pub model_name: Option<String>,
    pub material_directory: Option<String>,
    pub scale: f32,
    pub static_prop: bool,
    pub collision: ModelCollision,
    pub collision_mass: f32,
    pub surface_property: Option<String>,
    pub export_materials: bool,
    pub automatic_axes: bool,
    pub overwrite: bool,
    pub orientation: [f32; 3],
    pub skins: Vec<ModelSkin>,
    pub bodygroups: Vec<ModelBodygroup>,
    pub lods: Vec<gmpublished_backend::model_compile::ModelLod>,
    pub animation_settings: Vec<AnimationSettings>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportResult {
    pub directory: PathBuf,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    pub warnings: Vec<String>,
    pub compiled: bool,
}

impl ExportResult {
    pub fn blender_model(&self) -> Option<&Path> {
        self.files.iter().find_map(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
                .then_some(path.as_path())
        })
    }
}

#[derive(Clone, Debug)]
pub struct PreviewBuild {
    pub result: ExportResult,
    pub _directory: Option<Arc<tempfile::TempDir>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledModel {
    pub directory: PathBuf,
    pub name: String,
    pub model: String,
    pub bytes: u64,
    pub modified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapChoice(pub PathBuf);

impl std::fmt::Display for MapChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AnimationEventEditor {
    pub frame: String,
    pub event: String,
    pub options: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AnimationEditor {
    pub source: Option<String>,
    pub included: bool,
    pub name: String,
    pub fps: String,
    pub looping: bool,
    pub frame_count: usize,
    pub activity: String,
    pub activity_weight: String,
    pub events: Vec<AnimationEventEditor>,
    pub advanced_expanded: bool,
}

impl AnimationEditor {
    fn view(
        &self,
    ) -> AnimationSettingsView<'_, impl ExactSizeIterator<Item = AnimationEventView<'_>>> {
        let activity = self
            .included
            .then_some(self.activity.trim())
            .filter(|value| !value.is_empty());
        AnimationSettingsView {
            source: self.source.as_deref(),
            included: self.included,
            name: &self.name,
            fps: if self.included {
                self.fps.trim().parse().ok()
            } else {
                Some(30.0)
            },
            frame_count: self.frame_count,
            activity,
            activity_weight: if activity.is_some() {
                self.activity_weight.trim().parse().ok()
            } else {
                Some(0)
            },
            events: self.events.iter().map(|event| AnimationEventView {
                frame: event.frame.trim().parse().ok(),
                event: event.event.trim(),
                options: &event.options,
            }),
        }
    }
}

impl From<&AnimationSettings> for AnimationEditor {
    fn from(value: &AnimationSettings) -> Self {
        Self {
            source: value.source.clone(),
            included: value.included,
            name: value.name.clone(),
            fps: value.fps.to_string(),
            looping: value.looping,
            frame_count: value.frame_count,
            activity: value.activity.clone().unwrap_or_default(),
            activity_weight: if value.activity.is_some() {
                value.activity_weight.to_string()
            } else {
                "1".into()
            },
            events: value
                .events
                .iter()
                .map(|event| AnimationEventEditor {
                    frame: event.frame.to_string(),
                    event: event.event.clone(),
                    options: event.options.clone(),
                })
                .collect(),
            advanced_expanded: false,
        }
    }
}

#[derive(Debug)]
pub(super) struct RunningExport {
    pub generation: Generation,
    pub cancel: Arc<AtomicBool>,
    pub cancellation_requested: bool,
    pub target: ExportTarget,
}

#[derive(Debug)]
pub(super) struct PreviewJob {
    pub generation: Generation,
    pub revision: Generation,
    pub export_revision: Generation,
    pub request: ExportRequest,
    pub cancel: Arc<AtomicBool>,
    pub replaced_preview: bool,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "compile options are independent toggles"
)]
pub struct State {
    pub(super) workspace_tab: WorkspaceTab,
    pub(super) pane_open: bool,
    pub(super) advanced_open: bool,
    pub(super) preview_dirty: bool,
    pub(super) preview_job: Option<PreviewJob>,
    pub(super) preview_revision: Generation,
    pub(super) export_revision: Generation,
    pub(super) preview_export_dirty: bool,
    pub(super) preview_due: Option<Generation>,
    pub(super) preview_pending: bool,
    pub(super) displayed_transform: Option<(f32, [f32; 3])>,
    pub(super) install_pending: bool,
    pub(super) route_active: bool,
    pub(super) previous_preview: Option<PreviewBuild>,
    pub(super) qc_editor: super::qc_editor::State,
    pub(super) source: Option<PathBuf>,
    pub(super) destination: Option<PathBuf>,
    pub(super) model_name: String,
    pub(super) material_directory: String,
    pub(super) scale: String,
    pub(super) static_prop: bool,
    pub(super) model_type_edited: bool,
    pub(super) collision: super::collision::Editor,
    pub(super) collision_mass: String,
    pub(super) surface_property: String,
    pub(super) export_materials: bool,
    pub(super) automatic_axes: bool,
    pub(super) overwrite: bool,
    pub(super) generation: Generation,
    pub(super) picker: Option<Generation>,
    pub(super) running: Option<RunningExport>,
    pub(super) result: Option<ExportResult>,
    pub(super) error: Option<String>,
    pub(super) blender_launch_error: Option<String>,
    pub(super) cancelled: bool,
    pub(super) inspection: Option<ModelInspection>,
    pub(super) inspection_generation: Option<Generation>,
    pub(super) skins: Vec<ModelSkin>,
    pub(super) bodygroups: Vec<ModelBodygroup>,
    pub(super) lods: Vec<super::lods::LodEditor>,
    pub(super) bodygroup_editor: Option<(usize, usize)>,
    pub(super) bodygroup_filter: String,
    pub(super) animations: Vec<AnimationEditor>,
    pub(super) selected_skin: Option<usize>,
    pub(super) selected_material: Option<String>,
    pub(super) material_tint: String,
    pub(super) replacement_material: String,
    pub(super) orientation: [String; 3],
    pub(super) qc_path: Option<PathBuf>,
    pub(super) qc_saved: bool,
    pub(super) maps: Vec<MapChoice>,
    pub(super) map_generation: Option<Generation>,
    pub(super) selected_map: Option<MapChoice>,
    pub(super) stage: Option<ModelStage>,
    pub(super) preview: Option<PreviewBuild>,
    pub(super) preview_for_stage: bool,
    pub(super) install_confirmation: bool,
    pub(super) install_generation: Option<Generation>,
    pub(super) install_result: Option<InstalledModel>,
    pub(super) install_error: Option<String>,
    pub(super) installed_models: Vec<InstalledModel>,
    pub(super) installed_models_generation: Option<Generation>,
    pub(super) installed_models_error: Option<String>,
    pub(super) remove_installed_confirmation: Option<PathBuf>,
    pub(super) remove_installed_generation: Option<(Generation, PathBuf)>,
    pub(super) stage_skin: usize,
    pub(super) stage_bodygroups: Vec<usize>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            workspace_tab: WorkspaceTab::Model,
            pane_open: true,
            advanced_open: false,
            preview_dirty: false,
            preview_job: None,
            preview_revision: Generation::INITIAL,
            export_revision: Generation::INITIAL,
            preview_export_dirty: false,
            preview_due: None,
            preview_pending: false,
            displayed_transform: None,
            install_pending: false,
            route_active: true,
            previous_preview: None,
            qc_editor: super::qc_editor::State::default(),
            source: None,
            destination: None,
            model_name: String::new(),
            material_directory: String::new(),
            scale: "1".into(),
            static_prop: false,
            model_type_edited: false,
            collision: super::collision::Editor::default(),
            collision_mass: "1".into(),
            surface_property: String::new(),
            export_materials: true,
            automatic_axes: true,
            overwrite: false,
            generation: Generation::INITIAL,
            picker: None,
            running: None,
            result: None,
            error: None,
            blender_launch_error: None,
            cancelled: false,
            inspection: None,
            inspection_generation: None,
            skins: vec![],
            bodygroups: vec![],
            lods: vec![],
            bodygroup_editor: None,
            bodygroup_filter: String::new(),
            animations: vec![],
            selected_skin: None,
            selected_material: None,
            material_tint: "#ffffff".into(),
            replacement_material: String::new(),
            orientation: std::array::from_fn(|_| "0".into()),
            qc_path: None,
            qc_saved: false,
            maps: vec![],
            map_generation: None,
            selected_map: None,
            stage: None,
            preview: None,
            preview_for_stage: false,
            install_confirmation: false,
            install_generation: None,
            install_result: None,
            install_error: None,
            installed_models: vec![],
            installed_models_generation: None,
            installed_models_error: None,
            remove_installed_confirmation: None,
            remove_installed_generation: None,
            stage_skin: 0,
            stage_bodygroups: vec![],
        }
    }
}

impl State {
    pub(super) fn install_busy(&self) -> bool {
        self.install_pending || self.install_generation.is_some()
    }

    pub(super) fn installed_mutation_busy(&self) -> bool {
        self.install_busy() || self.remove_installed_generation.is_some()
    }

    pub fn needs_initial_preview(&self) -> bool {
        self.route_active
            && self.preview.is_none()
            && !self.preview_busy()
            && self.inspection.is_some()
            && self.can_preview()
    }

    pub fn preview_dirty(&self) -> bool {
        self.preview_dirty
    }

    pub fn preview_busy(&self) -> bool {
        self.preview_job.is_some()
    }

    pub fn preview_job_generation(&self) -> Option<Generation> {
        self.preview_job.as_ref().map(|job| job.generation)
    }

    pub fn preview_accepts_presentation(&self) -> bool {
        self.preview_job.as_ref().is_some_and(|job| {
            job.revision == self.preview_revision
                && !job.cancel.load(std::sync::atomic::Ordering::Acquire)
        })
    }

    pub fn preview_directory(&self) -> Option<Arc<tempfile::TempDir>> {
        self.preview
            .as_ref()
            .and_then(|preview| preview._directory.as_ref().map(Arc::clone))
    }

    pub fn displayed_preview_transform(&self) -> Option<(f32, [f32; 3])> {
        self.displayed_transform
    }

    pub fn desired_preview_transform(&self) -> Option<(f32, [f32; 3])> {
        if self.kind() != Some(SourceKind::Mesh) {
            return None;
        }
        Some((self.parsed_scale()?, self.parsed_orientation()?))
    }

    pub fn selected_map_path(&self) -> Option<&Path> {
        self.selected_map.as_ref().map(|map| map.0.as_path())
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn saved_stage_placement(&self) -> Option<([f32; 3], [f32; 3])> {
        self.stage
            .as_ref()
            .map(|stage| (stage.position, stage.angles))
    }

    pub fn preview_selection(&self) -> (usize, Vec<usize>) {
        (self.stage_skin, self.stage_bodygroups.clone())
    }

    pub fn kind(&self) -> Option<SourceKind> {
        self.source.as_deref().and_then(SourceKind::of)
    }

    pub fn busy(&self) -> bool {
        self.running.is_some() || self.picker.is_some()
    }

    pub fn can_export(&self) -> bool {
        !self.busy()
            && !self.preview_busy()
            && self.destination.is_some()
            && self.request_options_valid()
    }

    pub(super) fn can_preview(&self) -> bool {
        !self.busy() && self.request_options_valid()
    }

    pub(super) fn can_save_qc(&self) -> bool {
        !self.busy() && self.kind() != Some(SourceKind::Compiled) && self.request_options_valid()
    }

    fn request_options_valid(&self) -> bool {
        self.request_fields_valid() && self.animation_settings_valid() && self.lods_valid()
    }

    fn request_fields_valid(&self) -> bool {
        self.kind().is_some()
            && self.qc_editor_ready()
            && (self.kind() != Some(SourceKind::Mesh) || self.parsed_scale().is_some())
            && self.parsed_orientation().is_some()
            && self.physics_options_valid()
            && self.bodygroups_valid()
    }

    pub(super) fn parsed_scale(&self) -> Option<f32> {
        self.scale
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    }

    pub(super) fn parsed_collision_mass(&self) -> Option<f32> {
        self.collision_mass
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|mass| mass.is_finite() && *mass > 0.0)
    }

    pub(super) fn surface_property_valid(&self) -> bool {
        self.surface_property
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }

    fn physics_options_valid(&self) -> bool {
        self.kind() != Some(SourceKind::Mesh)
            || (self.surface_property_valid()
                && self.collision_valid()
                && (!self.collision_generates_physics() || self.parsed_collision_mass().is_some()))
    }

    pub(super) fn request(&self) -> Option<ExportRequest> {
        if self.busy() || self.preview_busy() {
            return None;
        }
        self.build_request(self.destination.clone()?)
    }

    pub(super) fn preview_request(&self) -> Option<ExportRequest> {
        if self.busy() {
            return None;
        }
        self.build_request(self.destination.clone().unwrap_or_default())
    }

    fn build_request(&self, destination: PathBuf) -> Option<ExportRequest> {
        if !self.request_fields_valid() {
            return None;
        }
        let mesh = self.kind() == Some(SourceKind::Mesh);
        Some(ExportRequest {
            target: ExportTarget::Source,
            qc_text: self.qc_editor.text(),
            source: self.source.clone()?,
            destination,
            kind: self.kind()?,
            model_name: optional_text(&self.model_name),
            material_directory: optional_text(&self.material_directory),
            scale: if self.kind() == Some(SourceKind::Mesh) {
                self.parsed_scale()?
            } else {
                1.0
            },
            static_prop: self.static_prop,
            collision: self.collision_request()?,
            collision_mass: if mesh && self.collision_generates_physics() {
                self.parsed_collision_mass()?
            } else {
                1.0
            },
            surface_property: if mesh {
                optional_text(&self.surface_property)
            } else {
                None
            },
            export_materials: self.export_materials,
            automatic_axes: self.automatic_axes,
            overwrite: self.overwrite,
            orientation: self.parsed_orientation()?,
            skins: self.skins.clone(),
            bodygroups: self.bodygroups.clone(),
            lods: self.parsed_lods()?,
            animation_settings: self.parsed_animation_settings()?,
        })
    }

    pub(super) fn parsed_animation_settings(&self) -> Option<Vec<AnimationSettings>> {
        if self.kind() != Some(SourceKind::Mesh) || self.static_prop {
            return Some(Vec::new());
        }
        let settings = self
            .animations
            .iter()
            .map(|animation| {
                Some(AnimationSettings {
                    source: animation.source.clone(),
                    included: animation.included,
                    name: animation.name.clone(),
                    fps: if animation.included {
                        animation.fps.trim().parse().ok()?
                    } else {
                        30.0
                    },
                    looping: animation.looping,
                    frame_count: animation.frame_count,
                    activity: if animation.included {
                        optional_text(&animation.activity)
                    } else {
                        None
                    },
                    activity_weight: if animation.included && !animation.activity.trim().is_empty()
                    {
                        animation.activity_weight.trim().parse().ok()?
                    } else {
                        0
                    },
                    events: if animation.included {
                        animation
                            .events
                            .iter()
                            .map(|event| {
                                Some(AnimationEvent {
                                    frame: event.frame.trim().parse().ok()?,
                                    event: event.event.trim().into(),
                                    options: event.options.clone(),
                                })
                            })
                            .collect::<Option<Vec<_>>>()?
                    } else {
                        Vec::new()
                    },
                })
            })
            .collect::<Option<Vec<_>>>()?;
        gmpublished_backend::model_compile::validate_animation_settings(&settings).ok()?;
        Some(settings)
    }

    pub(super) fn animation_settings_valid(&self) -> bool {
        self.kind() != Some(SourceKind::Mesh)
            || self.static_prop
            || gmpublished_backend::model_compile::validate_animation_settings_view(
                self.animations.iter().map(AnimationEditor::view),
            )
            .is_ok()
    }

    pub(super) fn parsed_orientation(&self) -> Option<[f32; 3]> {
        let parsed = self
            .orientation
            .each_ref()
            .map(|value| value.parse::<f32>().ok().filter(|value| value.is_finite()));
        Some([parsed[0]?, parsed[1]?, parsed[2]?])
    }

    pub(super) fn qc_request(&self) -> Option<ExportRequest> {
        if self.kind() == Some(SourceKind::Compiled) {
            return None;
        }
        self.build_request(self.destination.clone().unwrap_or_default())
    }
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
