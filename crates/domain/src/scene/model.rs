use vformats::Limits;
pub use vformats::mdl::{
    MdlError, MeshData, ModelAnimation, ModelAnimationError, ModelAssemblyOptions, ModelPose,
    ModelSequence, ModelSkinVertex, ModelVertex, VertexCompression,
};
use vformats::mdl::{ModelData, assemble_lossy_with_options, parse_mdl, parse_vtx, parse_vvd};

pub fn load_model(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Result<ModelData, MdlError> {
    load_model_with_options(mdl, vvd, vtx, ModelAssemblyOptions::default())
}

pub fn load_model_with_options(
    mdl: &[u8],
    vvd: &[u8],
    vtx: &[u8],
    options: ModelAssemblyOptions,
) -> Result<ModelData, MdlError> {
    let limits = Limits::default();
    let mdl = parse_mdl(mdl, &limits)?;
    let vvd = parse_vvd(vvd, &limits)?;
    let vtx = parse_vtx(vtx, &limits)?;
    let read = assemble_lossy_with_options(&mdl, &vvd, &vtx, options)?;
    if read.stats.total() > 0 {
        log::debug!(
            "model assembly sanitized structures: {:?}",
            read.stats.sanitized
        );
    }
    Ok(read.model)
}

#[cfg(feature = "test-support")]
#[must_use]
pub fn animation_for_tests() -> std::sync::Arc<ModelAnimation> {
    use std::sync::{Arc, OnceLock};
    use vformats::mdl::{Vtx, Vvd, assemble};

    static ANIMATION: OnceLock<Arc<ModelAnimation>> = OnceLock::new();
    Arc::clone(ANIMATION.get_or_init(|| {
        const BONES: usize = 408;
        const CLIP: usize = BONES + 2 * 216;
        const SEQUENCE: usize = CLIP + 100;
        const ATTACHMENT: usize = SEQUENCE + 212;
        const CHANNEL: usize = ATTACHMENT + 92;
        const BLEND: usize = CHANNEL + 32;
        const WEIGHTS: usize = BLEND + 2;
        let mut bytes = vec![0u8; WEIGHTS + 8];
        fn i32_at(bytes: &mut [u8], at: usize, value: i32) {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn f32_at(bytes: &mut [u8], at: usize, value: f32) {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn name_at(bytes: &mut Vec<u8>, base: usize, offset: usize, value: &str) {
            let relative = i32::try_from(bytes.len() - base).expect("fixture offset");
            i32_at(bytes, base + offset, relative);
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        bytes[..4].copy_from_slice(b"IDST");
        i32_at(&mut bytes, 4, 48);
        for (offset, value) in [
            (156, 2),
            (160, BONES),
            (180, 1),
            (184, CLIP),
            (188, 1),
            (192, SEQUENCE),
            (240, 1),
            (244, ATTACHMENT),
        ] {
            i32_at(&mut bytes, offset, value as i32);
        }
        for index in 0..2 {
            let base = BONES + index * 216;
            i32_at(&mut bytes, base + 4, index as i32 - 1);
            f32_at(&mut bytes, base + 56, 1.0);
            for axis in 0..3 {
                f32_at(&mut bytes, base + 96 + axis * 20, 1.0);
            }
            name_at(
                &mut bytes,
                base,
                0,
                if index == 0 { "root" } else { "child" },
            );
        }
        f32_at(&mut bytes, BONES + 72, 1.0);
        f32_at(&mut bytes, BONES + 92, std::f32::consts::FRAC_PI_2 / 1000.0);
        f32_at(&mut bytes, BONES + 216 + 36, 2.0);
        f32_at(&mut bytes, BONES + 216 + 124, -2.0);
        name_at(&mut bytes, CLIP, 4, "@turn");
        f32_at(&mut bytes, CLIP + 8, 2.0);
        i32_at(&mut bytes, CLIP + 16, 3);
        i32_at(&mut bytes, CLIP + 56, (CHANNEL - CLIP) as i32);
        name_at(&mut bytes, SEQUENCE, 4, "turn");
        for axis in 0..3 {
            f32_at(&mut bytes, SEQUENCE + 32 + axis * 4, -32.0);
            f32_at(&mut bytes, SEQUENCE + 44 + axis * 4, 32.0);
        }
        for (offset, value) in [
            (56, 1),
            (60, (BLEND - SEQUENCE) as i32),
            (68, 1),
            (72, 1),
            (76, -1),
            (80, -1),
            (156, (WEIGHTS - SEQUENCE) as i32),
        ] {
            i32_at(&mut bytes, SEQUENCE + offset, value);
        }
        f32_at(&mut bytes, WEIGHTS, 1.0);
        f32_at(&mut bytes, WEIGHTS + 4, 1.0);
        name_at(&mut bytes, ATTACHMENT, 0, "light");
        i32_at(&mut bytes, ATTACHMENT + 8, 1);
        for axis in 0..3 {
            f32_at(&mut bytes, ATTACHMENT + 12 + axis * 20, 1.0);
        }
        f32_at(&mut bytes, ATTACHMENT + 24, 1.0);
        bytes[CHANNEL + 1] = 0x0c;
        bytes[CHANNEL + 8..CHANNEL + 10].copy_from_slice(&12i16.to_le_bytes());
        bytes[CHANNEL + 10..CHANNEL + 12].copy_from_slice(&14i16.to_le_bytes());
        for (offset, values) in [(16, [0i16, 1000, 2000]), (24, [0i16, 8, 16])] {
            bytes[CHANNEL + offset..CHANNEL + offset + 2].copy_from_slice(&[3, 3]);
            for (index, value) in values.into_iter().enumerate() {
                let at = CHANNEL + offset + 2 + index * 2;
                bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        let mdl = parse_mdl(&bytes, &Limits::default()).expect("fixture mdl");
        let vvd = Vvd {
            checksum: 0,
            vertices: Vec::new(),
            tangents: Vec::new(),
        };
        let vtx = Vtx {
            checksum: 0,
            body_parts: Vec::new(),
        };
        assemble(&mdl, &vvd, &vtx)
            .expect("fixture assembly")
            .model
            .animation
            .expect("fixture animation")
    }))
}

#[cfg(all(test, feature = "test-support"))]
mod animation_fixture_tests {
    use super::*;

    #[test]
    fn parsed_fixture_turns_translates_and_poses_child_attachment() {
        let animation = animation_for_tests();
        assert_eq!(animation.bone_count(), 2);
        assert_eq!(animation.duration(0).unwrap(), 1.0);
        assert!(!animation.sequences()[0].looping());
        let mut pose = ModelPose::default();
        for (cycle, expected, attachment) in [
            (0.0, [1.0, 0.0, 0.0], [1.0, 2.0, 0.0]),
            (0.5, [8.0, 1.0, 0.0], [6.0, 1.0, 0.0]),
            (1.0, [15.0, 0.0, 0.0], [15.0, -2.0, 0.0]),
        ] {
            animation.evaluate(0, cycle, &mut pose).unwrap();
            let matrix = pose.skin_matrices[0];
            let position: [f32; 3] = std::array::from_fn(|i| matrix[0][i] + matrix[3][i]);
            for axis in 0..3 {
                assert!((position[axis] - expected[axis]).abs() < 1e-5);
                assert!((pose.attachments[0].model[axis][3] - attachment[axis]).abs() < 1e-5);
            }
            assert_eq!(pose.bounds_min, [-32.0; 3]);
            assert_eq!(pose.bounds_max, [32.0; 3]);
        }
    }
}
