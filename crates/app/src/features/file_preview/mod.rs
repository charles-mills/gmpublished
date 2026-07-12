//! In-archive file preview modal for GMA entries.

#![cfg_attr(not(feature = "asset-studio"), allow(dead_code, unused_imports))]

mod effect;
mod message;
pub mod model;
#[cfg(feature = "asset-studio")]
mod particles3d;
mod state;
mod update;
mod view;
#[cfg(feature = "asset-studio")]
mod viewer3d;

pub use effect::Effect;
use iced::{Subscription, time};
pub use message::{Message, PreviewLoadError};
pub use model::PreviewLoadStage;
pub use model::{
    CodeLine, CodeSpan, InfoReason, MAX_PREVIEW_LINES, PreviewContent, PreviewData, PreviewRequest,
    RelatedPreviewKind, RelatedPreviewTarget,
};
#[cfg(feature = "asset-studio")]
pub use model::{
    DetailSprite, DoorAudioEvent, DoorAudioEventKind, DoorInstance, DoorSound, DoorSoundSourceTier,
    DoorSoundWave, DoorSounds, LightmapSlot, MapFog, MapSkyCamera, MapSpawn, MapStats,
    MaterialSlot, ModelPreview, ModelStats, OverlayPrimitive, OverlayVertex,
    PHY_DEBUG_MATERIAL_NAME, ParticleMaterialSlot, ParticlePreview, ParticleSystemInfo, Skybox,
    SkyboxFace, normalize_particle_material,
};
pub use state::State;
pub use update::update;
pub use view::pane;

/// True while a host modal's embedded preview pane is in its expanded
/// (near full-window) mode.
pub fn embedded_expanded(state: &State) -> bool {
    #[cfg(feature = "asset-studio")]
    {
        state.is_open() && state.expanded()
    }
    #[cfg(not(feature = "asset-studio"))]
    {
        let _ = state;
        false
    }
}

/// Animation clock for the preview modal's active work: runs only while the
/// spinner is on screen or audio is playing, never while idle.
pub fn subscription(state: &State) -> Subscription<Message> {
    if state.spinner_visible() || state.audio_playing() || state.fly_speed_readout_visible() {
        time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
            .map(Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
