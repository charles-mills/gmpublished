use super::materials::PreparedMaterial;
use super::{MaterialSlot, PreviewScene};
use crate::bridge::materials::{
    DecodedTextureFrames, EnvironmentMapSource, ScreenRefraction, TextureLoadOutcome,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextureRole {
    Base,
    Base2,
    Normal,
    Normal2,
    Detail,
    BlendModulation,
    EmissionMask,
    Environment,
    EnvironmentMask,
    PhongExponent,
    PhongWarp,
    DiffuseWarp,
    SkyFace(usize),
    Stars(usize),
    ImmersionNormal,
    RefractionTint,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("material {material} texture role {role:?} blocked by source policy: {outcome:?}")]
pub(super) struct TextureBarrier {
    pub material: String,
    pub role: TextureRole,
    pub outcome: TextureLoadOutcome,
}

fn outcome(
    material: &str,
    role: TextureRole,
    value: TextureLoadOutcome,
) -> Result<(), TextureBarrier> {
    match value {
        TextureLoadOutcome::Loaded
        | TextureLoadOutcome::Generated
        | TextureLoadOutcome::MissingSource
        | TextureLoadOutcome::InvalidSource
        | TextureLoadOutcome::InvalidPayload => Ok(()),
        TextureLoadOutcome::QualityRejected
        | TextureLoadOutcome::SourceBudgetRejected
        | TextureLoadOutcome::SourceUnavailable
        | TextureLoadOutcome::Unsupported
        | TextureLoadOutcome::Deferred
        | TextureLoadOutcome::Unplanned => Err(TextureBarrier {
            material: material.to_owned(),
            role,
            outcome: value,
        }),
    }
}

fn data(
    material: &str,
    role: TextureRole,
    texture: Option<&DecodedTextureFrames>,
) -> Result<(), TextureBarrier> {
    texture.map_or(Ok(()), |texture| {
        outcome(material, role, texture.load_outcome())
    })
}

pub(super) fn validate_material(slot: &MaterialSlot) -> Result<(), TextureBarrier> {
    if !slot.render_mode.draws() {
        return Ok(());
    }
    for (role, texture) in [
        (TextureRole::Base, &slot.texture),
        (TextureRole::Base2, &slot.texture2),
    ] {
        if let Some(texture) = texture {
            outcome(&slot.name, role, texture.load_outcome())?;
        }
    }
    let prepared = PreparedMaterial::from(&slot.shader);
    data(&slot.name, TextureRole::Normal, prepared.normal())?;
    data(&slot.name, TextureRole::Normal2, prepared.normal2())?;
    if let Some(surface) = slot.shader.surface() {
        data(
            &slot.name,
            TextureRole::RefractionTint,
            surface
                .refraction
                .as_ref()
                .and_then(|g| g.tint_texture.as_deref()),
        )?;
        data(
            &slot.name,
            TextureRole::Detail,
            surface
                .detail
                .as_ref()
                .and_then(|detail| detail.texture.as_deref()),
        )?;
        data(
            &slot.name,
            TextureRole::BlendModulation,
            surface.blend_modulation.as_deref(),
        )?;
        data(
            &slot.name,
            TextureRole::EmissionMask,
            surface
                .emission
                .as_ref()
                .and_then(|emission| emission.mask.as_deref()),
        )?;
        if let Some(environment) = &surface.envmap {
            data(
                &slot.name,
                TextureRole::EnvironmentMask,
                environment.mask_texture.as_deref(),
            )?;
            if let EnvironmentMapSource::Named(cube) = &environment.source {
                outcome(&slot.name, TextureRole::Environment, cube.load_outcome())?;
            }
        }
        if let Some(phong) = &surface.phong {
            for (role, texture) in [
                (TextureRole::PhongExponent, &phong.exponent_texture),
                (TextureRole::PhongWarp, &phong.specular_warp),
                (TextureRole::DiffuseWarp, &phong.diffuse_warp),
            ] {
                data(
                    &slot.name,
                    role,
                    texture
                        .as_ref()
                        .and_then(|texture| texture.texture.as_deref()),
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn validate_sky(source: &PreviewScene) -> Result<(), TextureBarrier> {
    if let Some(skybox) = source.map().and_then(|map| map.skybox.as_ref()) {
        for (index, material) in skybox.materials.iter().enumerate() {
            let Some(material) = material else { continue };
            if material.no_draw {
                continue;
            }
            if material.is_painted() {
                data(
                    "<sky>",
                    TextureRole::Stars(index),
                    material
                        .painted
                        .as_ref()
                        .and_then(|paint| paint.texture.as_deref()),
                )?;
            } else if let Some(texture) = &skybox.faces[index] {
                outcome("<sky>", TextureRole::SkyFace(index), texture.load_outcome())?;
            }
        }
    }
    Ok(())
}

pub(super) fn validate_immersion(effect: &ScreenRefraction) -> Result<(), TextureBarrier> {
    data(
        &effect.name,
        TextureRole::ImmersionNormal,
        Some(&effect.normal_map),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_error_objects_are_supported_but_resource_policy_is_a_barrier() {
        for accepted in [
            TextureLoadOutcome::Loaded,
            TextureLoadOutcome::Generated,
            TextureLoadOutcome::MissingSource,
            TextureLoadOutcome::InvalidSource,
            TextureLoadOutcome::InvalidPayload,
        ] {
            assert_eq!(outcome("fixture", TextureRole::Base, accepted), Ok(()));
        }
        for rejected in [
            TextureLoadOutcome::QualityRejected,
            TextureLoadOutcome::SourceBudgetRejected,
            TextureLoadOutcome::SourceUnavailable,
            TextureLoadOutcome::Unsupported,
            TextureLoadOutcome::Deferred,
            TextureLoadOutcome::Unplanned,
        ] {
            let error = outcome("fixture", TextureRole::Normal, rejected).unwrap_err();
            assert_eq!(
                error,
                TextureBarrier {
                    material: "fixture".into(),
                    role: TextureRole::Normal,
                    outcome: rejected
                }
            );
        }
    }
}
