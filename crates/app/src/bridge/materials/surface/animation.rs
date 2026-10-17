use super::super::{parameters::MaterialProxy, vmt_bool};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameChannel {
    Base,
    Base2,
    Bump,
    Bump2,
    Detail,
    Environment,
    EnvironmentMask,
}

impl FrameChannel {
    pub const ALL: [Self; 7] = [
        Self::Base,
        Self::Base2,
        Self::Bump,
        Self::Bump2,
        Self::Detail,
        Self::Environment,
        Self::EnvironmentMask,
    ];

    pub const fn parameter(self) -> &'static str {
        match self {
            Self::Base => "$frame",
            Self::Base2 => "$frame2",
            Self::Bump => "$bumpframe",
            Self::Bump2 => "$bumpframe2",
            Self::Detail => "$detailframe",
            Self::Environment => "$envmapframe",
            Self::EnvironmentMask => "$envmapmaskframe",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|channel| channel.parameter().eq_ignore_ascii_case(value.trim()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationWrap {
    Loop,
    NoWrap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnimationClock {
    pub time_seconds: f32,
    pub frame_seconds: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnimationError {
    #[error("texture animation clock or rate is not finite")]
    NonFinite,
    #[error("texture animation requires a positive signed frame count")]
    FrameCount,
    #[error("texture animation frame is outside the Source integer range")]
    IntegerRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnimatedTexture {
    pub texture_parameter: String,
    pub frame_channel: FrameChannel,
    pub frame_rate: f32,
    pub wrap: AnimationWrap,
}

impl AnimatedTexture {
    pub(super) fn parse(proxy: &MaterialProxy) -> Option<Self> {
        if proxy.name != "animatedtexture" {
            return None;
        }
        let value = |name| proxy.values.get(name).map(String::as_str);
        let texture_parameter = value("animatedtexturevar")?.trim().to_ascii_lowercase();
        if !matches!(
            texture_parameter.as_str(),
            "$basetexture"
                | "$basetexture2"
                | "$bumpmap"
                | "$bumpmap2"
                | "$normalmap"
                | "$detail"
                | "$envmap"
                | "$envmapmask"
                | "$selfillummask"
                | "$blendmodulatetexture"
                | "$phongexponenttexture"
                | "$phongwarptexture"
                | "$lightwarptexture"
                | "$dudvmap"
                | "$refracttinttexture"
                | "$startexture"
        ) {
            return None;
        }
        let frame_channel = FrameChannel::parse(value("animatedtextureframenumvar")?)?;
        let frame_rate = value("animatedtextureframerate")
            .map(|value| value.trim().parse::<f32>())
            .transpose()
            .ok()?
            .unwrap_or(15.0);
        frame_rate.is_finite().then_some(Self {
            texture_parameter,
            frame_channel,
            frame_rate,
            wrap: if vmt_bool(value("animationnowrap")) {
                AnimationWrap::NoWrap
            } else {
                AnimationWrap::Loop
            },
        })
    }

    pub fn frame(&self, clock: AnimationClock, frame_count: u32) -> Result<i32, AnimationError> {
        if !clock.time_seconds.is_finite()
            || !clock.frame_seconds.is_finite()
            || !self.frame_rate.is_finite()
        {
            return Err(AnimationError::NonFinite);
        }
        let count = i32::try_from(frame_count)
            .ok()
            .filter(|count| *count > 0)
            .ok_or(AnimationError::FrameCount)?;
        let current = source_integer(self.frame_rate * clock.time_seconds.max(0.0))?;
        let previous =
            source_integer(self.frame_rate * (clock.time_seconds - clock.frame_seconds).max(0.0))?;
        let frame = current % count;
        // Source's no-wrap branch tests the previous modulo frame, not total elapsed cycles.
        Ok(
            if self.wrap == AnimationWrap::NoWrap && previous % count > frame {
                count - 1
            } else {
                frame
            },
        )
    }
}

fn source_integer(value: f32) -> Result<i32, AnimationError> {
    if !value.is_finite()
        || f64::from(value.trunc()) < f64::from(i32::MIN)
        || f64::from(value.trunc()) > f64::from(i32::MAX)
    {
        return Err(AnimationError::IntegerRange);
    }
    Ok(value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn proxy(values: &[(&str, &str)]) -> MaterialProxy {
        MaterialProxy {
            name: "animatedtexture".to_owned(),
            values: values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn animation(rate: f32) -> AnimatedTexture {
        AnimatedTexture {
            texture_parameter: "$basetexture".to_owned(),
            frame_channel: FrameChannel::Base,
            frame_rate: rate,
            wrap: AnimationWrap::Loop,
        }
    }

    fn clock(time_seconds: f32) -> AnimationClock {
        AnimationClock {
            time_seconds,
            frame_seconds: 1.0 / 60.0,
        }
    }

    #[test]
    fn effective_material_keeps_proxy_order_and_reports_only_unsupported_programs() {
        use crate::bridge::{gma::PreviewArchive, materials::MaterialResolver};
        use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
        use std::sync::Arc;

        let pixels = [255, 128, 64, 255].repeat(363);
        let mut texture = fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&pixels]);
        texture[24..26].copy_from_slice(&363_u16.to_le_bytes());
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Ordered animated material")
                .entry(
                    "materials/animated.vmt",
                    br#"UnlitGeneric {
                    "$basetexture" "animated"
                    Proxies {
                        AnimatedTexture {
                            animatedtexturevar "$basetexture"
                            animatedtextureframenumvar "$frame"
                            animatedtextureframerate "8"
                        }
                        AnimatedEntityTexture {}
                        MysteryProxy {}
                        AnimatedTexture {
                            animatedtexturevar "$basetexture"
                            animatedtextureframenumvar "$frame"
                            animatedtextureframerate "30"
                        }
                        AnimatedTexture { animatedtexturevar "$basetexture" }
                    }
                }"#
                    .to_vec(),
                )
                .entry("materials/animated.vtf", texture)
                .build(),
        )
        .unwrap();
        let material = MaterialResolver::new(Arc::new(archive), None)
            .resolve_primary(&[], "animated")
            .into_material()
            .unwrap();
        let surface = material.shader.surface().unwrap();
        let count = material
            .texture
            .as_ref()
            .unwrap()
            .source_metadata()
            .unwrap()
            .frame_count;
        assert_eq!(count, 363);
        assert_eq!(
            surface.unsupported_proxies,
            ["animatedentitytexture", "mysteryproxy", "animatedtexture",]
        );
        assert_eq!(surface.animated_textures.len(), 2);
        let outputs = surface
            .animated_textures
            .iter()
            .map(|program| program.frame(clock(1.0), count).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outputs, [8, 30]);
        assert!(surface.is_animated());
    }

    #[test]
    fn receiver_specific_probe_input_remains_diagnosed_without_banning_probe_destinations() {
        use crate::bridge::{gma::PreviewArchive, materials::MaterialResolver};
        use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
        use std::sync::Arc;
        let pixels = [255, 128, 64, 255];
        let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("Local probe animation")
            .entry("materials/control.vmt", br#"LightmappedGeneric {
                "$basetexture" "base" "$envmap" "env_cubemap"
                Proxies {
                    AnimatedTexture { animatedtexturevar "$envmap" animatedtextureframenumvar "$frame" }
                    AnimatedTexture { animatedtexturevar "$basetexture" animatedtextureframenumvar "$envmapframe" }
                }
            }"#.to_vec())
            .entry("materials/base.vtf", fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&pixels]))
            .build()).unwrap();
        let material = MaterialResolver::new(Arc::new(archive), None)
            .resolve_primary(&[], "control")
            .into_material()
            .unwrap();
        let surface = material.shader.surface().unwrap();
        assert_eq!(surface.unsupported_proxies, ["animatedtexture"]);
        assert_eq!(surface.animated_textures.len(), 1);
        assert_eq!(
            surface.animated_textures[0].texture_parameter,
            "$basetexture"
        );
        assert_eq!(
            surface.animated_textures[0].frame_channel,
            FrameChannel::Environment
        );
    }

    #[test]
    fn every_destination_preserves_its_channel_and_cross_texture_input() {
        for channel in FrameChannel::ALL {
            let parsed = AnimatedTexture::parse(&proxy(&[
                ("animatedtexturevar", "$BASEtexture2"),
                ("animatedtextureframenumvar", channel.parameter()),
            ]))
            .unwrap();
            assert_eq!(parsed.texture_parameter, "$basetexture2");
            assert_eq!(parsed.frame_channel, channel);
            assert_eq!(parsed.frame_rate, 15.0);
            assert_eq!(parsed.wrap, AnimationWrap::Loop);
        }
    }

    #[test]
    fn malformed_and_unknown_entity_proxies_are_not_accepted() {
        for values in [
            vec![("animatedtexturevar", "$basetexture")],
            vec![
                ("animatedtexturevar", "$unimplementedtexture"),
                ("animatedtextureframenumvar", "$frame"),
            ],
            vec![
                ("animatedtexturevar", "$basetexture"),
                ("animatedtextureframenumvar", "$unimplementedframe"),
            ],
            vec![
                ("animatedtexturevar", "$basetexture"),
                ("animatedtextureframenumvar", "$frame"),
                ("animatedtextureframerate", "NaN"),
            ],
        ] {
            assert!(AnimatedTexture::parse(&proxy(&values)).is_none());
        }
        let mut entity = proxy(&[
            ("animatedtexturevar", "$basetexture"),
            ("animatedtextureframenumvar", "$frame"),
        ]);
        entity.name = "animatedentitytexture".to_owned();
        assert!(AnimatedTexture::parse(&entity).is_none());
    }

    #[test]
    fn all_363_frames_cross_array_limit_and_wrap_without_decimation() {
        let animation = animation(30.0);
        let selected = (0..363)
            .map(|frame| {
                animation
                    .frame(clock((frame as f32 + 0.25) / 30.0), 363)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(selected, (0..363).collect::<Vec<_>>());
        for (time, expected) in [
            (8.5, 255),
            (256.25 / 30.0, 256),
            (362.25 / 30.0, 362),
            (12.1, 0),
        ] {
            assert_eq!(animation.frame(clock(time), 363), Ok(expected));
        }
    }

    #[test]
    fn absolute_clock_is_idempotent_and_independent_between_viewers() {
        let animation = animation(8.0);
        assert_eq!(animation.frame(clock(2.25), 29), Ok(18));
        assert_eq!(animation.frame(clock(0.5), 29), Ok(4));
        assert_eq!(animation.frame(clock(2.25), 29), Ok(18));
        assert_eq!(animation.frame(clock(-5.0), 29), Ok(0));
        assert_eq!(animation.frame(clock(2.25), 1), Ok(0));
    }

    #[test]
    fn signed_source_output_is_not_silently_wrapped_to_a_positive_request() {
        assert_eq!(animation(-2.5).frame(clock(1.0), 5), Ok(-2));
        assert_eq!(animation(2.5).frame(clock(1.0), 5), Ok(2));
    }

    #[test]
    fn sdk_nowrap_uses_previous_modulo_and_does_not_invent_a_lifetime_clamp() {
        let mut animation = animation(1.0);
        animation.wrap = AnimationWrap::NoWrap;
        assert_eq!(
            animation.frame(
                AnimationClock {
                    time_seconds: 5.1,
                    frame_seconds: 0.2
                },
                5
            ),
            Ok(4)
        );
        assert_eq!(
            animation.frame(
                AnimationClock {
                    time_seconds: 5.2,
                    frame_seconds: 0.1
                },
                5
            ),
            Ok(0)
        );
        assert_eq!(animation.frame(clock(6.2), 5), Ok(1));
    }

    #[test]
    fn undefined_numeric_states_and_invalid_counts_are_explicit() {
        assert_eq!(
            animation(1.0).frame(clock(f32::NAN), 5),
            Err(AnimationError::NonFinite)
        );
        assert_eq!(
            animation(1.0).frame(clock(1.0), 0),
            Err(AnimationError::FrameCount)
        );
        assert_eq!(
            animation(1.0).frame(clock(1.0), u32::MAX),
            Err(AnimationError::FrameCount)
        );
        assert_eq!(
            animation(f32::MAX).frame(clock(2.0), 5),
            Err(AnimationError::IntegerRange)
        );
        assert_eq!(
            animation(1.0).frame(clock(2147483648.0), 5),
            Err(AnimationError::IntegerRange)
        );
    }
}
