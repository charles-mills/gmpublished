//! UI-facing names for bridge-produced Prepare Publish data.
//!
//! Keeping this re-export seam lets the feature describe its messages and
//! state in local vocabulary while the implementations that touch files,
//! images, settings, Steam, or transactions remain bridge/runner owned.

pub use crate::bridge::prepare_publish::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    IgnorePatternMutationResult, IgnoredPattern, PublishIconSubmitRequestEnvelope,
    PublishIconSubmitResult, PublishSubmitContext, PublishSubmitRequestEnvelope,
    PublishSubmitResult, VerifiedContentPath, VerifiedContentPathState, VerifiedIconPreview,
    WorkshopContentRequest, WorkshopSnapshotInventory, default_icon_path, publish_selected_preview,
};
#[cfg(test)]
pub use crate::bridge::prepare_publish::{VerifiedIcon, inspect_workshop_snapshot};
