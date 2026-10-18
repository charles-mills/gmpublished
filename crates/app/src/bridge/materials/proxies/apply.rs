use super::{Program, State, Value};
use crate::bridge::materials::{SurfaceMaterial, WaterMaterial};
impl Program {
    fn scalar(&self, state: &State, name: &str) -> Option<f32> {
        self.value(state, name).map(|v| v.scalar(None))
    }
    fn vector3(&self, state: &State, name: &str) -> Option<[f32; 3]> {
        self.value(state, name).map(|v| {
            let v = v.vector();
            [v[0], v[1], v[2]]
        })
    }
    fn matrix(&self, state: &State, name: &str) -> Option<[[f32; 4]; 2]> {
        match self.value(state, name)? {
            Value::Matrix(m) => Some(*m),
            _ => None,
        }
    }
    pub fn apply_surface(&self, source: &SurfaceMaterial, state: &State) -> SurfaceMaterial {
        let mut s = source.clone();
        s.texture_scrolls.clear();
        s.animated_textures.clear();
        macro_rules! scalar {
            ($field:ident,$name:literal) => {
                if let Some(v) = self.scalar(state, $name) {
                    s.$field = v;
                }
            };
        }
        macro_rules! vector {
            ($field:ident,$name:literal) => {
                if let Some(v) = self.vector3(state, $name) {
                    s.$field = v;
                }
            };
        }
        macro_rules! matrix {
            ($field:ident,$name:literal) => {
                if let Some(v) = self.matrix(state, $name) {
                    s.$field = v;
                }
            };
        }
        scalar!(alpha, "$alpha");
        scalar!(base_frame, "$frame");
        scalar!(base2_frame, "$frame2");
        scalar!(normal_frame, "$bumpframe");
        scalar!(normal2_frame, "$bumpframe2");
        vector!(color, "$color");
        vector!(color2, "$color2");
        matrix!(base_transform, "$basetexturetransform");
        matrix!(transform, "$bumptransform");
        matrix!(blend_mask_transform, "$blendmasktransform");
        if let Some(v) = self.matrix(state, "$bumptransform2") {
            s.transform2 = Some(v);
        }
        if let Some(detail) = s.detail.as_mut() {
            if let Some(v) = self.matrix(state, "$detailtexturetransform") {
                detail.transform = Some(v);
            }
            if let Some(v) = self.scalar(state, "$detailblendfactor") {
                detail.factor = v;
            }
            if let Some(v) = self.scalar(state, "$detailframe") {
                detail.frame = v;
            }
            if let Some(v) = self.vector3(state, "$detailtint") {
                detail.tint = v;
            }
        }
        if let Some(emission) = s.emission.as_mut()
            && let Some(v) = self.vector3(state, "$selfillumtint")
        {
            emission.tint = v;
        }
        if let Some(env) = s.envmap.as_mut() {
            let env = std::sync::Arc::make_mut(env);
            if let Some(v) = self.vector3(state, "$envmaptint") {
                env.tint = v;
            }
            if let Some(v) = self.scalar(state, "$envmapcontrast") {
                env.contrast = v;
            }
            if let Some(v) = self.scalar(state, "$envmapsaturation") {
                env.saturation = v;
            }
            if let Some(v) = self.scalar(state, "$envmapframe") {
                env.frame = v.max(0.0) as u32;
            }
            if let Some(v) = self.scalar(state, "$envmapmaskframe") {
                env.mask_frame = v.max(0.0) as u32;
            }
            if let Some(v) = self.matrix(state, "$envmapmasktransform") {
                env.mask_transform = v;
            }
        }
        if let Some(phong) = s.phong.as_mut() {
            let phong = std::sync::Arc::make_mut(phong);
            if let Some(v) = self.scalar(state, "$phongboost") {
                phong.boost = Some(v);
            }
            if let Some(v) = self.scalar(state, "$phongexponent") {
                phong.exponent = Some(v);
            }
            if let Some(v) = self.vector3(state, "$phongtint") {
                phong.tint = Some(v);
            }
        }
        if let Some(glass) = s.refraction.as_mut() {
            if let Some(v) = self.scalar(state, "$refractamount") {
                glass.amount = v;
            }
            if let Some(v) = self.vector3(state, "$refracttint") {
                glass.tint = v.map(|v| v.max(0.0).powf(2.2));
            }
            if let Some(v) = self.scalar(state, "$refracttinttextureframe") {
                glass.tint_frame = v;
            }
        }
        s
    }
    pub fn apply_water(&self, source: &WaterMaterial, state: &State) -> WaterMaterial {
        let mut s = source.clone();
        s.proxy_scroll = [0.0; 2];
        s.frame_rate = 0.0;
        if let Some(v) = self.matrix(state, "$bumptransform") {
            s.bump_transform = v;
        }
        if let Some(v) = self.scalar(state, "$bumpframe") {
            s.bump_frame = v;
        }
        if let Some(v) = self.scalar(state, "$refractamount") {
            s.refract_amount = v;
        }
        if let Some(v) = self.scalar(state, "$reflectamount") {
            s.reflect_amount = v;
        }
        if let Some(v) = self.scalar(state, "$fogstart") {
            s.fog_start = v;
        }
        if let Some(v) = self.scalar(state, "$fogend") {
            s.fog_end = v;
        }
        for (name, dest) in [
            ("$refracttint", &mut s.refract_tint_linear),
            ("$reflecttint", &mut s.reflect_tint_linear),
            ("$fogcolor", &mut s.fog_color_linear),
        ] {
            if let Some(v) = self.vector3(state, name) {
                *dest = v.map(|v| v.max(0.0).powf(2.2));
            }
        }
        s
    }
}
