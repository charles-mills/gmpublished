use super::{Arc, EffectiveMaterial, MaterialResolver, WaterMaterial, is_water_shader};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum MaterialShader {
    #[default]
    Unknown,
    Unsupported(String),
    Surface(Arc<super::SurfaceMaterial>),
    Water(Arc<WaterMaterial>),
    Mirror(Arc<super::MirrorMaterial>),
}

impl MaterialShader {
    pub(crate) fn mapping_dimensions(
        &self,
        base: Option<&super::ResolvedTexture>,
    ) -> Option<(u32, u32)> {
        if matches!(self, Self::Unknown | Self::Unsupported(_)) {
            return None;
        }
        if let Some(base) = base {
            if let Some(dimensions) = base.mapping_dimensions() {
                return Some(dimensions);
            }
            if base.load_outcome().is_unavailable() {
                return None;
            }
        }
        let dimensions = |texture: &super::DecodedTextureFrames| {
            let [width, height] = texture.source_metadata()?.original_dimensions;
            Some((width.max(1), height.max(1)))
        };
        match self {
            Self::Unknown | Self::Unsupported(_) => None,
            Self::Surface(surface) => {
                if let Some(mask) = surface
                    .envmap
                    .as_ref()
                    .and_then(|environment| environment.mask_texture.as_deref())
                {
                    return dimensions(mask);
                }
                surface.normal.as_deref().map_or(Some((32, 32)), dimensions)
            }
            Self::Water(water) => water
                .normal_map
                .as_deref()
                .map_or(Some((32, 32)), dimensions),
            Self::Mirror(mirror) => mirror
                .normal_map
                .as_deref()
                .map_or(Some((32, 32)), dimensions),
        }
    }

    pub(crate) fn bind_environment_map(&mut self, environment: Arc<super::EnvironmentMapMaterial>) {
        let Self::Surface(surface) = self else {
            return;
        };
        Arc::make_mut(surface).envmap = Some(environment);
    }

    pub(crate) fn surface(&self) -> Option<&super::SurfaceMaterial> {
        if let Self::Surface(surface) = self {
            Some(surface)
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn unlit(&self) -> Option<&super::SurfaceMaterial> {
        self.surface()
            .filter(|surface| surface.family == super::SurfaceFamily::Unlit)
    }

    pub(crate) fn suppresses_decals(&self) -> bool {
        match self {
            Self::Surface(surface) => surface.suppress_decals,
            Self::Water(water) => {
                super::vmt_bool(water.parameters.get("$nodecal").map(String::as_str))
            }
            Self::Mirror(mirror) => {
                super::vmt_bool(mirror.parameters.get("$nodecal").map(String::as_str))
            }
            Self::Unknown | Self::Unsupported(_) => false,
        }
    }

    pub(crate) fn is_planar(&self) -> bool {
        matches!(self, Self::Water(_) | Self::Mirror(_))
    }
    pub(crate) fn mirror(&self) -> Option<&super::MirrorMaterial> {
        if let Self::Mirror(material) = self {
            Some(material)
        } else {
            None
        }
    }
    pub(crate) fn water(&self) -> Option<&WaterMaterial> {
        match self {
            Self::Water(material) => Some(material),
            _ => None,
        }
    }
}

impl MaterialResolver {
    pub(super) fn resolve_shader(
        &self,
        material: &EffectiveMaterial,
        has_base_texture: bool,
        has_secondary_base_texture: bool,
    ) -> MaterialShader {
        if super::mirror::is_mirror_shader(&material.shader) {
            return MaterialShader::Mirror(Arc::new(self.resolve_mirror(material)));
        }
        if is_water_shader(&material.shader) {
            return MaterialShader::Water(self.resolve_water(material, has_base_texture));
        }
        if let Some(family) = surface_family(&material.shader) {
            let base_blend = family.base_blend(
                has_base_texture && material.effective_base_texture().is_some(),
                has_secondary_base_texture,
                super::vmt_bool(material.parameter("$maskedblending")),
            );
            return MaterialShader::Surface(self.resolve_surface(material, family, base_blend));
        }
        MaterialShader::Unsupported(material.shader.clone())
    }
}

pub(super) fn surface_family(shader: &str) -> Option<super::SurfaceFamily> {
    use super::SurfaceFamily;
    if super::refract::is_refract(shader) {
        return Some(SurfaceFamily::Unlit);
    }
    let families: &[(SurfaceFamily, &[&str])] = &[
        (
            SurfaceFamily::DecalModulate,
            &["DecalModulate", "DecalModulate_DX9"],
        ),
        (
            SurfaceFamily::Unlit,
            &["UnlitGeneric", "UnlitGeneric_DX9", "UnlitGeneric_DX8"],
        ),
        (
            SurfaceFamily::Lightmapped,
            &["LightmappedGeneric", "LightmappedGeneric_DX9"],
        ),
        (
            SurfaceFamily::Transition,
            &["WorldVertexTransition", "WorldVertexTransition_DX9"],
        ),
        (
            SurfaceFamily::VertexLit,
            &[
                "VertexLitGeneric",
                "VertexLitGeneric_DX9",
                "VertexLitGeneric_DX8",
            ],
        ),
    ];
    families.iter().find_map(|(family, aliases)| {
        aliases
            .iter()
            .any(|alias| shader.eq_ignore_ascii_case(alias))
            .then_some(*family)
    })
}
