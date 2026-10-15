use super::{Arc, ContentPath, MaterialResolver, ResolvedTexture, Vec3, parse_water_fog_color};
use gmpublished_domain::scene::map::MapSkyPaint;

#[derive(Clone, Debug, PartialEq)]
pub struct PaintedSkyMaterial {
    pub top_color: Vec3,
    pub bottom_color: Vec3,
    pub fade_bias: f32,
    pub sun_normal: Vec3,
    pub sun_color: Vec3,
    pub sun_size: f32,
    pub dusk_color: Vec3,
    pub dusk_scale: f32,
    pub dusk_intensity: f32,
    pub hdr_scale: f32,
    pub star_texture: String,
    pub star_layers: i32,
    pub star_scale: f32,
    pub star_fade: f32,
    pub star_position: f32,
    pub star_speed: f32,
    pub texture: Option<Arc<super::LinearTextureFrames>>,
}

impl PaintedSkyMaterial {
    fn resolve(
        material: &super::EffectiveMaterial,
        controller: Option<&MapSkyPaint>,
        resolver: &MaterialResolver,
    ) -> Self {
        let vector = |key| {
            material
                .parameter(key)
                .and_then(parse_water_fog_color)
                .unwrap_or(Vec3::ZERO)
        };
        let scalar = |key| super::water::scalar(material.parameter(key), 0.0);
        // Native g_sky leaves undefined material parameters at zero. The Lua
        // entity's defaults apply only when its SkyPaint proxy actually binds.
        let mut paint = Self {
            top_color: vector("$topcolor"),
            bottom_color: vector("$bottomcolor"),
            fade_bias: scalar("$fadebias"),
            sun_normal: vector("$sunnormal"),
            sun_color: vector("$suncolor"),
            sun_size: scalar("$sunsize"),
            dusk_color: vector("$duskcolor"),
            dusk_scale: scalar("$duskscale"),
            dusk_intensity: scalar("$duskintensity"),
            hdr_scale: scalar("$hdrscale"),
            star_texture: material
                .parameter("$startexture")
                .unwrap_or_default()
                .to_owned(),
            star_layers: scalar("$starlayers") as i32,
            star_scale: scalar("$starscale"),
            star_fade: scalar("$starfade"),
            star_position: scalar("$starpos"),
            star_speed: 0.0,
            texture: None,
        };
        if let Some(controller) = controller {
            paint.top_color = controller.top_color;
            paint.bottom_color = controller.bottom_color;
            paint.fade_bias = controller.fade_bias;
            paint.sun_normal = controller.sun_normal;
            paint.sun_color = controller.sun_color;
            paint.sun_size = controller.sun_size;
            paint.dusk_color = controller.dusk_color;
            paint.dusk_scale = controller.dusk_scale;
            paint.dusk_intensity = controller.dusk_intensity;
            paint.hdr_scale = controller.hdr_scale;
            paint.star_layers = if controller.draw_stars {
                controller.star_layers
            } else {
                0
            };
            if controller.draw_stars {
                paint.star_texture.clone_from(&controller.star_texture);
                paint.star_scale = controller.client_star_scale();
                paint.star_fade = controller.client_star_fade();
                paint.star_position = 0.0;
                paint.star_speed = controller.client_star_speed();
            }
        }
        if paint.star_layers > 0 {
            if paint.star_scale == 0.0 || paint.star_fade == 0.0 {
                log::warn!(
                    "painted sky: zero star scale or fade; disabling the undefined star contribution"
                );
                paint.star_layers = 0;
            }
            paint.texture = resolver.resolve_raw_texture_frames(&paint.star_texture);
            if paint.star_layers > 0 && paint.texture.is_none() {
                log::warn!(
                    "painted sky: star texture {} unavailable; disabling its contribution",
                    paint.star_texture
                );
            }
        }
        paint
    }

    pub fn is_animated(&self) -> bool {
        self.star_layers > 0 && self.texture.is_some() && self.star_speed != 0.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkyMaterial {
    pub shader: String,
    pub texture: Option<Arc<ResolvedTexture>>,
    pub color: Vec3,
    pub transform: [[f32; 4]; 2],
    pub paint_proxy: bool,
    pub painted: Option<PaintedSkyMaterial>,
}

impl SkyMaterial {
    pub fn is_painted(&self) -> bool {
        self.shader.eq_ignore_ascii_case("g_sky")
    }
}

impl MaterialResolver {
    pub(crate) fn resolve_sky_material_at_path(
        &self,
        path: &str,
        controller: Option<&MapSkyPaint>,
    ) -> Option<SkyMaterial> {
        let path = ContentPath::new(path)?;
        self.find_entry_by_source(std::slice::from_ref(&path), |path, bytes| {
            let text = String::from_utf8_lossy(&bytes);
            let material = self.effective_material(&text, 0, &mut vec![path.clone()])?;
            let paint_proxy = material
                .proxies
                .iter()
                .any(|proxy| proxy.name.eq_ignore_ascii_case("skypaint"));
            Some(SkyMaterial {
                shader: material.shader.clone(),
                texture: material
                    .parameter("$basetexture")
                    .and_then(|name| self.resolve_texture(name, false)),
                color: material
                    .parameter("$color")
                    .and_then(parse_water_fog_color)
                    .unwrap_or(Vec3::splat(1.0)),
                transform: super::water::texture_transform(
                    material.parameter("$basetexturetransform"),
                ),
                paint_proxy,
                painted: material.shader.eq_ignore_ascii_case("g_sky").then(|| {
                    PaintedSkyMaterial::resolve(&material, controller.filter(|_| paint_proxy), self)
                }),
            })
        })
    }
}
