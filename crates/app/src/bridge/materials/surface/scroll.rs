use super::{super::parameters::MaterialProxy, TextureTransform};

#[derive(Clone, Debug, PartialEq)]
pub struct TextureScroll {
    pub parameter: String,
    pub rate: f32,
    pub angle: f32,
    pub scale: f32,
}

impl TextureScroll {
    pub(super) fn parse(proxy: &MaterialProxy) -> Option<Self> {
        if proxy.name != "texturescroll" {
            return None;
        }
        let parameter = proxy
            .values
            .get("texturescrollvar")?
            .trim()
            .to_ascii_lowercase();
        // Only matrix destinations consumed by the generic surface shaders.
        if !matches!(
            parameter.as_str(),
            "$basetexturetransform"
                | "$bumptransform"
                | "$detailtexturetransform"
                | "$envmapmasktransform"
                | "$blendmasktransform"
        ) {
            return None;
        }
        let input = |name, default| {
            let value = proxy
                .values
                .get(name)
                .map_or(Some(default), |value| value.trim().parse::<f32>().ok())?;
            value.is_finite().then_some(value)
        };
        Some(Self {
            parameter,
            rate: input("texturescrollrate", 1.0)?,
            angle: input("texturescrollangle", 0.0)?,
            scale: input("texturescale", 1.0)?,
        })
    }

    pub(super) fn transform(&self) -> (TextureTransform, [f32; 4]) {
        // Source replaces the matrix, including authored translation/rotation.
        // Keep the phase in the view shader so immutable material bindings can
        // be shared between independently timed previews and optical passes.
        let angle = f64::from(self.angle).to_radians();
        let velocity = [
            angle.cos() as f32 * self.rate,
            angle.sin() as f32 * self.rate,
        ];
        (
            [[self.scale, 0.0, 0.0, 0.0], [0.0, self.scale, 0.0, 0.0]],
            [velocity[0], velocity[1], 1.0, 1.0],
        )
    }
}
