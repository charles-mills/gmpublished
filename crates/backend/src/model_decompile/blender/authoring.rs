use std::{path::Path, sync::atomic::AtomicBool};

use serde_json::json;

use super::super::LoadedModel;
use super::{DecompileError, Document, check_cancelled, invalid, io_error};
use crate::model_compile::authoring::{AnimationBinding, MeshBinding, SourceAuthoring};

pub(super) fn write(
    output: &mut Document,
    source: &Path,
    loaded: &LoadedModel,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    check_cancelled(cancelled)?;
    let temporary = tempfile::tempdir().map_err(|error| io_error(source, error))?;
    let exported = match super::super::decompile_model_with_cancel(
        source,
        &temporary.path().join("source"),
        cancelled,
    ) {
        Ok(exported) => exported,
        Err(error @ (DecompileError::Cancelled | DecompileError::SizeLimit)) => return Err(error),
        Err(error) => {
            output.extras["sourceAuthoringError"] = json!(error.to_string());
            output.warnings.push(format!(
                "Source authoring settings cannot be restored when recompiling this Blender export: {error}"
            ));
            return Ok(());
        }
    };
    let description = mdlc::qc::parse_qc_file(&exported.qc_path).map_err(|errors| {
        invalid(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    })?;
    let mut meshes = Vec::new();
    for (bodygroup, body) in description.bodyparts.iter().enumerate() {
        for (choice, model) in body.models.iter().enumerate() {
            if model.smd.is_empty() {
                continue;
            }
            let mut levels = Vec::new();
            for level in 0..=model.lods.len() {
                levels.push(output.nodes.iter().find_map(|node| {
                    let extras = &node["extras"];
                    (extras["sourceBodygroup"] == bodygroup
                        && extras["sourceChoice"] == choice
                        && extras["sourceLOD"] == level)
                        .then(|| node["name"].as_str().map(str::to_owned))
                        .flatten()
                }));
            }
            meshes.push(MeshBinding {
                bodygroup,
                choice,
                levels,
            });
        }
    }
    let mut animations = Vec::new();
    let reader = super::flexes::Reader(&loaded.raw_mdl);
    let records = reader.limited_table(0, 180, 184, 100, 65536)?;
    for animation in &description.animations {
        if animation.forward_declared || animation.name == "__decompiled_delta_base" {
            continue;
        }
        let index = animation
            .name
            .strip_prefix("__decompiled_animation_")
            .and_then(|index| index.parse::<usize>().ok())
            .ok_or_else(|| invalid("A reconstructed animation has no exported clip binding"))?;
        let record = *records
            .get(index)
            .ok_or_else(|| invalid("An animation binding is outside the model"))?;
        let clip = reader.string(record, 4)?;
        if !output
            .animations
            .iter()
            .any(|animation| animation["name"] == clip)
        {
            output.extras["sourceAuthoringError"] =
                json!(format!("Animation {clip:?} could not be exported"));
            output.warnings.push(format!("Source authoring settings cannot be restored because animation {clip:?} could not be exported."));
            return Ok(());
        }
        animations.push(AnimationBinding {
            name: animation.name.clone(),
            clip,
        });
    }
    output.extras["sourceAuthoring"] = serde_json::to_value(SourceAuthoring {
        version: 1,
        description,
        meshes,
        animations,
    })
    .map_err(|error| invalid(error.to_string()))?;
    for warning in exported.warnings {
        if !warning.starts_with("The QC was reconstructed")
            && !warning.starts_with("Materials and textures must be supplied")
            && !warning.starts_with("No material or texture was found for '")
            && !output.warnings.contains(&warning)
        {
            output.warnings.push(warning);
        }
    }
    Ok(())
}
