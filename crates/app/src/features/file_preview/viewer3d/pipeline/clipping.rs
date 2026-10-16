use super::wgpu;
use std::fmt::Write as _;
use wgpu::naga;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ClipMode {
    Primitive,
    Fragment,
}

impl ClipMode {
    pub fn for_device(device: &wgpu::Device) -> Self {
        Self::for_features(device.features())
    }

    fn for_features(features: wgpu::Features) -> Self {
        if features.contains(wgpu::Features::CLIP_DISTANCES) {
            Self::Primitive
        } else {
            Self::Fragment
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ShaderKind {
    Model,
    Detail,
    Water,
}

#[derive(Clone, Copy)]
enum VertexClipping {
    Surface,
    Unclipped,
}

impl ShaderKind {
    fn entries(self) -> &'static [(&'static str, &'static str, VertexClipping)] {
        match self {
            Self::Model => &[("vs_main", "transform_model_vertex", VertexClipping::Surface)],
            Self::Detail => &[(
                "vs_main",
                "transform_detail_vertex",
                VertexClipping::Surface,
            )],
            Self::Water => &[
                ("vs_main", "transform_water_vertex", VertexClipping::Surface),
                (
                    "vs_near_main",
                    "transform_near_water_vertex",
                    VertexClipping::Unclipped,
                ),
            ],
        }
    }

    fn distance(self, clipping: VertexClipping) -> &'static str {
        match (clipping, self) {
            (VertexClipping::Unclipped, _) => "1.0",
            (VertexClipping::Surface, Self::Model) => "value.world_position_clip.w",
            (VertexClipping::Surface, Self::Detail | Self::Water) => "value.clip_distance",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ShaderError {
    #[error("clip shader parsing failed: {0}")]
    Parse(String),
    #[error("clip shader contract is invalid: {0}")]
    Contract(&'static str),
}

pub(super) fn shader(
    source: &str,
    kind: ShaderKind,
    mode: ClipMode,
) -> Result<String, ShaderError> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| ShaderError::Parse(error.emit_to_string(source)))?;
    let (layout_handle, layout) = module
        .types
        .iter()
        .find(|(_, ty)| ty.name.as_deref() == Some("VertexOutput"))
        .ok_or(ShaderError::Contract("VertexOutput missing"))?;
    let naga::TypeInner::Struct { members, .. } = &layout.inner else {
        return Err(ShaderError::Contract("VertexOutput is not a struct"));
    };
    if members.iter().any(|member| member.name.is_none()) {
        return Err(ShaderError::Contract("unnamed varying"));
    }
    let input_type = module
        .types
        .iter()
        .find_map(|(handle, ty)| (ty.name.as_deref() == Some("VertexInput")).then_some(handle))
        .ok_or(ShaderError::Contract("VertexInput missing"))?;
    let distance_member = match kind {
        ShaderKind::Model => "world_position_clip",
        ShaderKind::Detail | ShaderKind::Water => "clip_distance",
    };
    if !members
        .iter()
        .any(|member| member.name.as_deref() == Some(distance_member))
    {
        return Err(ShaderError::Contract("clip varying missing"));
    }
    for &(_, function, _) in kind.entries() {
        let transform = module
            .functions
            .iter()
            .find_map(|(_, value)| (value.name.as_deref() == Some(function)).then_some(value))
            .ok_or(ShaderError::Contract("vertex transform missing"))?;
        if transform.arguments.len() != 1
            || transform.arguments[0].ty != input_type
            || transform.result.as_ref().map(|result| result.ty) != Some(layout_handle)
        {
            return Err(ShaderError::Contract("vertex transform signature differs"));
        }
    }
    let mut result = String::new();
    if mode == ClipMode::Primitive {
        result.push_str("enable clip_distances;\n");
        let (_, constant) = module
            .constants
            .iter()
            .find(|(_, value)| value.name.as_deref() == Some("HARDWARE_CLIPPING"))
            .ok_or(ShaderError::Contract(
                "clipping capability constant missing",
            ))?;
        if !matches!(
            module.global_expressions[constant.init],
            naga::Expression::Literal(naga::Literal::Bool(false))
        ) {
            return Err(ShaderError::Contract("clipping default must be false"));
        }
        let initializer = module
            .global_expressions
            .get_span(constant.init)
            .to_range()
            .ok_or(ShaderError::Contract("capability initializer span missing"))?;
        result.push_str(&source[..initializer.start]);
        result.push_str("true");
        result.push_str(&source[initializer.end..]);
        let declaration = module
            .types
            .get_span(layout_handle)
            .to_range()
            .and_then(|range| source.get(range))
            .ok_or(ShaderError::Contract("varying declaration span missing"))?;
        let begin = declaration
            .find('{')
            .ok_or(ShaderError::Contract("varying declaration body missing"))?;
        let end = declaration.rfind('}').unwrap_or(declaration.len());
        result.push_str("\nstruct ClippedVertexOutput {");
        result.push_str(declaration[begin + 1..end].trim_end().trim_end_matches(','));
        result.push(',');
        result.push_str("\n@builtin(clip_distances) distances:array<f32,1>,\n}\n");
    } else {
        result.push_str(source);
    }
    for &(entry, transform, clipping) in kind.entries() {
        if mode == ClipMode::Primitive {
            let _ = writeln!(
                result,
                "@vertex fn {entry}(input:VertexInput)->ClippedVertexOutput {{\nlet value={transform}(input);\nvar out:ClippedVertexOutput;"
            );
            for member in members {
                let name = member
                    .name
                    .as_deref()
                    .ok_or(ShaderError::Contract("unnamed varying"))?;
                let _ = writeln!(result, "out.{name}=value.{name};");
            }
            let _ = writeln!(
                result,
                "out.distances=array<f32,1>({});\nreturn out;\n}}",
                kind.distance(clipping)
            );
        } else {
            let _ = writeln!(
                result,
                "\n@vertex fn {entry}(input:VertexInput)->VertexOutput {{return {transform}(input);}}"
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_device_features_select_clipping_mode() {
        assert_eq!(
            ClipMode::for_features(wgpu::Features::empty()),
            ClipMode::Fragment
        );
        assert_eq!(
            ClipMode::for_features(wgpu::Features::CLIP_DISTANCES),
            ClipMode::Primitive
        );
    }

    #[test]
    fn both_stage_interfaces_validate_for_every_shader() {
        for (kind, source, entries) in [
            (ShaderKind::Model, super::super::SHADER_SOURCE, 1),
            (ShaderKind::Detail, super::super::DETAIL_SHADER_SOURCE, 1),
            (ShaderKind::Water, super::super::WATER_SHADER_SOURCE, 2),
        ] {
            for mode in [ClipMode::Fragment, ClipMode::Primitive] {
                let generated = shader(source, kind, mode).unwrap();
                let module = naga::front::wgsl::parse_str(&generated)
                    .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&generated)));
                let capabilities = if mode == ClipMode::Primitive {
                    naga::valid::Capabilities::all()
                } else {
                    naga::valid::Capabilities::all() & !naga::valid::Capabilities::CLIP_DISTANCE
                };
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
                    .validate(&module)
                    .unwrap();
                assert_eq!(
                    module
                        .entry_points
                        .iter()
                        .filter(|entry| entry.stage == naga::ShaderStage::Vertex)
                        .count(),
                    entries
                );
                for entry in &module.entry_points {
                    if entry.stage != naga::ShaderStage::Vertex {
                        continue;
                    }
                    let result = entry.function.result.as_ref().unwrap();
                    let naga::TypeInner::Struct { members, .. } = &module.types[result.ty].inner
                    else {
                        panic!("vertex interface");
                    };
                    assert_eq!(
                        members
                            .iter()
                            .filter(|member| member.binding
                                == Some(naga::Binding::BuiltIn(naga::BuiltIn::ClipDistance)))
                            .count(),
                        usize::from(mode == ClipMode::Primitive)
                    );
                }
                let (_, constant) = module
                    .constants
                    .iter()
                    .find(|(_, value)| value.name.as_deref() == Some("HARDWARE_CLIPPING"))
                    .unwrap();
                assert_eq!(
                    module.global_expressions[constant.init],
                    naga::Expression::Literal(naga::Literal::Bool(mode == ClipMode::Primitive))
                );
            }
        }
    }

    #[test]
    fn near_water_mask_ignores_surface_clipping_in_both_modes() {
        for mode in [ClipMode::Fragment, ClipMode::Primitive] {
            let generated =
                shader(super::super::WATER_SHADER_SOURCE, ShaderKind::Water, mode).unwrap();
            let near = generated.split("@vertex fn vs_near_main").nth(1).unwrap();
            if mode == ClipMode::Primitive {
                assert!(near.contains("out.distances=array<f32,1>(1.0);"));
                assert!(!near.contains("out.distances=array<f32,1>(value.clip_distance);"));
                let main = generated
                    .split("@vertex fn vs_main")
                    .nth(1)
                    .unwrap()
                    .split("@vertex")
                    .next()
                    .unwrap();
                assert!(main.contains("out.distances=array<f32,1>(value.clip_distance);"));
            } else {
                assert!(!near.contains("distances"));
            }
            let mask = generated
                .split("fn fs_near_intersection")
                .nth(1)
                .unwrap()
                .split("fn ")
                .next()
                .unwrap();
            assert!(!mask.contains("clip_distance"));
            assert!(!mask.contains("view_fragment_is_clipped"));
        }
    }

    #[test]
    fn hardware_wrapper_copies_the_complete_canonical_layout() {
        let generated = shader(
            super::super::SHADER_SOURCE,
            ShaderKind::Model,
            ClipMode::Primitive,
        )
        .unwrap();
        let module = naga::front::wgsl::parse_str(&generated).unwrap();
        let layout = |name| {
            module
                .types
                .iter()
                .find_map(|(_, ty)| {
                    if ty.name.as_deref() != Some(name) {
                        return None;
                    }
                    let naga::TypeInner::Struct { members, .. } = &ty.inner else {
                        return None;
                    };
                    Some(members)
                })
                .unwrap()
        };
        let canonical = layout("VertexOutput");
        let hardware = layout("ClippedVertexOutput");
        assert_eq!(canonical, &hardware[..canonical.len()]);
        assert_eq!(hardware.len(), canonical.len() + 1);
        for member in canonical {
            let name = member.name.as_deref().unwrap();
            assert!(generated.contains(&format!("out.{name}=value.{name};")));
        }
    }

    #[test]
    #[ignore = "requires a Vulkan GPU with CLIP_DISTANCES"]
    fn gpu_both_clipping_interfaces_validate() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("Vulkan adapter");
        assert!(adapter.features().contains(wgpu::Features::CLIP_DISTANCES));
        let capabilities = adapter.get_downlevel_capabilities();
        for required_features in [wgpu::Features::empty(), wgpu::Features::CLIP_DISTANCES] {
            let (device, queue) = iced::futures::executor::block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    required_features,
                    ..Default::default()
                },
            ))
            .expect("Vulkan device");
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let resources = super::super::resources::RenderResources::new(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                Some(&capabilities),
            );
            assert_eq!(
                resources.clip_mode,
                ClipMode::for_features(required_features)
            );
            let error = device.pop_error_scope();
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(iced::futures::executor::block_on(error).is_none());
        }
    }

    #[test]
    fn malformed_shader_contract_returns_typed_errors() {
        assert!(matches!(
            shader("invalid WGSL", ShaderKind::Model, ClipMode::Primitive),
            Err(ShaderError::Parse(_))
        ));
        assert!(matches!(
            shader("", ShaderKind::Model, ClipMode::Primitive),
            Err(ShaderError::Contract("VertexOutput missing"))
        ));
        let source = super::super::DETAIL_SHADER_SOURCE
            .replace("transform_detail_vertex", "wrong_transform");
        assert!(matches!(
            shader(&source, ShaderKind::Detail, ClipMode::Primitive),
            Err(ShaderError::Contract("vertex transform missing"))
        ));
    }
}
