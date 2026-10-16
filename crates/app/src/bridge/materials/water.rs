use super::parameters::{scalar, texture_transform, tint, vec2};
use super::{
    Arc, BTreeMap, DecodedTextureFrames, EffectiveMaterial, MaterialResolver,
    normalize_texture_name, parse_material_color, vmt_bool,
};

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent authored shader switches; none describes a mutually exclusive state"
)]
pub struct WaterMaterial {
    pub shader: String,
    pub parameters: BTreeMap<String, String>,
    pub normal_map_path: String,
    pub normal_map: Option<Arc<DecodedTextureFrames>>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenRefraction {
    pub name: String,
    pub normal_map: Arc<DecodedTextureFrames>,
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
                    || (normal.frame_count() > 1 && self.frame_rate > 0.0)
            })
    }
}

impl MaterialResolver {
    fn resolve_screen_refraction(&self, name: &str) -> Option<Arc<ScreenRefraction>> {
        let paths = super::material_paths(&[], name);
        let definition = self.material_definition(&paths)?;
        let material = &definition.material;
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
    }

    pub(super) fn resolve_water(
        &self,
        material: &EffectiveMaterial,
        has_base_texture: bool,
    ) -> Arc<WaterMaterial> {
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
        Arc::new(WaterMaterial {
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
            scene_fog_color_linear: get("$fogcolor").and_then(parse_material_color).map_or(
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
        })
    }

    pub(super) fn resolve_water_normal(&self, name: &str) -> Option<Arc<DecodedTextureFrames>> {
        self.resolve_raw_texture_frames(name)
    }
}
