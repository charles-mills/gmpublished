use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use mdlc::{
    bone_math,
    fbx::{FbxOpts, ForcedAxis},
    gltf,
    smd::Smd,
};
use serde_json::json;

struct Source(PathBuf);

impl Source {
    fn new(parent_scale: [f32; 3], animated: bool, end: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "mdlc-gltf-affine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        let mut bytes: Vec<u8> = [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        bytes.extend(
            [0.0_f32, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes),
        );
        let mut document = json!({
            "asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}],
            "nodes": [
                {"name": "root", "scale": parent_scale, "children": [1]},
                {"name": "rotated", "rotation": [0.0, 0.0, (std::f64::consts::PI/8.0).sin(), (std::f64::consts::PI/8.0).cos()], "children": [2]},
                {"name": "mesh", "translation": [1.0, 0.0, if end {1.0} else {0.0}], "mesh": 0}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "buffers": [{"uri": format!("data:application/octet-stream;base64,{}", base64::encode(&bytes)), "byteLength": bytes.len()}],
            "bufferViews": [
                {"buffer": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 8},
                {"buffer": 0, "byteOffset": 44, "byteLength": 24}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
                {"bufferView": 1, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0], "max": [1]},
                {"bufferView": 2, "componentType": 5126, "count": 2, "type": "VEC3"}
            ]
        });
        if animated {
            document["animations"] = json!([{
                "name": "move", "samplers": [{"input": 1, "output": 2}],
                "channels": [{"sampler": 0, "target": {"node": 2, "path": "translation"}}]
            }]);
        }
        let path = directory.join("source.gltf");
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        Self(path)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn worlds(source: &Smd, frame: usize) -> Vec<bone_math::Matrix3x4> {
    let poses = &source.frames[frame].poses;
    bone_math::compute_world(
        &poses.iter().map(|pose| pose.position).collect::<Vec<_>>(),
        &poses.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
        &source
            .nodes
            .iter()
            .map(|node| node.parent)
            .collect::<Vec<_>>(),
    )
}

fn assert_close(actual: &[f32], expected: &[f32]) {
    for (actual, expected) in actual.iter().zip(expected) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }
}

#[test]
fn inherited_shear_keeps_rigid_bone_origins_on_the_transformed_mesh() {
    for parent_scale in [[2.0, 1.0, 1.0], [-2.0, 1.0, 1.0]] {
        for scale in [1.0, 0.5] {
            let source = Source::new(parent_scale, false, false);
            let options = FbxOpts {
                axis: Some(ForcedAxis::Auto),
                scale,
                ..Default::default()
            };
            let geometry = gltf::read(&source.0, "sheared hierarchy", &options).unwrap();
            let bones = worlds(&geometry.smd, 0);
            let index = geometry
                .smd
                .nodes
                .iter()
                .position(|node| node.name == "mesh")
                .unwrap();
            let origin = [bones[index][3], bones[index][7], bones[index][11]];
            assert!(
                geometry.smd.triangles[0]
                    .vertices
                    .iter()
                    .any(|vertex| vertex
                        .position
                        .iter()
                        .zip(origin)
                        .all(|(actual, expected)| (actual - expected).abs() < 0.0001)),
                "bone origin {origin:?} is detached from the mesh origin"
            );
        }
    }
}

#[test]
fn inherited_shear_uses_the_same_rigid_reference_and_animation_frames() {
    for axis in [None, Some(ForcedAxis::Auto)] {
        let source = Source::new([2.0, 1.0, 1.0], true, false);
        let end = Source::new([2.0, 1.0, 1.0], false, true);
        let options = FbxOpts {
            axis,
            ..Default::default()
        };
        let geometry = gltf::read(&source.0, "sheared hierarchy", &options).unwrap();
        let end_geometry = gltf::read(&end.0, "end hierarchy", &options).unwrap();
        let animation = gltf::read_frames(&source.0, "move", None, 1.0, &options).unwrap();
        assert_eq!(animation.frames.len(), 2);
        for (sample, reference) in worlds(&animation, 0).iter().zip(worlds(&geometry.smd, 0)) {
            assert_close(sample, &reference);
        }
        for (sample, reference) in worlds(&animation, 1)
            .iter()
            .zip(worlds(&end_geometry.smd, 0))
        {
            assert_close(sample, &reference);
        }
    }
}

#[test]
fn changing_inherited_shear_is_rejected_while_rigid_rotation_is_supported() {
    for (parent_scale, transient) in [
        ([2.0, 1.0, 1.0], false),
        ([1.0, 1.0, 1.0], false),
        ([2.0, 1.0, 1.0], true),
        ([1.0, 1.0, 1.0], true),
    ] {
        let source = Source::new(parent_scale, true, false);
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&source.0).unwrap()).unwrap();
        let mut bytes: Vec<u8> = [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let times: &[f32] = if transient {
            &[0.0, 0.5, 1.0]
        } else {
            &[0.0, 1.0]
        };
        bytes.extend(times.iter().flat_map(|time| time.to_le_bytes()));
        let mut angles = vec![std::f32::consts::FRAC_PI_4, std::f32::consts::FRAC_PI_2];
        if transient {
            angles.push(std::f32::consts::FRAC_PI_4);
        }
        for angle in angles {
            bytes.extend(
                [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()]
                    .into_iter()
                    .flat_map(f32::to_le_bytes),
            );
        }
        document["buffers"][0] = json!({"uri": format!("data:application/octet-stream;base64,{}", base64::encode(&bytes)), "byteLength": bytes.len()});
        document["bufferViews"][1]["byteLength"] = (times.len() * 4).into();
        document["bufferViews"][2]["byteOffset"] = (36 + times.len() * 4).into();
        document["bufferViews"][2]["byteLength"] = (times.len() * 16).into();
        document["accessors"][1]["count"] = times.len().into();
        document["accessors"][2]["count"] = times.len().into();
        document["accessors"][2]["type"] = "VEC4".into();
        document["animations"][0]["channels"][0]["target"] = json!({"node": 1, "path": "rotation"});
        fs::write(&source.0, serde_json::to_vec(&document).unwrap()).unwrap();
        let result = gltf::read_frames(
            &source.0,
            "changing shear",
            None,
            1.0,
            &FbxOpts {
                axis: Some(ForcedAxis::Auto),
                ..Default::default()
            },
        );
        if parent_scale[0] == 2.0 {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("scale or shear") && error.contains("rotated"),
                "{error}"
            );
        } else {
            assert_eq!(result.unwrap().frames.len(), 2);
        }
    }
}
