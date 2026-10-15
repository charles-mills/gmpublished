//! Authored material identity and water resources. Texture decoding and material
//! classification are independent: a missing texture never changes the shader.
use super::{
    Arc, BTreeMap, EffectiveMaterial, Limits, MaterialResolver, ResolvedTextureMip,
    is_water_shader, normalize_texture_name, parse_water_fog_color, texture_path, vmt_bool,
};
use vformats::keyvalues::{KvDocument, KvValue};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum MaterialShader {
    #[default]
    Unknown,
    Surface(String),
    Unlit(Arc<UnlitMaterial>),
    Lightmapped(Arc<LightmappedMaterial>),
    VertexLit(Arc<super::SurfaceMaterial>),
    Water(Arc<WaterMaterial>),
    Mirror(Arc<super::MirrorMaterial>),
}

/// UnlitGeneric's authored vertex modulation is independent of baked lighting.
#[derive(Clone, Debug, PartialEq)]
pub struct UnlitMaterial {
    pub shader: String,
    pub vertex_color: bool,
    pub vertex_alpha: bool,
    pub surface: Arc<super::SurfaceMaterial>,
}

pub type LightmappedMaterial = super::SurfaceMaterial;
impl MaterialShader {
    pub(crate) fn bind_environment_map(&mut self, environment: Arc<super::EnvironmentMapMaterial>) {
        let surface = match self {
            Self::Unlit(material) => &mut Arc::make_mut(material).surface,
            Self::Lightmapped(surface) | Self::VertexLit(surface) => surface,
            _ => return,
        };
        Arc::make_mut(surface).envmap = Some(environment);
    }

    pub(crate) fn surface(&self) -> Option<&super::SurfaceMaterial> {
        match self {
            Self::Unlit(material) => Some(&material.surface),
            Self::Lightmapped(material) | Self::VertexLit(material) => Some(material),
            _ => None,
        }
    }
    pub(crate) fn unlit(&self) -> Option<&UnlitMaterial> {
        if let Self::Unlit(material) = self {
            Some(material)
        } else {
            None
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

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent authored shader switches; none describes a mutually exclusive state"
)]
pub struct WaterMaterial {
    pub shader: String,
    pub parameters: BTreeMap<String, String>,
    pub normal_map_path: String,
    pub normal_map: Option<Arc<WaterNormalMap>>,
    pub bump_transform: [[f32; 4]; 2],
    pub proxy_scroll: [f32; 2],
    pub scroll1: [f32; 2],
    pub scroll2: [f32; 2],
    pub bump_frame: f32,
    pub frame_rate: f32,
    pub animation_wrap: bool,
    pub above_water: bool,
    pub has_base_texture: bool,
    pub reflect_enabled: bool,
    pub force_cheap: bool,
    pub reflect_entities: bool,
    pub reflect_3d_skybox: bool,
    pub refract_enabled: bool,
    pub reflect_amount: f32,
    pub refract_amount: f32,
    pub reflect_tint_linear: [f32; 3],
    pub refract_tint_linear: [f32; 3],
    pub fog_color_linear: [f32; 3],
    /// Scene fog and its clear use quantized authored bytes through the
    /// framebuffer transfer, separately from Water's shader GammaToLinear.
    pub scene_fog_color_linear: [f32; 3],
    pub fog_start: f32,
    pub fog_end: f32,
    pub fog_enabled: bool,
    pub no_fresnel: bool,
    pub below_material: Option<String>,
    pub underwater_overlay: Option<Arc<ScreenRefraction>>,
    pub unsupported_proxies: Vec<String>,
    pub unsupported_features: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinearTextureFrames {
    pub flags: u32,
    /// Stored, linear-data mip chains. Each layer is one animation frame.
    pub frames: Vec<Vec<ResolvedTextureMip>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenRefraction {
    pub name: String,
    pub normal_map: Arc<WaterNormalMap>,
    pub bump_transform: [[f32; 4]; 2],
    pub proxy_scroll: [f32; 2],
    pub bump_frame: f32,
    pub refract_amount: f32,
    pub tint_linear: [f32; 3],
    pub blur: bool,
}

impl WaterMaterial {
    pub(crate) fn is_animated(&self) -> bool {
        self.underwater_overlay
            .as_ref()
            .is_some_and(|overlay| overlay.proxy_scroll != [0.0; 2])
            || self.normal_map.as_ref().is_some_and(|normal| {
                self.proxy_scroll != [0.0; 2]
                    || (self.scroll1[0] != 0.0 && !self.has_base_texture)
                    || (normal.frames.len() > 1 && self.frame_rate > 0.0)
            })
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct WaterNormalKey {
    path: super::ContentPath,
    max_dimension: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MaterialProxy {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

pub(super) fn string_values(document: &KvDocument<'_>) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for pair in &document.pairs {
        if let KvValue::String(value) = &pair.value {
            values
                .entry(pair.key.to_ascii_lowercase())
                .or_insert_with(|| value.to_string());
        }
    }
    // The preview currently uses the Source LDR/sRGB profile. Preserve raw
    // conditional keys for diagnostics, then apply that profile's overrides.
    // Raw profile blocks survive include/patch assembly. This final pass
    // evaluates conditional strings in each selected inheritance layer.
    let overrides = document
        .pairs
        .iter()
        .filter_map(|pair| {
            let KvValue::String(value) = &pair.value else {
                return None;
            };
            let key = pair.key.to_ascii_lowercase();
            let (condition, parameter) = key.split_once('?')?;
            let (condition, negate) = condition
                .strip_prefix('!')
                .map_or((condition, false), |condition| (condition, true));
            let Some(enabled) = super::profile::HardwareProfile::LDR.condition(condition) else {
                log::warn!("unsupported material condition {condition}");
                return None;
            };
            (enabled != negate).then(|| (parameter.to_owned(), value.to_string()))
        })
        .collect::<Vec<_>>();
    values.extend(overrides);
    values
}

pub(super) fn proxies(document: &KvDocument<'_>) -> Vec<MaterialProxy> {
    document
        .blocks("proxies")
        .flat_map(|block| &block.pairs)
        .filter_map(|pair| match &pair.value {
            KvValue::Block(block) => Some(MaterialProxy {
                name: pair.key.to_ascii_lowercase(),
                values: string_values(block),
            }),
            KvValue::String(_) => None,
        })
        .collect()
}

pub(super) fn scalar(value: Option<&str>, default: f32) -> f32 {
    value
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn vector(value: Option<&str>) -> Option<Vec<f32>> {
    let value = value?.trim().trim_matches(['[', ']', '{', '}']);
    let values = value
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    values.iter().all(|v| v.is_finite()).then_some(values)
}

fn vec2(value: Option<&str>) -> [f32; 2] {
    vector(value)
        .filter(|v| v.len() >= 2)
        .map_or([0.0; 2], |v| [v[0], v[1]])
}

pub(super) fn tint(value: Option<&str>, default: [f32; 3]) -> [f32; 3] {
    value
        .and_then(parse_water_fog_color)
        .map_or(default, |color| {
            color.to_array().map(|v| v.max(0.0).powf(2.2))
        })
}

/// Source texture transforms are centered affine transforms. TextureScroll
/// replaces the transform with translation at bind time (matching its proxy).
pub(super) fn texture_transform(value: Option<&str>) -> [[f32; 4]; 2] {
    let mut center = [0.5_f32; 2];
    let mut scale = [1.0_f32; 2];
    let mut translation = [0.0_f32; 2];
    let mut rotation = 0.0_f32;
    if let Some(value) = value {
        let words = value.split_whitespace().collect::<Vec<_>>();
        let mut i = 0;
        while i < words.len() {
            let destination = match words[i].to_ascii_lowercase().as_str() {
                "center" => Some(&mut center),
                "scale" => Some(&mut scale),
                "translate" => Some(&mut translation),
                "rotate" => {
                    rotation = scalar(words.get(i + 1).copied(), 0.0);
                    i += 2;
                    continue;
                }
                _ => None,
            };
            if let Some(destination) = destination {
                *destination = [
                    scalar(words.get(i + 1).copied(), destination[0]),
                    scalar(words.get(i + 2).copied(), destination[1]),
                ];
                i += 3;
            } else {
                i += 1;
            }
        }
    }
    let (sin, cos) = rotation.to_radians().sin_cos();
    let a = cos * scale[0];
    let b = -sin * scale[1];
    let c = sin * scale[0];
    let d = cos * scale[1];
    [
        [
            a,
            b,
            0.0,
            center[0] + translation[0] - a * center[0] - b * center[1],
        ],
        [
            c,
            d,
            0.0,
            center[1] + translation[1] - c * center[0] - d * center[1],
        ],
    ]
}

pub type WaterNormalMap = LinearTextureFrames;

impl MaterialResolver {
    fn resolve_screen_refraction(&self, name: &str) -> Option<Arc<ScreenRefraction>> {
        let paths = super::material_paths(&[], name);
        self.find_entry_by_source(&paths, |path, bytes| {
            let mut visited = vec![path.clone()];
            let material =
                self.effective_material(&String::from_utf8_lossy(&bytes), 0, &mut visited)?;
            if !material.shader.eq_ignore_ascii_case("Refract")
                && !material.shader.eq_ignore_ascii_case("Refract_DX90")
            {
                log::warn!("unsupported underwater overlay shader {}", material.shader);
                return None;
            }
            let get = |key| material.parameter(key);
            if ["$normalmap2", "$envmap", "$refracttinttexture"]
                .iter()
                .any(|key| get(key).is_some())
                || vmt_bool(get("$masked"))
                || vmt_bool(get("$fadeoutonsilhouette"))
                || scalar(get("$bluramount"), 0.0) > 1.0
            {
                log::warn!("unsupported underwater Refract overlay variant: {name}");
                return None;
            }
            let normal_name = get("$normalmap").and_then(normalize_texture_name)?;
            let normal_map = self.resolve_water_normal(&normal_name)?;
            let mut bump_transform = texture_transform(get("$bumptransform"));
            let mut proxy_scroll = [0.0; 2];
            for proxy in &material.proxies {
                let value = |key: &str| proxy.values.get(key).map(String::as_str);
                if proxy.name == "texturescroll"
                    && value("texturescrollvar")
                        .is_some_and(|v| v.eq_ignore_ascii_case("$bumptransform"))
                {
                    let angle = scalar(value("texturescrollangle"), 0.0).to_radians();
                    let rate = scalar(value("texturescrollrate"), 1.0);
                    let scale = scalar(value("texturescale"), 1.0);
                    proxy_scroll = [angle.cos() * rate, angle.sin() * rate];
                    bump_transform = [[scale, 0.0, 0.0, 0.0], [0.0, scale, 0.0, 0.0]];
                } else {
                    log::warn!("unsupported underwater overlay proxy {}", proxy.name);
                    return None;
                }
            }
            Some(Arc::new(ScreenRefraction {
                name: name.to_owned(),
                normal_map,
                bump_transform,
                proxy_scroll,
                bump_frame: scalar(get("$bumpframe"), 0.0).max(0.0),
                refract_amount: scalar(get("$refractamount"), 2.0),
                tint_linear: tint(get("$refracttint"), [1.0; 3]),
                blur: scalar(get("$bluramount"), 0.0) > 0.0,
            }))
        })
    }

    pub(super) fn resolve_shader(
        &self,
        material: &EffectiveMaterial,
        has_base_texture: bool,
    ) -> MaterialShader {
        if super::mirror::is_mirror_shader(&material.shader) {
            return MaterialShader::Mirror(Arc::new(self.resolve_mirror(material)));
        }
        if !is_water_shader(&material.shader) {
            if ["UnlitGeneric", "UnlitGeneric_DX9", "UnlitGeneric_DX8"]
                .iter()
                .any(|name| material.shader.eq_ignore_ascii_case(name))
            {
                return MaterialShader::Unlit(Arc::new(UnlitMaterial {
                    shader: material.shader.clone(),
                    vertex_color: vmt_bool(material.parameter("$vertexcolor")),
                    vertex_alpha: vmt_bool(material.parameter("$vertexalpha")),
                    surface: self.resolve_surface(material, super::SurfaceFamily::Unlit),
                }));
            }
            if [
                "LightmappedGeneric",
                "LightmappedGeneric_DX9",
                "WorldVertexTransition",
                "WorldVertexTransition_DX9",
            ]
            .iter()
            .any(|name| material.shader.eq_ignore_ascii_case(name))
            {
                let family = if material
                    .shader
                    .to_ascii_lowercase()
                    .starts_with("worldvertextransition")
                {
                    super::SurfaceFamily::Transition
                } else {
                    super::SurfaceFamily::Lightmapped
                };
                return MaterialShader::Lightmapped(self.resolve_surface(material, family));
            }
            if [
                "VertexLitGeneric",
                "VertexLitGeneric_DX9",
                "VertexLitGeneric_DX8",
            ]
            .iter()
            .any(|name| material.shader.eq_ignore_ascii_case(name))
            {
                return MaterialShader::VertexLit(
                    self.resolve_surface(material, super::SurfaceFamily::VertexLit),
                );
            }
            return MaterialShader::Surface(material.shader.clone());
        }
        let get = |key| material.parameter(key);
        let normal_map_path = get("$normalmap")
            .and_then(normalize_texture_name)
            .unwrap_or_else(|| "dev/water_normal".into());
        let normal_map = self.resolve_water_normal(&normal_map_path);
        if normal_map.is_none() {
            log::warn!("water normal map unavailable: {normal_map_path}; using a flat normal");
        }
        let mut bump_transform = texture_transform(get("$bumptransform"));
        let mut proxy_scroll = [0.0; 2];
        let mut frame_rate = 0.0;
        let mut bump_frame = scalar(get("$bumpframe"), 0.0).max(0.0);
        let mut animation_wrap = true;
        let mut unsupported_proxies = Vec::new();
        for proxy in &material.proxies {
            let value = |key: &str| proxy.values.get(key).map(String::as_str);
            match proxy.name.as_str() {
                "texturescroll"
                    if value("texturescrollvar")
                        .is_some_and(|s| s.eq_ignore_ascii_case("$bumptransform")) =>
                {
                    let angle = scalar(value("texturescrollangle"), 0.0).to_radians();
                    let rate = scalar(value("texturescrollrate"), 1.0);
                    let scale = scalar(value("texturescale"), 1.0);
                    let (sin, cos) = angle.sin_cos();
                    proxy_scroll = [cos * rate, sin * rate];
                    bump_transform = [[scale, 0.0, 0.0, 0.0], [0.0, scale, 0.0, 0.0]];
                }
                "animatedtexture"
                    if value("animatedtexturevar")
                        .is_some_and(|s| s.eq_ignore_ascii_case("$normalmap"))
                        && value("animatedtextureframenumvar")
                            .is_some_and(|s| s.eq_ignore_ascii_case("$bumpframe")) =>
                {
                    frame_rate = scalar(value("animatedtextureframerate"), 15.0).max(0.0);
                    bump_frame = 0.0;
                    animation_wrap = !vmt_bool(value("animationnowrap"));
                }
                "waterlod" => {}
                _ => {
                    unsupported_proxies.push(proxy.name.clone());
                    log::warn!("unsupported water proxy {}", proxy.name);
                }
            }
        }
        let mut unsupported_features = Vec::new();
        for (active, feature) in [
            (
                vmt_bool(get("$forcecheap")),
                "cheap water needs cubemap reflections",
            ),
            (vmt_bool(get("$blurrefract")), "blurred refraction"),
            (
                vmt_bool(get("$reflectonlymarkedentities")),
                "reflection entity marking",
            ),
        ] {
            if active {
                log::warn!("unsupported water feature: {feature}");
                unsupported_features.push(feature.to_owned());
            }
        }
        MaterialShader::Water(Arc::new(WaterMaterial {
            shader: material.shader.clone(),
            parameters: material.parameters.clone(),
            normal_map_path,
            normal_map,
            bump_transform,
            proxy_scroll,
            scroll1: vec2(get("$scroll1")),
            scroll2: vec2(get("$scroll2")),
            bump_frame,
            frame_rate,
            animation_wrap,
            above_water: get("$abovewater").is_none_or(|v| vmt_bool(Some(v))),
            has_base_texture,
            reflect_enabled: get("$reflecttexture").is_some_and(|value| !value.is_empty()),
            force_cheap: vmt_bool(get("$forcecheap")),
            reflect_entities: vmt_bool(get("$reflectentities")),
            reflect_3d_skybox: get("$reflect3dskybox").is_none_or(|v| vmt_bool(Some(v))),
            refract_enabled: get("$refracttexture").is_some_and(|value| !value.is_empty()),
            reflect_amount: scalar(get("$reflectamount"), 0.8).max(0.0),
            refract_amount: scalar(get("$refractamount"), 0.0).max(0.0),
            reflect_tint_linear: tint(get("$reflecttint"), [1.0; 3]),
            refract_tint_linear: tint(get("$refracttint"), [1.0; 3]),
            fog_color_linear: tint(get("$fogcolor"), [1.0, 0.0, 0.0]),
            scene_fog_color_linear: get("$fogcolor").and_then(parse_water_fog_color).map_or(
                [1.0, 0.0, 0.0],
                |color| {
                    color
                        .to_array()
                        .map(|v| super::srgb_byte_to_linear((v.clamp(0.0, 1.0) * 255.0) as u8))
                },
            ),
            fog_start: scalar(get("$fogstart"), 0.0),
            fog_end: scalar(get("$fogend"), 1000.0),
            fog_enabled: get("$fogenable").is_none_or(|v| vmt_bool(Some(v))),
            no_fresnel: vmt_bool(get("$nofresnel")),
            below_material: get("$bottommaterial")
                .and_then(super::discovery::normalize_material_name),
            underwater_overlay: get("$underwateroverlay")
                .and_then(super::discovery::normalize_material_name)
                .filter(|name| name != "0")
                .and_then(|name| self.resolve_screen_refraction(&name)),
            unsupported_proxies,
            unsupported_features,
        }))
    }

    pub(super) fn resolve_water_normal(&self, name: &str) -> Option<Arc<WaterNormalMap>> {
        self.resolve_raw_texture_frames(name)
    }

    pub(super) fn resolve_raw_texture_frames(
        &self,
        name: &str,
    ) -> Option<Arc<LinearTextureFrames>> {
        let path = texture_path(name)?;
        let key = WaterNormalKey {
            path: path.clone(),
            max_dimension: self.config.decoded_texture_max_dimension,
        };
        if let Some(normal) = self.shared.normal_map_cache.lock().get(&key).cloned() {
            return Some(normal);
        }
        self.find_entry_by_source(std::slice::from_ref(&path), |_, bytes| {
            let limits = Limits::default();
            let vtf = vformats::vtf::parse(&bytes, &limits).ok()?;
            if vtf.frame_count() == 0 || vtf.frame_count() > 128 {
                log::warn!("linear texture {name}: invalid or excessive animation frame count");
                return None;
            }
            let max = self
                .config
                .decoded_texture_max_dimension
                .unwrap_or(u32::MAX)
                .max(1);
            let mut first_mip = 0;
            while (vtf.width() >> first_mip).max(vtf.height() >> first_mip) > max
                && first_mip + 1 < vtf.mip_count()
            {
                first_mip += 1;
            }
            if (vtf.width() >> first_mip).max(vtf.height() >> first_mip) > max {
                log::warn!(
                    "linear texture {name}: no stored mip satisfies the texture dimension limit"
                );
                return None;
            }
            let estimate = (first_mip..vtf.mip_count())
                .map(|mip| {
                    u64::from((vtf.width() >> mip).max(1))
                        * u64::from((vtf.height() >> mip).max(1))
                        * 4
                        * u64::from(vtf.frame_count())
                })
                .sum::<u64>();
            if estimate > 64 * 1024 * 1024 {
                log::warn!("linear texture {name}: decoded animation exceeds 64 MiB");
                return None;
            }
            let mut frames = Vec::new();
            let mut byte_len = 0_usize;
            for frame in 0..vtf.frame_count() {
                let mut mips = Vec::new();
                for mip in first_mip..vtf.mip_count() {
                    let image = vtf.decode_rgba(frame, 0, mip, &limits).ok()?;
                    byte_len = byte_len.checked_add(image.rgba.len())?;
                    mips.push(ResolvedTextureMip {
                        rgba: image.rgba,
                        width: image.width,
                        height: image.height,
                    });
                }
                frames.push(mips);
            }
            let mut cache = self.shared.normal_map_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return Some(Arc::clone(cached));
            }
            if let Some(budget) = &self.config.decoded_texture_budget
                && !budget.try_reserve(byte_len)
            {
                log::warn!("linear texture {name}: texture budget exhausted");
                return None;
            }
            let normal = Arc::new(WaterNormalMap {
                flags: vtf.flags(),
                frames,
            });
            cache.insert(key.clone(), Arc::clone(&normal));
            drop(cache);
            Some(normal)
        })
    }
}
