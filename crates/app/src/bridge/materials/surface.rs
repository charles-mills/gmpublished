use super::parameters::{scalar, texture_transform};
use super::{Arc, BTreeMap, DecodedTextureFrames, EffectiveMaterial, MaterialResolver};

pub type TextureTransform = [[f32; 4]; 2];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SurfaceFamily {
    #[default]
    Lightmapped,
    Transition,
    VertexLit,
    Unlit,
    DecalModulate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "resolved independent Source shader permutations"
)]
pub struct ModelShadingProfile {
    /// Warp availability selects runtime lighting independently of Fresnel.
    pub runtime_lighting: bool,
    pub skin: bool,
    pub normal: bool,
    pub diffuse_warp: bool,
    pub specular_warp: bool,
    pub exponent_map: bool,
    pub half_lambert: bool,
    pub base_alpha_mask: bool,
    pub invert_mask: bool,
    pub rim: bool,
}

impl ModelShadingProfile {
    pub const fn requires_runtime_lighting(self) -> bool {
        self.runtime_lighting
    }

    pub const fn uses_pixel_lighting(self) -> bool {
        self.skin || self.normal || self.diffuse_warp
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceDetail {
    pub path: String,
    pub texture: Option<Arc<DecodedTextureFrames>>,
    pub frame: f32,
    pub scale: [f32; 2],
    pub transform: Option<TextureTransform>,
    pub mode: u32,
    pub factor: f32,
    pub tint: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceEmission {
    pub self_illumination: bool,
    pub tint: [f32; 3],
    pub mask: Option<Arc<DecodedTextureFrames>>,
    pub mask_path: Option<String>,
    pub envmap_mask_alpha: bool,
    pub fresnel: Option<[f32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecalParameters {
    pub scale: f32,
    pub fade_time: Option<f32>,
    pub fade_duration: Option<f32>,
}

impl Default for DecalParameters {
    fn default() -> Self {
        Self {
            scale: 1.0,
            fade_time: None,
            fade_duration: None,
        }
    }
}

impl DecalParameters {
    pub(crate) fn mapping_extent(self, dimensions: (u32, u32)) -> Option<[f32; 2]> {
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return None;
        }
        let reciprocal = self.scale.recip();
        let extent =
            [dimensions.0, dimensions.1].map(|dimension| (dimension as f32 / reciprocal).trunc());
        extent
            .iter()
            .all(|value| value.is_finite() && *value >= 1.0 && *value < i32::MAX as f32)
            .then_some(extent)
    }
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent authored shader switches retained before family-specific permutation selection"
)]
pub struct SurfaceMaterial {
    pub shader: String,
    pub family: SurfaceFamily,
    pub parameters: BTreeMap<String, String>,
    pub color: [f32; 3],
    pub color2: [f32; 3],
    pub alpha: f32,
    pub base_transform: TextureTransform,
    pub base2_transform: Option<TextureTransform>,
    pub base_frame: f32,
    pub base2_frame: f32,
    pub normal: Option<Arc<DecodedTextureFrames>>,
    pub normal2: Option<Arc<DecodedTextureFrames>>,
    pub normal_frame: f32,
    pub ssbump: bool,
    pub diffuse_bump: bool,
    pub transform: TextureTransform,
    pub transform2: Option<TextureTransform>,
    pub detail: Option<SurfaceDetail>,
    pub emission: Option<SurfaceEmission>,
    pub envmap: Option<Arc<super::EnvironmentMapMaterial>>,
    pub phong: Option<Arc<super::PhongMaterial>>,
    pub blend_modulation: Option<Arc<DecodedTextureFrames>>,
    pub blend_mask_transform: TextureTransform,
    pub masked_blending: bool,
    pub vertex_color: bool,
    pub vertex_alpha: bool,
    pub decal: bool,
    pub decal_parameters: DecalParameters,
    pub suppress_decals: bool,
    pub no_debug_override: bool,
    pub blend_tint_by_base_alpha: bool,
    pub tint_replace_factor: f32,
    pub alpha_test: bool,
    pub alpha_test_reference: Option<f32>,
    pub additive: bool,
    pub translucent: bool,
    pub no_cull: bool,
    pub ignore_z: bool,
    pub unsupported_proxies: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnavailableTexture<'a> {
    pub parameter: &'static str,
    pub path: &'a str,
}

impl SurfaceMaterial {
    pub(crate) const fn base_read_transfer(&self) -> super::TextureReadTransfer {
        super::TextureRole::Base {
            family: self.family,
        }
        .read_transfer()
    }

    pub(crate) const fn blends_in_gamma_space(&self) -> bool {
        matches!(self.family, SurfaceFamily::DecalModulate)
    }

    pub(crate) const fn effective_vertex_color(&self) -> bool {
        match self.family {
            SurfaceFamily::DecalModulate => false,
            _ => self.vertex_color,
        }
    }

    pub(crate) const fn effective_vertex_alpha(&self) -> bool {
        match self.family {
            SurfaceFamily::Lightmapped => self.vertex_color,
            SurfaceFamily::Transition => false,
            SurfaceFamily::DecalModulate => false,
            SurfaceFamily::VertexLit | SurfaceFamily::Unlit => self.vertex_alpha,
        }
    }
    pub(crate) fn unavailable_textures(&self) -> Vec<UnavailableTexture<'_>> {
        if self.family == SurfaceFamily::DecalModulate {
            return Vec::new();
        }
        let phong = self.phong.as_deref();
        let phong_available = |texture: Option<&super::PhongTexture>| {
            texture.is_some_and(|texture| texture.texture.is_some())
        };
        [
            ("$bumpmap", self.normal.is_some()),
            ("$bumpmap2", self.normal2.is_some()),
            (
                "$detail",
                self.detail
                    .as_ref()
                    .is_some_and(|detail| detail.texture.is_some()),
            ),
            ("$blendmodulatetexture", self.blend_modulation.is_some()),
            (
                "$selfillummask",
                self.emission
                    .as_ref()
                    .is_some_and(|emission| emission.mask.is_some()),
            ),
            ("$envmap", self.envmap.is_some()),
            (
                "$envmapmask",
                self.envmap
                    .as_ref()
                    .is_some_and(|environment| environment.mask_texture.is_some()),
            ),
            (
                "$phongexponenttexture",
                phong_available(phong.and_then(|phong| phong.exponent_texture.as_ref())),
            ),
            (
                "$phongwarptexture",
                phong_available(phong.and_then(|phong| phong.specular_warp.as_ref())),
            ),
            (
                "$lightwarptexture",
                phong_available(phong.and_then(|phong| phong.diffuse_warp.as_ref())),
            ),
        ]
        .into_iter()
        .filter_map(|(parameter, available)| {
            let path = self.parameters.get(parameter)?.as_str();
            (!available && !path.trim().is_empty())
                .then_some(UnavailableTexture { parameter, path })
        })
        .collect()
    }
    pub(crate) fn unsupported_features(&self) -> Vec<&'static str> {
        let mut features = Vec::new();
        if self.detail.as_ref().is_some_and(|detail| {
            detail.texture.is_some() && self.effective_detail_mode().is_none()
        }) {
            features.push("undefined generic detail permutation");
        }
        if self
            .phong
            .as_ref()
            .is_some_and(|phong| phong.exponent_factor.is_some())
        {
            features.push("$phongexponentfactor (absent from installed skin shader)");
        }
        if self.uses_unbumped_generic()
            && self
                .emission
                .as_ref()
                .is_some_and(|emission| emission.self_illumination && emission.envmap_mask_alpha)
            && self
                .envmap
                .as_ref()
                .is_some_and(|environment| environment.mask_texture.is_some())
        {
            features.push(
                "SELFILLUM with environment-mask-alpha glow (missing native static permutation)",
            );
        }
        features
    }

    pub(crate) fn model_shading_profile(&self) -> ModelShadingProfile {
        if self.family == SurfaceFamily::DecalModulate {
            return ModelShadingProfile::default();
        }
        let phong = self.phong.as_deref();
        let available = |texture: Option<&super::PhongTexture>| {
            texture.is_some_and(|texture| texture.texture.is_some())
        };
        let skin = self.uses_skin_shader();
        let authored_diffuse_warp =
            phong.is_some_and(|phong| available(phong.diffuse_warp.as_ref()));
        ModelShadingProfile {
            runtime_lighting: self.normal.is_some() || skin || authored_diffuse_warp,
            skin,
            normal: self.normal.is_some(),
            diffuse_warp: authored_diffuse_warp
                && (skin || self.effective_emission_mode(true) != 3),
            specular_warp: phong.is_some_and(|phong| available(phong.specular_warp.as_ref())),
            exponent_map: phong.is_some_and(|phong| available(phong.exponent_texture.as_ref())),
            half_lambert: skin || phong.is_some_and(|phong| phong.half_lambert == Some(true)),
            base_alpha_mask: phong.is_some_and(|phong| phong.base_alpha_mask == Some(true)),
            invert_mask: phong.is_some_and(|phong| phong.invert_mask == Some(true)),
            rim: skin && phong.is_some_and(|phong| phong.rim.enabled == Some(true)),
        }
    }

    fn uses_skin_shader(&self) -> bool {
        self.family == SurfaceFamily::VertexLit
            && self.phong.as_ref().is_some_and(|phong| {
                phong.enabled == Some(true)
                    && (self.normal.is_some()
                        || phong.base_alpha_mask == Some(true)
                        || phong
                            .diffuse_warp
                            .as_ref()
                            .is_some_and(|warp| warp.texture.is_some()))
            })
    }

    fn uses_unbumped_generic(&self) -> bool {
        self.family == SurfaceFamily::VertexLit
            && self.normal.is_none()
            && !self.uses_skin_shader()
            && self.phong.as_ref().is_none_or(|phong| {
                phong
                    .diffuse_warp
                    .as_ref()
                    .is_none_or(|warp| warp.texture.is_none())
            })
    }

    pub(crate) fn effective_detail_mode(&self) -> Option<u32> {
        if self.family == SurfaceFamily::DecalModulate {
            return None;
        }
        let detail = self.detail.as_ref()?;
        detail.texture.as_ref()?;
        if matches!(
            self.family,
            SurfaceFamily::Lightmapped | SurfaceFamily::Transition
        ) {
            if detail
                .texture
                .as_ref()
                .is_some_and(|texture| texture.flags & vformats::vtf::texture_flags::SSBUMP != 0)
            {
                return Some(if self.normal.is_some() { 10 } else { 11 });
            }
            return Some(if detail.mode == 1 { 1 } else { 0 });
        }
        // Model/Unlit detail permutations define only modes0..9.
        (detail.mode <= 9).then_some(detail.mode)
    }

    pub(crate) fn authored_flag(&self, name: &str) -> bool {
        super::vmt_bool(self.parameters.get(name).map(String::as_str))
    }

    pub(crate) fn effective_environment_mask(
        &self,
        base_has_alpha: bool,
    ) -> super::EnvironmentMapMask {
        use super::EnvironmentMapMask;
        if self.family == SurfaceFamily::DecalModulate {
            return EnvironmentMapMask::None;
        }
        let Some(environment) = &self.envmap else {
            return EnvironmentMapMask::None;
        };
        let mut mask = environment.mask;
        if mask == EnvironmentMapMask::BaseAlpha && !base_has_alpha {
            mask = EnvironmentMapMask::None;
        }
        if self.family == SurfaceFamily::Unlit && mask == EnvironmentMapMask::NormalAlpha {
            mask = if base_has_alpha && self.authored_flag("$basealphaenvmapmask") {
                EnvironmentMapMask::BaseAlpha
            } else if environment.mask_texture.is_some() {
                EnvironmentMapMask::Texture
            } else {
                EnvironmentMapMask::None
            };
        }
        if matches!(
            self.family,
            SurfaceFamily::Lightmapped | SurfaceFamily::Transition
        ) && self.normal.is_some()
            && matches!(
                mask,
                EnvironmentMapMask::BaseAlpha | EnvironmentMapMask::Texture
            )
        {
            mask = EnvironmentMapMask::None;
        }
        mask
    }

    pub(crate) fn base_alpha_is_mask(&self, base_has_alpha: bool) -> bool {
        let mode = self.effective_emission_mode(base_has_alpha);
        let self_illumination = mode != 0 && mode != 4;
        self_illumination
            || self.blend_tint_by_base_alpha
            || self.effective_environment_mask(base_has_alpha)
                == super::EnvironmentMapMask::BaseAlpha
    }

    pub(crate) fn effective_emission_mode(&self, base_has_alpha: bool) -> u32 {
        if self.family == SurfaceFamily::DecalModulate {
            return 0;
        }
        let Some(emission) = &self.emission else {
            return 0;
        };
        if self.family == SurfaceFamily::Unlit {
            return 0;
        }
        if self.family != SurfaceFamily::VertexLit {
            return u32::from(emission.self_illumination && base_has_alpha);
        }
        let skin = self.uses_skin_shader();
        let bumped = self.normal.is_some()
            || self.phong.as_ref().is_some_and(|phong| {
                phong
                    .diffuse_warp
                    .as_ref()
                    .is_some_and(|warp| warp.texture.is_some())
            });
        if !skin
            && !bumped
            && emission.envmap_mask_alpha
            && !emission.self_illumination
            && self
                .envmap
                .as_ref()
                .is_some_and(|environment| environment.mask_texture.is_some())
        {
            return 4;
        }
        if !emission.self_illumination {
            return 0;
        }
        if (skin || bumped)
            && emission.fresnel.is_some()
            && (skin
                || self
                    .detail
                    .as_ref()
                    .is_none_or(|detail| detail.texture.is_none()))
        {
            return 3;
        }
        if (skin || !bumped) && emission.mask.is_some() {
            return 2;
        }
        u32::from(base_has_alpha)
    }

    pub(crate) fn draw_render_mode(
        &self,
        base_flags: Option<u32>,
        alpha: f32,
    ) -> super::RenderMode {
        use super::RenderMode;
        if self.family == SurfaceFamily::DecalModulate {
            return RenderMode::Modulate;
        }
        let has_alpha = |flags: u32| {
            flags
                & (vformats::vtf::texture_flags::ONEBITALPHA
                    | vformats::vtf::texture_flags::EIGHTBITALPHA)
                != 0
        };
        let base_mask = self.base_alpha_is_mask(base_flags.is_some_and(has_alpha));
        let alpha_test = self.alpha_test && !base_mask;
        let texture_blend =
            self.translucent && !base_mask && !alpha_test && base_flags.is_none_or(has_alpha);
        let detail_blend = self.translucent
            && !base_mask
            && matches!(self.family, SurfaceFamily::VertexLit | SurfaceFamily::Unlit)
            && self.detail.as_ref().is_some_and(|detail| {
                matches!(detail.mode, 3 | 8 | 9)
                    && detail
                        .texture
                        .as_ref()
                        .is_some_and(|texture| has_alpha(texture.flags))
            });
        if self.additive {
            RenderMode::Additive
        } else if alpha < 1.0 || self.vertex_alpha || texture_blend || detail_blend {
            RenderMode::Translucent
        } else if alpha_test {
            RenderMode::Cutout
        } else {
            RenderMode::Opaque
        }
    }
}

impl Default for SurfaceMaterial {
    fn default() -> Self {
        Self {
            shader: String::new(),
            family: SurfaceFamily::default(),
            parameters: BTreeMap::new(),
            color: [1.0; 3],
            color2: [1.0; 3],
            alpha: 1.0,
            base_transform: texture_transform(None),
            base2_transform: None,
            base_frame: 0.0,
            base2_frame: 0.0,
            normal: None,
            normal2: None,
            normal_frame: 0.0,
            ssbump: false,
            diffuse_bump: true,
            transform: texture_transform(None),
            transform2: None,
            detail: None,
            emission: None,
            envmap: None,
            phong: None,
            blend_modulation: None,
            blend_mask_transform: texture_transform(None),
            masked_blending: false,
            vertex_color: false,
            vertex_alpha: false,
            decal: false,
            decal_parameters: DecalParameters::default(),
            suppress_decals: false,
            no_debug_override: false,
            blend_tint_by_base_alpha: false,
            tint_replace_factor: 0.0,
            alpha_test: false,
            alpha_test_reference: None,
            additive: false,
            translucent: false,
            no_cull: false,
            ignore_z: false,
            unsupported_proxies: Vec::new(),
        }
    }
}

fn vector3(value: Option<&str>, fallback: [f32; 3]) -> [f32; 3] {
    value
        .and_then(super::parse_material_color)
        .map(super::Vec3::to_array)
        .or_else(|| {
            value
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map(|v| [v; 3])
        })
        .unwrap_or(fallback)
}

fn scale(value: Option<&str>) -> [f32; 2] {
    let Some(value) = value else { return [4.0; 2] };
    let mut values = value.trim_matches(['[', ']']).split_whitespace();
    let x = scalar(values.next(), 4.0);
    [x, scalar(values.next(), x)]
}

impl MaterialResolver {
    pub(super) fn resolve_surface(
        &self,
        material: &EffectiveMaterial,
        family: SurfaceFamily,
    ) -> Arc<SurfaceMaterial> {
        let get = |key| material.parameter(key);
        let generic = family != SurfaceFamily::DecalModulate;
        let raw_texture = |key| get(key).and_then(|name| self.resolve_raw_texture_frames(name));
        let detail = generic
            .then(|| get("$detail"))
            .flatten()
            .map(|path| SurfaceDetail {
                path: path.to_owned(),
                texture: self.resolve_raw_texture_frames(path),
                frame: scalar(get("$detailframe"), 0.0),
                scale: scale(get("$detailscale")),
                transform: get("$detailtexturetransform").map(|v| texture_transform(Some(v))),
                mode: scalar(get("$detailblendmode"), 0.0).max(0.0) as u32,
                factor: scalar(get("$detailblendfactor"), 1.0),
                tint: vector3(get("$detailtint"), [1.0; 3]),
            });
        let self_illumination = generic && super::vmt_bool(get("$selfillum"));
        let envmap_mask_alpha = generic && super::vmt_bool(get("$selfillum_envmapmask_alpha"));
        let emission = (self_illumination || envmap_mask_alpha).then(|| SurfaceEmission {
            self_illumination,
            tint: vector3(get("$selfillumtint"), [1.0; 3]),
            mask: raw_texture("$selfillummask"),
            mask_path: get("$selfillummask").map(str::to_owned),
            envmap_mask_alpha,
            fresnel: super::vmt_bool(get("$selfillumfresnel"))
                .then(|| vector3(get("$selfillumfresnelminmaxexp"), [0.0, 1.0, 1.0])),
        });
        let unsupported_proxies = material.proxies.iter().map(|p| p.name.clone()).collect();
        let surface = SurfaceMaterial {
            shader: material.shader.clone(),
            family,
            parameters: material.parameters.clone(),
            phong: (family == SurfaceFamily::VertexLit)
                .then(|| self.resolve_phong(material))
                .flatten(),
            color: vector3(get("$color"), [1.0; 3]),
            color2: vector3(get("$color2"), [1.0; 3]),
            alpha: scalar(get("$alpha"), 1.0),
            base_transform: texture_transform(get("$basetexturetransform")),
            base2_transform: get("$basetexturetransform2").map(|v| texture_transform(Some(v))),
            base_frame: scalar(get("$frame"), 0.0),
            base2_frame: scalar(get("$frame2"), 0.0),
            normal: generic.then(|| raw_texture("$bumpmap")).flatten(),
            normal2: generic.then(|| raw_texture("$bumpmap2")).flatten(),
            normal_frame: scalar(get("$bumpframe"), 0.0),
            ssbump: super::vmt_bool(get("$ssbump")),
            diffuse_bump: !super::vmt_bool(get("$nodiffusebumplighting")),
            transform: texture_transform(get("$bumptransform")),
            transform2: get("$bumptransform2").map(|v| texture_transform(Some(v))),
            detail,
            emission,
            envmap: generic
                .then(|| self.resolve_environment_map(material))
                .flatten(),
            blend_modulation: generic
                .then(|| raw_texture("$blendmodulatetexture"))
                .flatten(),
            blend_mask_transform: texture_transform(get("$blendmasktransform")),
            masked_blending: super::vmt_bool(get("$maskedblending")),
            vertex_color: super::vmt_bool(get("$vertexcolor")),
            vertex_alpha: super::vmt_bool(get("$vertexalpha")),
            decal: super::vmt_bool(get("$decal")),
            decal_parameters: DecalParameters {
                scale: scalar(get("$decalscale"), 1.0),
                fade_time: get("decalfadetime")
                    .or_else(|| get("$decalfadetime"))
                    .map(|value| scalar(Some(value), 0.0)),
                fade_duration: get("$decalfadeduration").map(|value| scalar(Some(value), 0.0)),
            },
            suppress_decals: super::vmt_bool(get("$nodecal")),
            no_debug_override: matches!(family, SurfaceFamily::DecalModulate)
                || super::vmt_bool(get("$decal"))
                || super::vmt_bool(get("$no_debug_override")),
            blend_tint_by_base_alpha: super::vmt_bool(get("$blendtintbybasealpha")),
            tint_replace_factor: scalar(get("$blendtintcoloroverbase"), 0.0),
            alpha_test: super::vmt_bool(get("$alphatest")),
            alpha_test_reference: get("$alphatestreference").map(|v| scalar(Some(v), 0.0)),
            additive: super::vmt_bool(get("$additive")),
            translucent: super::vmt_bool(get("$translucent")),
            no_cull: super::vmt_bool(get("$nocull")),
            ignore_z: super::vmt_bool(get("$ignorez")),
            unsupported_proxies,
        };
        for texture in surface.unavailable_textures() {
            log::debug!(
                "material {}: unavailable {} texture {}",
                surface.shader,
                texture.parameter,
                texture.path
            );
        }
        for feature in surface.unsupported_features() {
            log::debug!("material {}: {feature}", surface.shader);
        }
        Arc::new(surface)
    }
}

#[cfg(test)]
mod detail_mode_tests {
    use super::*;

    #[test]
    fn native_detail_permutations_keep_texture_capability_separate_from_authored_mode() {
        let frames = |flags| {
            Arc::new(DecodedTextureFrames {
                flags,
                frames: Vec::new(),
            })
        };
        for family in [
            SurfaceFamily::Lightmapped,
            SurfaceFamily::Transition,
            SurfaceFamily::VertexLit,
            SurfaceFamily::Unlit,
        ] {
            let mut material = SurfaceMaterial {
                family,
                detail: Some(SurfaceDetail {
                    path: "detail/control".to_owned(),
                    texture: Some(frames(0)),
                    frame: 0.0,
                    scale: [1.0; 2],
                    transform: None,
                    mode: 0,
                    factor: 0.5,
                    tint: [1.0; 3],
                }),
                ..SurfaceMaterial::default()
            };
            for mode in 0..12 {
                material.detail.as_mut().unwrap().mode = mode;
                let world = matches!(
                    family,
                    SurfaceFamily::Lightmapped | SurfaceFamily::Transition
                );
                assert_eq!(
                    material.effective_detail_mode(),
                    if world {
                        Some(u32::from(mode == 1))
                    } else {
                        (mode <= 9).then_some(mode)
                    }
                );
                material.detail.as_mut().unwrap().texture =
                    Some(frames(vformats::vtf::texture_flags::SSBUMP));
                assert_eq!(
                    material.effective_detail_mode(),
                    if world {
                        Some(11)
                    } else {
                        (mode <= 9).then_some(mode)
                    }
                );
                material.normal = Some(frames(0));
                assert_eq!(
                    material.effective_detail_mode(),
                    if world {
                        Some(10)
                    } else {
                        (mode <= 9).then_some(mode)
                    }
                );
                assert_eq!(material.detail.as_ref().unwrap().mode, mode);
                material.normal = None;
                material.detail.as_mut().unwrap().texture = Some(frames(0));
            }
        }
    }
    fn available_frames() -> Arc<DecodedTextureFrames> {
        Arc::new(DecodedTextureFrames {
            flags: 0,
            frames: Vec::new(),
        })
    }

    #[test]
    fn model_profile_keeps_skin_and_generic_fresnel_warp_rules_distinct() {
        let warp = super::super::PhongTexture {
            path: "warp".into(),
            texture: Some(available_frames()),
        };
        let mut material = SurfaceMaterial {
            family: SurfaceFamily::VertexLit,
            phong: Some(Arc::new(super::super::PhongMaterial {
                diffuse_warp: Some(warp),
                ..Default::default()
            })),
            emission: Some(SurfaceEmission {
                self_illumination: true,
                tint: [1.0; 3],
                mask: None,
                mask_path: None,
                envmap_mask_alpha: false,
                fresnel: Some([0.0, 1.0, 1.0]),
            }),
            ..Default::default()
        };
        assert_eq!(material.effective_emission_mode(true), 3);
        assert!(!material.model_shading_profile().diffuse_warp);
        assert!(!material.model_shading_profile().uses_pixel_lighting());
        assert!(material.model_shading_profile().requires_runtime_lighting());
        Arc::make_mut(material.phong.as_mut().unwrap()).enabled = Some(true);
        let profile = material.model_shading_profile();
        assert!(
            profile.skin
                && profile.diffuse_warp
                && profile.half_lambert
                && profile.uses_pixel_lighting()
        );
    }

    #[test]
    fn unavailable_assets_remain_diagnosed_without_enabling_permutations() {
        let mut material = SurfaceMaterial {
            family: SurfaceFamily::VertexLit,
            parameters: BTreeMap::from([
                ("$bumpmap".into(), "missing/normal".into()),
                ("$lightwarptexture".into(), "missing/warp".into()),
            ]),
            phong: Some(Arc::new(super::super::PhongMaterial {
                enabled: Some(true),
                diffuse_warp: Some(super::super::PhongTexture {
                    path: "missing/warp".into(),
                    texture: None,
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert_eq!(material.unavailable_textures().len(), 2);
        assert!(!material.model_shading_profile().uses_pixel_lighting());
        material.normal = Some(available_frames());
        assert_eq!(
            material.unavailable_textures(),
            vec![UnavailableTexture {
                parameter: "$lightwarptexture",
                path: "missing/warp"
            }]
        );
        assert!(material.model_shading_profile().skin);
        assert!(!material.model_shading_profile().diffuse_warp);
    }
}
