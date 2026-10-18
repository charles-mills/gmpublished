use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compile::SourceGeometry;
use crate::smd::{Smd, SmdBoneLink, SmdFrame, SmdNode, SmdPose, SmdTriangle, SmdVertex};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendSource {
    pub source: String,
    #[serde(default = "one")]
    pub input_scale: f32,
    #[serde(default)]
    pub offset: Option<SourceOffset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOffset {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualVta {
    pub name: String,
    pub frames: Vec<VirtualVtaFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualVtaFrame {
    pub source: String,
    pub frame: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SphereNormals {
    pub material: String,
    pub center: [f32; 3],
}

pub fn sphere_normals(
    source: &mut Smd,
    specs: &[SphereNormals],
    material_paths: &[String],
) -> Result<(), String> {
    for spec in specs {
        if spec.center.iter().any(|v| !v.is_finite()) {
            return Err("spherenormals center must contain finite coordinates".into());
        }
        let key = crate::mdl_writer::texture_match_key(&spec.material, material_paths);
        let mut matched = false;
        for triangle in &mut source.triangles {
            if !crate::mdl_writer::texture_match_key(&triangle.material, material_paths)
                .eq_ignore_ascii_case(&key)
            {
                continue;
            }
            matched = true;
            for vertex in &mut triangle.vertices {
                let offset: [f64; 3] = std::array::from_fn(|axis| {
                    f64::from(vertex.position[axis]) - f64::from(spec.center[axis])
                });
                let length = offset.iter().map(|v| v * v).sum::<f64>().sqrt();
                let normal = if length > 0.0 {
                    offset.map(|v| (v / length) as f32)
                } else {
                    [0.0; 3]
                };
                let dot: f32 = normal.iter().zip(vertex.normal).map(|(a, b)| a * b).sum();
                vertex.normal = if dot < 0.0 {
                    normal.map(|v| -v)
                } else {
                    normal
                };
            }
        }
        if !matched {
            return Err(format!(
                "spherenormals material {:?} is not present in the model",
                spec.material
            ));
        }
    }
    Ok(())
}

fn one() -> f32 {
    1.0
}

fn rotate(matrix: &[f32; 12], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0] * point[0] + matrix[1] * point[1] + matrix[2] * point[2],
        matrix[4] * point[0] + matrix[5] * point[1] + matrix[6] * point[2],
        matrix[8] * point[0] + matrix[9] * point[1] + matrix[10] * point[2],
    ]
}

pub fn append_geometry(
    original: &mut SourceGeometry,
    mut appended: SourceGeometry,
    spec: &AppendSource,
) -> Result<(), String> {
    if !spec.input_scale.is_finite() || spec.input_scale <= 0.0 {
        return Err("appended source scale must be positive and finite".into());
    }
    // StudioMDL applies this matrix after Load_Source's scale.
    let (mut matrix, scale) = if let Some(offset) = &spec.offset {
        if offset
            .position
            .iter()
            .chain(&offset.rotation)
            .any(|v| !v.is_finite())
            || !offset.scale.is_finite()
            || offset.scale <= 0.0
        {
            return Err(
                "appended source offset must contain finite values and a positive scale".into(),
            );
        }
        let [pitch, yaw, roll] = offset.rotation.map(f32::to_radians);
        (
            crate::bone_math::local_transform(offset.position, [roll, pitch, yaw]),
            offset.scale / spec.input_scale,
        )
    } else {
        (crate::bone_math::IDENTITY, spec.input_scale)
    };
    for row in 0..3 {
        for col in 0..3 {
            matrix[row * 4 + col] *= scale;
        }
    }
    let next_source_index = original
        .smd
        .triangles
        .iter()
        .flat_map(|t| &t.vertices)
        .map(|v| v.src_index)
        .filter(|&index| index != crate::smd::NO_SOURCE_INDEX)
        .max()
        .map_or(Ok(0), |index| {
            index.checked_add(1).ok_or("source vertex indices overflow")
        })?;
    let mut names: HashMap<String, i32> = original
        .smd
        .nodes
        .iter()
        .map(|n| (n.name.to_ascii_lowercase(), n.index))
        .collect();
    let mut last_bone = original
        .smd
        .nodes
        .iter()
        .map(|n| n.index)
        .max()
        .unwrap_or(-1);
    let mut remap = HashMap::new();
    for node in &appended.smd.nodes {
        let bone = if let Some(&existing) = names.get(&node.name.to_ascii_lowercase()) {
            existing
        } else {
            let index = last_bone
                .checked_add(1)
                .ok_or("source bone indices overflow")?;
            last_bone = index;
            names.insert(node.name.to_ascii_lowercase(), index);
            index
        };
        if remap.insert(node.index, bone).is_some() {
            return Err(format!("duplicate appended bone index {}", node.index));
        }
    }
    let mut new_nodes = Vec::new();
    for node in &appended.smd.nodes {
        let index = remap[&node.index];
        let parent = if node.parent < 0 {
            -1
        } else {
            *remap
                .get(&node.parent)
                .ok_or_else(|| format!("appended bone {:?} has a missing parent", node.name))?
        };
        if let Some(existing) = original.smd.nodes.iter().find(|n| n.index == index) {
            if existing.parent != parent {
                return Err(format!(
                    "appended bone {:?} has a different hierarchy",
                    node.name
                ));
            }
        } else {
            new_nodes.push(SmdNode {
                index,
                name: node.name.clone(),
                parent,
            });
        }
    }
    let new_indices: std::collections::HashSet<_> = new_nodes.iter().map(|n| n.index).collect();
    let poses = appended
        .smd
        .frames
        .first()
        .map(|f| f.poses.as_slice())
        .unwrap_or(&[]);
    for frame in &mut original.smd.frames {
        for pose in poses {
            let bone = *remap
                .get(&pose.bone)
                .ok_or("appended pose refers to an unknown bone")?;
            if new_indices.contains(&bone) {
                frame.poses.push(SmdPose { bone, ..*pose });
            }
        }
    }
    for triangle in &mut appended.smd.triangles {
        for vertex in &mut triangle.vertices {
            vertex.parent_bone = *remap
                .get(&vertex.parent_bone)
                .ok_or("appended vertex refers to an unknown parent bone")?;
            for link in &mut vertex.links {
                link.bone = *remap
                    .get(&link.bone)
                    .ok_or("appended vertex refers to an unknown bone")?;
            }
            let position = rotate(&matrix, vertex.position);
            vertex.position = [
                position[0] + matrix[3],
                position[1] + matrix[7],
                position[2] + matrix[11],
            ];
            let normal = rotate(&matrix, vertex.normal);
            let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
            vertex.normal = if length > 0.0 {
                normal.map(|v| v / length)
            } else {
                normal
            };
            if vertex.src_index != crate::smd::NO_SOURCE_INDEX {
                vertex.src_index = vertex
                    .src_index
                    .checked_add(next_source_index)
                    .filter(|&i| i != crate::smd::NO_SOURCE_INDEX)
                    .ok_or("source vertex indices overflow")?;
            }
        }
    }
    for shape in &mut appended.shape_keys {
        for index in &mut shape.vertex_index {
            *index = index
                .checked_add(next_source_index)
                .filter(|&i| i != crate::smd::NO_SOURCE_INDEX)
                .ok_or("shape vertex indices overflow")?;
        }
        for position in &mut shape.position_offsets {
            *position = rotate(&matrix, *position);
        }
        for normal in &mut shape.normal_offsets {
            *normal = rotate(&matrix, *normal).map(|v| v / scale);
        }
    }
    if let Some(mut metadata) = appended.dmx {
        for shape in &mut metadata.shapes {
            for delta in &mut shape.vertices {
                delta.vertex = delta
                    .vertex
                    .checked_add(next_source_index)
                    .filter(|&index| index != crate::smd::NO_SOURCE_INDEX)
                    .ok_or("DMX shape vertex indices overflow")?;
                delta.position = rotate(&matrix, delta.position);
                delta.normal = rotate(&matrix, delta.normal).map(|v| v / scale);
            }
        }
        crate::dmx::merge_metadata(original.dmx.get_or_insert_with(Default::default), metadata)?;
    }
    original.smd.nodes.extend(new_nodes);
    original.smd.triangles.extend(appended.smd.triangles);
    original.shape_keys.extend(appended.shape_keys);
    original
        .untextured_meshes
        .extend(appended.untextured_meshes);
    original.merged_meshes.extend(appended.merged_meshes);
    Ok(())
}

#[derive(Clone, Copy)]
struct Corner {
    position: usize,
    uv: Option<usize>,
    normal: Option<usize>,
}

struct Obj {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    triangles: Vec<(String, [Corner; 3])>,
}

fn index(text: &str, count: usize) -> Result<usize, String> {
    let value: i64 = text
        .parse()
        .map_err(|_| format!("invalid OBJ index {text:?}"))?;
    let index = if value < 0 {
        count as i64 + value
    } else {
        value - 1
    };
    if index < 0 || index >= count as i64 {
        return Err(format!(
            "OBJ index {value} is outside the {count} declared values"
        ));
    }
    Ok(index as usize)
}

fn parse_obj(text: &str) -> Result<Obj, String> {
    let mut obj = Obj {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        triangles: Vec::new(),
    };
    let mut material = "debugempty".to_owned();
    for (line, raw) in text.lines().enumerate() {
        let mut parts = raw.split('#').next().unwrap_or("").split_whitespace();
        let Some(command) = parts.next() else {
            continue;
        };
        let parse = |token: Option<&str>| -> Result<f32, String> {
            let value: f32 = token
                .ok_or_else(|| format!("OBJ line {}: missing coordinate", line + 1))?
                .parse()
                .map_err(|_| format!("OBJ line {}: invalid coordinate", line + 1))?;
            if !value.is_finite() {
                return Err(format!("OBJ line {}: nonfinite coordinate", line + 1));
            }
            Ok(value)
        };
        match command {
            "v" => obj.positions.push([
                parse(parts.next())?,
                parse(parts.next())?,
                parse(parts.next())?,
            ]),
            "vn" => obj.normals.push([
                parse(parts.next())?,
                parse(parts.next())?,
                parse(parts.next())?,
            ]),
            "vt" => obj.uvs.push([parse(parts.next())?, parse(parts.next())?]),
            "usemtl" => {
                material = parts.collect::<Vec<_>>().join(" ");
                if material.is_empty() {
                    return Err(format!("OBJ line {}: empty material", line + 1));
                }
            }
            "f" => {
                let mut face = Vec::new();
                for text in parts {
                    let mut ids = text.split('/');
                    let position = index(ids.next().unwrap_or(""), obj.positions.len())?;
                    let uv = ids
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|s| index(s, obj.uvs.len()))
                        .transpose()?;
                    let normal = ids
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|s| index(s, obj.normals.len()))
                        .transpose()?;
                    if ids.next().is_some() {
                        return Err(format!("OBJ line {}: malformed face corner", line + 1));
                    }
                    face.push(Corner {
                        position,
                        uv,
                        normal,
                    });
                }
                if face.len() < 3 {
                    return Err(format!(
                        "OBJ line {}: face has fewer than three vertices",
                        line + 1
                    ));
                }
                for i in 1..face.len() - 1 {
                    obj.triangles
                        .push((material.clone(), [face[0], face[i], face[i + 1]]));
                }
            }
            _ => {}
        }
    }
    if obj.triangles.is_empty() {
        return Err("OBJ contains no faces".into());
    }
    Ok(obj)
}

pub fn read_obj(text: &str) -> Result<Smd, String> {
    let obj = parse_obj(text)?;
    let triangles = obj
        .triangles
        .into_iter()
        .map(|(material, corners)| {
            let points = corners.map(|c| obj.positions[c.position]);
            let u = [
                points[1][0] - points[0][0],
                points[1][1] - points[0][1],
                points[1][2] - points[0][2],
            ];
            let v = [
                points[2][0] - points[0][0],
                points[2][1] - points[0][1],
                points[2][2] - points[0][2],
            ];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let length = n.iter().map(|v| v * v).sum::<f32>().sqrt();
            let normal = if length > 0.0 {
                n.map(|v| v / length)
            } else {
                [0.0, 0.0, 1.0]
            };
            SmdTriangle {
                material,
                vertices: corners.map(|corner| SmdVertex {
                    parent_bone: 0,
                    position: obj.positions[corner.position],
                    normal: corner.normal.map_or(normal, |i| obj.normals[i]),
                    uv: corner
                        .uv
                        .map_or([0.0; 2], |i| [obj.uvs[i][0], 1.0 - obj.uvs[i][1]]),
                    links: vec![SmdBoneLink {
                        bone: 0,
                        weight: 1.0,
                    }],
                    src_index: corner.position as u32,
                }),
            }
        })
        .collect();
    Ok(Smd {
        version: 1,
        nodes: vec![SmdNode {
            index: 0,
            name: "default".into(),
            parent: -1,
        }],
        frames: vec![SmdFrame {
            time: 0,
            poses: vec![SmdPose {
                bone: 0,
                position: [0.0; 3],
                rotation: [0.0; 3],
            }],
        }],
        triangles,
    })
}

pub fn read_obj_file(path: &Path) -> Result<Smd, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut source = read_obj(&text)?;
    let mut materials = HashMap::new();
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        if words.next() != Some("mtllib") {
            continue;
        }
        for library in words {
            let mtl_path = path
                .parent()
                .unwrap_or(Path::new("."))
                .join(library.replace('\\', "/"));
            let content = match std::fs::read_to_string(&mtl_path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("cannot read {}: {error}", mtl_path.display())),
            };
            let mut name = None;
            for line in content.lines() {
                let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                match words.next() {
                    Some("newmtl") => {
                        name = words.next().map(str::to_owned);
                        if let Some(name) = &name {
                            materials.insert(name.to_ascii_lowercase(), "debugempty".to_owned());
                        }
                    }
                    Some("map_Kd") => {
                        if let (Some(name), Some(texture)) = (&name, words.next_back()) {
                            let normalized = texture.replace('\\', "/");
                            if let Some(stem) =
                                Path::new(&normalized).file_stem().and_then(|s| s.to_str())
                            {
                                materials.insert(name.to_ascii_lowercase(), stem.to_owned());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    for triangle in &mut source.triangles {
        if let Some(texture) = materials.get(&triangle.material.to_ascii_lowercase()) {
            triangle.material.clone_from(texture);
        }
    }
    Ok(source)
}

pub fn load_virtual_vta(
    spec: &VirtualVta,
    base: &Path,
    scale: f32,
) -> Result<crate::vta::Vta, String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("vertex animation scale must be positive and finite".into());
    }
    if spec.frames.is_empty() || !spec.frames.iter().any(|f| f.frame == 0) {
        return Err(format!("virtual VTA {:?} requires frame 0", spec.name));
    }
    let last = spec.frames.iter().map(|f| f.frame).max().unwrap();
    if last > 65535 || spec.frames.iter().any(|f| f.frame < 0) {
        return Err("virtual VTA frames must be between 0 and 65535".into());
    }
    let mut frames = vec![Vec::new(); last as usize + 1];
    let mut topology = None;
    for frame in &spec.frames {
        if !frames[frame.frame as usize].is_empty() {
            return Err(format!(
                "virtual VTA frame {} is declared twice",
                frame.frame
            ));
        }
        let path = base.join(frame.source.replace('\\', "/"));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let obj = parse_obj(&text)?;
        let mut mapping = std::collections::HashSet::new();
        let mut vertices = Vec::new();
        let mut keys = Vec::new();
        for (_, face) in &obj.triangles {
            // FakeVTA numbers vertices in StudioMDL's reversed triangle order.
            for corner in [face[0], face[2], face[1]] {
                let normal_index = corner.normal.ok_or_else(|| {
                    format!("virtual VTA frame {} requires OBJ normals", frame.frame)
                })?;
                let key = (corner.position, normal_index);
                if !mapping.insert(key) {
                    continue;
                }
                let index = vertices.len() as u32;
                keys.push(key);
                let [x, y, z] = obj.positions[corner.position];
                let [nx, ny, nz] = obj.normals[normal_index];
                vertices.push(crate::vta::VtaVert {
                    index,
                    pos: [x * scale, -z * scale, y * scale],
                    normal: [nx, -nz, ny],
                });
            }
        }
        if let Some(expected) = &topology {
            if expected != &keys {
                return Err(format!(
                    "virtual VTA frame {} changes vertex or normal topology",
                    frame.frame
                ));
            }
        } else {
            topology = Some(keys);
        }
        frames[frame.frame as usize] = vertices;
    }
    Ok(crate::vta::Vta {
        start_frame: 0,
        end_frame: last,
        num_vertices: frames[0].len(),
        frames,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGLE: &str =
        "v 1 2 3\nv 4 2 3\nv 1 5 3\nvn 0 0 1\nvt 0.2 0.3\nusemtl paint\nf 1/1/1 2/1/1 3/1/1\n";

    fn geometry(source: Smd) -> SourceGeometry {
        SourceGeometry {
            dmx: None,
            smd: source,
            shape_keys: Vec::new(),
            untextured_meshes: Vec::new(),
            merged_meshes: Vec::new(),
            anim_stacks: Vec::new(),
        }
    }

    #[test]
    fn obj_geometry_preserves_materials_uvs_and_source_winding() {
        let source = read_obj(TRIANGLE).unwrap();
        assert_eq!(source.triangles.len(), 1);
        let triangle = &source.triangles[0];
        assert_eq!(triangle.material, "paint");
        assert_eq!(
            triangle
                .vertices
                .iter()
                .map(|v| v.src_index)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(triangle.vertices[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(triangle.vertices[0].uv, [0.2, 0.7]);
        assert_eq!(triangle.vertices[0].normal, [0.0, 0.0, 1.0]);
        let negative = read_obj("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf -4 -3 -2 -1\n").unwrap();
        assert_eq!(negative.triangles.len(), 2);
        assert_eq!(negative.triangles[1].vertices[1].position, [1.0, 1.0, 0.0]);
        for invalid in [
            "v NaN 0 0",
            "v 0 0 0\nf 1 2 3",
            "v 0 0 0\nf 0 0 0",
            "v 0 0 0\nf 1 1",
        ] {
            assert!(read_obj(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn spherical_normals_preserve_the_original_facing_and_only_affect_the_target_material() {
        let mut source = read_obj(TRIANGLE).unwrap();
        source.triangles[0].vertices[1].normal = [-1.0, 0.0, 0.0];
        let mut other = source.triangles[0].clone();
        other.material = "other".into();
        source.triangles.push(other.clone());
        let spec = SphereNormals {
            material: "models/PAINT.vmt".into(),
            center: [1.0, 2.0, 3.0],
        };
        sphere_normals(&mut source, std::slice::from_ref(&spec), &[]).unwrap();
        assert_eq!(source.triangles[0].vertices[0].normal, [0.0; 3]);
        assert_eq!(source.triangles[0].vertices[1].normal, [-1.0, 0.0, 0.0]);
        assert_eq!(source.triangles[0].vertices[2].normal, [0.0, 1.0, 0.0]);
        assert_eq!(source.triangles[1], other);
        let absent = SphereNormals {
            material: "absent".into(),
            ..spec.clone()
        };
        assert!(
            sphere_normals(&mut source, &[absent], &[])
                .unwrap_err()
                .contains("not present")
        );
        let invalid = SphereNormals {
            center: [f32::NAN, 0.0, 0.0],
            ..spec
        };
        assert!(sphere_normals(&mut source, &[invalid], &[]).is_err());
    }

    #[test]
    fn append_remaps_sparse_bones_and_morph_indices_and_transforms_appended_geometry() {
        let mut original = geometry(read_obj(TRIANGLE).unwrap());
        original.smd.nodes[0].index = 9;
        original.smd.frames[0].poses[0].bone = 9;
        for vertex in original
            .smd
            .triangles
            .iter_mut()
            .flat_map(|t| &mut t.vertices)
        {
            vertex.parent_bone = 9;
            vertex.links[0].bone = 9;
        }
        let mut appended = geometry(read_obj(TRIANGLE).unwrap());
        appended.smd.nodes[0].index = 44;
        appended.smd.nodes.push(SmdNode {
            index: 7,
            name: "extra".into(),
            parent: 44,
        });
        appended.smd.frames[0].poses = vec![
            SmdPose {
                bone: 44,
                position: [0.0; 3],
                rotation: [0.0; 3],
            },
            SmdPose {
                bone: 7,
                position: [0.0; 3],
                rotation: [0.0; 3],
            },
        ];
        for vertex in appended
            .smd
            .triangles
            .iter_mut()
            .flat_map(|t| &mut t.vertices)
        {
            vertex.parent_bone = 7;
            vertex.links[0].bone = 7;
        }
        appended.shape_keys.push(crate::fbx::FbxShapeKey {
            name: "bend".into(),
            vertex_index: vec![0],
            position_offsets: vec![[1.0, 0.0, 0.0]],
            normal_offsets: vec![[1.0, 0.0, 0.0]],
        });
        let original_point = original.smd.triangles[0].vertices[0].clone();
        let spec = AppendSource {
            source: "extra.obj".into(),
            input_scale: 1.0,
            offset: Some(SourceOffset {
                position: [10.0, 20.0, 30.0],
                rotation: [0.0, 90.0, 0.0],
                scale: 2.0,
            }),
        };
        append_geometry(&mut original, appended, &spec).unwrap();
        assert_eq!(original.smd.triangles.len(), 2);
        assert_eq!(original.smd.triangles[0].vertices[0], original_point);
        assert_eq!(original.smd.nodes[1].index, 10);
        assert_eq!(original.smd.nodes[1].parent, 9);
        let vertex = &original.smd.triangles[1].vertices[0];
        for (actual, expected) in vertex.position.into_iter().zip([6.0, 22.0, 36.0]) {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert_eq!(vertex.links[0].bone, 10);
        assert_eq!(vertex.src_index, 3);
        assert_eq!(original.shape_keys[0].vertex_index, [3]);
        for (actual, expected) in original.shape_keys[0].position_offsets[0]
            .into_iter()
            .zip([0.0, 2.0, 0.0])
        {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert_eq!(original.smd.frames[0].poses.last().unwrap().bone, 10);
    }

    #[test]
    fn virtual_vta_recovers_obj_frames_in_source_coordinates_without_writing_source_files() {
        let dir = std::env::temp_dir().join(format!("mdlc-virtual-vta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let changed = TRIANGLE.replacen("v 1 2 3", "v 1 2 4", 1);
        std::fs::write(dir.join("base.obj"), TRIANGLE).unwrap();
        std::fs::write(dir.join("shape.obj"), &changed).unwrap();
        let spec = VirtualVta {
            name: "virtual.vta".into(),
            frames: vec![
                VirtualVtaFrame {
                    source: "base.obj".into(),
                    frame: 0,
                },
                VirtualVtaFrame {
                    source: "shape.obj".into(),
                    frame: 1,
                },
            ],
        };
        let vta = load_virtual_vta(&spec, &dir, 2.0).unwrap();
        assert_eq!(vta.num_vertices, 3);
        assert_eq!(vta.frames[0][0].pos, [2.0, -6.0, 4.0]);
        assert_eq!(vta.frames[1][0].pos, [2.0, -8.0, 4.0]);
        assert_eq!(vta.frames[0][0].normal, [0.0, -1.0, 0.0]);
        assert!(!dir.join("virtual.vta").exists());
        std::fs::write(
            dir.join("shape.obj"),
            changed.replace("1/1/1 2/1/1 3/1/1", "1/1/1 3/1/1 2/1/1"),
        )
        .unwrap();
        assert!(load_virtual_vta(&spec, &dir, 1.0).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod output_tests {
    fn model(source: &str, extra: &str) -> crate::model::ModelDesc {
        crate::model::ModelDesc::from_toml(&format!(
            r#"
[model]
name = "models/source_support.mdl"
version = 48
{extra}
[materials]
textures = [{{name="paint"}}]
[[bones]]
name = "default"
[[bodyparts]]
name = "body"
[[bodyparts.models]]
smd = "{source}"
"#
        ))
        .unwrap()
    }

    #[test]
    fn appended_obj_geometry_reaches_compiled_vertex_and_index_buffers() {
        let dir = std::env::temp_dir().join(format!("mdlc-append-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nusemtl paint\nf 1//1 2//1 3//1\n";
        std::fs::write(dir.join("base.obj"), source).unwrap();
        std::fs::write(dir.join("extra.obj"), source).unwrap();
        let desc = model(
            "base.obj",
            r#"append_sources = [{source="extra.obj",offset={position=[10,20,30],rotation=[0,90,0],scale=2}}]"#,
        );
        let output = crate::pipeline::build(&desc, &dir, Default::default()).unwrap();
        assert_eq!(output.compiled.total_triangles(), 2);
        assert_eq!(output.compiled.total_vertices(), 6);
        let mesh = &output.compiled.bodyparts[0].models[0].meshes[0];
        assert!(mesh.vertices.iter().any(|v| {
            v.pos
                .into_iter()
                .zip([10.0, 22.0, 30.0])
                .all(|(a, b)| (a - b).abs() < 1e-5)
        }));
        assert_eq!(
            u32::from_le_bytes(output.vvd[16..20].try_into().unwrap()),
            6
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn qc_spherical_normals_reach_the_vvd_vertex_records() {
        let dir = std::env::temp_dir().join(format!("mdlc-sphere-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mesh.obj"),
            "v 1 2 2\nv 2 1 2\nv 2 2 1\nvn 0 0 1\nusemtl paint\nf 1//1 2//1 3//1\n",
        )
        .unwrap();
        let desc = crate::qc::parse_qc_str(
            "$modelname sphere.mdl\n$model body mesh.obj { spherenormals paint 0 0 0 }\n",
            &dir,
        )
        .unwrap();
        let output = crate::pipeline::build(&desc, &dir, Default::default()).unwrap();
        let start = u32::from_le_bytes(output.vvd[56..60].try_into().unwrap()) as usize;
        let float = |offset| f32::from_le_bytes(output.vvd[offset..offset + 4].try_into().unwrap());
        for vertex in 0..3 {
            let p =
                std::array::from_fn::<_, 3, _>(|axis| float(start + vertex * 48 + 16 + axis * 4));
            let n =
                std::array::from_fn::<_, 3, _>(|axis| float(start + vertex * 48 + 28 + axis * 4));
            for (actual, position) in n.into_iter().zip(p) {
                assert!((actual - position / 3.0).abs() < 1e-6);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn virtual_vta_produces_compiled_flex_deltas_without_a_vta_file() {
        let dir = std::env::temp_dir().join(format!("mdlc-fake-vta-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = "v 1 2 3\nv 4 2 3\nv 1 5 3\nvn 0 0 1\nf 1//1 2//1 3//1\n";
        std::fs::write(dir.join("base.obj"), obj).unwrap();
        std::fs::write(dir.join("shape.obj"), obj.replacen("v 1 2 3", "v 1 2 4", 1)).unwrap();
        std::fs::write(dir.join("mesh.smd"),"version 1\nnodes\n0 \"default\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\npaint\n0 1 -3 2 0 -1 0 0 0 1 0 1\n0 1 -3 5 0 -1 0 0 0 1 0 1\n0 4 -3 2 0 -1 0 0 0 1 0 1\nend\n").unwrap();
        let mut desc = model(
            "mesh.smd",
            r#"virtual_vtas = [{name="virtual.vta",frames=[{source="base.obj",frame=0},{source="shape.obj",frame=1}]}]"#,
        );
        desc.bodyparts[0].models[0]
            .flexes
            .push(toml::from_str("vta='virtual.vta'\nname='moved'\nframe=1").unwrap());
        let output = crate::pipeline::build(&desc, &dir, Default::default()).unwrap();
        let flexes = &output.compiled.bodyparts[0].models[0].mesh_flexes[0];
        assert_eq!(flexes.len(), 1);
        assert_eq!(flexes[0].vertanims.len(), 1);
        let delta = flexes[0].vertanims[0].delta;
        for (actual, expected) in delta.into_iter().zip([0.0, -1.0, 0.0]) {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert!(!dir.join("virtual.vta").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
