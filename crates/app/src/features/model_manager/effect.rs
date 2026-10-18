use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use super::{ExportRequest, ExportResult};
use crate::generation::Generation;

#[derive(Debug)]
pub enum Effect {
    PickSource(Generation),
    PickCollisionSource(Generation, Option<PathBuf>),
    InspectCollision(Generation, PathBuf),
    PickLodSource(Generation, Option<PathBuf>),
    PickDestination(Generation, Option<PathBuf>),
    Export(Generation, ExportRequest, Arc<AtomicBool>),
    Reveal(PathBuf),
    OpenBlender(PathBuf),
    Copy(String),
    Preview(Generation, ExportResult),
    OpenPreview(crate::media::preview_model::PreviewRequest),
    Inspect(Generation, PathBuf),
    LoadQc(Generation, PathBuf),
    ValidateQc(Generation, PathBuf, String),
    PickQcPath(Generation, Option<PathBuf>),
    SaveQc(Generation, PathBuf, ExportRequest),
    PickSkinTexture(Generation),
    DiscoverMaps(Generation),
    PickMap(Generation),
    DebouncePreview(Generation),
    BuildPreview(Generation, ExportRequest, Arc<AtomicBool>),
    PrepareStage(Generation, ExportResult, PathBuf),
    OpenStage {
        model: crate::media::preview_model::PreviewRequest,
        map: crate::media::preview_model::PreviewRequest,
    },
    InstallModel {
        generation: Generation,
        result: ExportResult,
    },
    ListInstalledModels(Generation),
    RemoveInstalledModel(Generation, PathBuf),
}
