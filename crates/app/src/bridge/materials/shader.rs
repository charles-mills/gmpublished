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
    #[cfg(feature = "map-capture")]
    pub(crate) fn shader_name(&self) -> &str {
        match self {
            Self::Unknown => "unknown",
            Self::Unsupported(name) => name,
            Self::Surface(surface) => &surface.shader,
            Self::Water(water) => &water.shader,
            Self::Mirror(mirror) => &mirror.shader,
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

    #[cfg(any(test, feature = "map-capture"))]
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
    ) -> MaterialShader {
        if super::mirror::is_mirror_shader(&material.shader) {
            return MaterialShader::Mirror(Arc::new(self.resolve_mirror(material)));
        }
        if is_water_shader(&material.shader) {
            return MaterialShader::Water(self.resolve_water(material, has_base_texture));
        }
        if let Some(family) = surface_family(&material.shader) {
            return MaterialShader::Surface(self.resolve_surface(material, family));
        }
        MaterialShader::Unsupported(material.shader.clone())
    }
}

pub(super) fn surface_family(shader: &str) -> Option<super::SurfaceFamily> {
    use super::SurfaceFamily;
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

pub(super) fn base_read_transfer(shader: &str) -> super::TextureReadTransfer {
    surface_family(shader).map_or(super::TextureReadTransfer::SrgbToLinear, |family| {
        super::TextureRole::Base { family }.read_transfer()
    })
}
