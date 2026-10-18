use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UvChecks {
    #[serde(default)]
    pub normalized: bool,
    #[serde(default)]
    pub overlap: bool,
    #[serde(default)]
    pub inverse: bool,
    // Source parses gutter settings, but CheckGutter is an unimplemented no-op.
    #[serde(default)]
    pub gutter: Option<[u32; 2]>,
}

impl UvChecks {
    pub fn all() -> Self {
        Self {
            normalized: true,
            overlap: true,
            inverse: true,
            gutter: Some([512, 5]),
        }
    }
}

type Point = [f64; 2];
type Face = [Point; 3];
const EPSILON: f64 = 1e-12;

fn side(a: Point, b: Point, p: Point) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn intersection_area(a: Face, b: Face) -> f64 {
    let orientation = side(b[0], b[1], b[2]).signum();
    if side(a[0], a[1], a[2]).abs() <= EPSILON || orientation == 0.0 {
        return 0.0;
    }
    let mut polygon = a.to_vec();
    for edge in 0..3 {
        if polygon.is_empty() {
            return 0.0;
        }
        let (start, end) = (b[edge], b[(edge + 1) % 3]);
        let input = std::mem::take(&mut polygon);
        let mut previous = *input.last().unwrap();
        let mut previous_side = side(start, end, previous) * orientation;
        for current in input {
            let current_side = side(start, end, current) * orientation;
            if (current_side >= -EPSILON) != (previous_side >= -EPSILON) {
                let t = previous_side / (previous_side - current_side);
                polygon.push([
                    previous[0] + (current[0] - previous[0]) * t,
                    previous[1] + (current[1] - previous[1]) * t,
                ]);
            }
            if current_side >= -EPSILON {
                polygon.push(current);
            }
            previous = current;
            previous_side = current_side;
        }
    }
    (0..polygon.len())
        .map(|i| {
            let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f64>()
        .abs()
        * 0.5
}

fn check_faces(faces: &[Face], options: &UvChecks) -> Result<(), String> {
    for (index, face) in faces.iter().enumerate() {
        if face.iter().flatten().any(|value| !value.is_finite()) {
            return Err(format!("face {index} has nonfinite UV coordinates"));
        }
        if options.normalized
            && face
                .iter()
                .flatten()
                .any(|value| !(0.0..=1.0).contains(value))
        {
            return Err(format!("face {index} has UV coordinates outside [0, 1]"));
        }
        if options.inverse && side(face[0], face[1], face[2]) < 0.0 {
            return Err(format!("face {index} has inverse UV winding"));
        }
    }
    if options.overlap {
        let mut bounds: Vec<_> = faces
            .iter()
            .enumerate()
            .filter(|(_, face)| side(face[0], face[1], face[2]).abs() > EPSILON)
            .map(|(index, face)| {
                (
                    index,
                    std::array::from_fn::<_, 2, _>(|axis| {
                        face.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min)
                    }),
                    std::array::from_fn::<_, 2, _>(|axis| {
                        face.iter()
                            .map(|p| p[axis])
                            .fold(f64::NEG_INFINITY, f64::max)
                    }),
                )
            })
            .collect();
        bounds.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]).then(a.0.cmp(&b.0)));
        for (i, &(left, min, max)) in bounds.iter().enumerate() {
            for &(right, other_min, other_max) in bounds.iter().skip(i + 1) {
                if other_min[0] >= max[0] - EPSILON {
                    break;
                }
                if other_min[1] >= max[1] - EPSILON || min[1] >= other_max[1] - EPSILON {
                    continue;
                }
                if intersection_area(faces[left], faces[right]) > EPSILON {
                    return Err(format!("UV faces {left} and {right} overlap"));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate(compiled: &crate::model::CompiledModelDesc) -> Result<(), String> {
    let Some(options) = &compiled.desc.model.check_uv else {
        return Ok(());
    };
    if options.gutter.is_some_and(|values| values.contains(&0)) {
        return Err("gutter resolution and minimum must be positive".into());
    }
    for body in &compiled.bodyparts {
        for model in &body.models {
            for (index, mesh) in model.meshes.iter().enumerate() {
                let material = compiled
                    .desc
                    .materials
                    .textures
                    .get(mesh.material)
                    .map_or("unknown", |t| t.name.as_str());
                let check = |vertices: &[crate::model::Vertex],
                             triangles: &[[u32; 3]],
                             level: usize|
                 -> Result<(), String> {
                    let faces: Vec<Face> = triangles
                        .iter()
                        .map(|triangle| {
                            let mut face = [[0.0; 2]; 3];
                            for (corner, &vertex) in triangle.iter().enumerate() {
                                face[corner] = vertices
                                    .get(vertex as usize)
                                    .ok_or("UV face references a missing vertex")?
                                    .uv
                                    .map(f64::from);
                            }
                            Ok(face)
                        })
                        .collect::<Result<_, String>>()?;
                    check_faces(&faces, options).map_err(|error| {
                        format!(
                            "model {:?}, material {material:?}, LOD {level}: {error}",
                            model.name
                        )
                    })
                };
                if let Some(lods) = model.lods.as_ref().and_then(|lods| lods.meshes.get(index)) {
                    for (level, triangles) in lods.triangles.iter().enumerate() {
                        check(&lods.vertices, triangles, level)?;
                    }
                } else {
                    check(&mesh.vertices, &mesh.triangles, 0)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_and_inverse_checks_reject_the_requested_defects() {
        let positive = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let normalized = UvChecks {
            normalized: true,
            ..Default::default()
        };
        assert!(check_faces(&[positive], &normalized).is_ok());
        assert!(
            check_faces(&[[[-0.01, 0.0], [1.0, 0.0], [0.0, 1.0]]], &normalized)
                .unwrap_err()
                .contains("outside")
        );
        let inverse = UvChecks {
            inverse: true,
            ..Default::default()
        };
        assert!(check_faces(&[positive], &inverse).is_ok());
        assert!(
            check_faces(&[[positive[0], positive[2], positive[1]]], &inverse)
                .unwrap_err()
                .contains("inverse")
        );
        assert!(check_faces(&[[[0.0, 0.0]; 3]], &inverse).is_ok());
    }

    #[test]
    fn overlap_detects_containment_crossings_and_duplicates_but_ignores_boundary_contact() {
        let options = UvChecks {
            overlap: true,
            ..Default::default()
        };
        let a = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        for b in [
            a,
            [[0.1, 0.1], [0.2, 0.1], [0.1, 0.2]],
            [[0.4, -0.1], [0.4, 1.1], [0.6, 1.1]],
        ] {
            assert!(
                check_faces(&[a, b], &options)
                    .unwrap_err()
                    .contains("overlap")
            );
            assert!(
                check_faces(&[b, a], &options)
                    .unwrap_err()
                    .contains("overlap")
            );
        }
        for b in [
            [[1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            [[1.0, 0.0], [2.0, 0.0], [1.0, 1.0]],
            [[2.0, 0.0], [3.0, 0.0], [2.0, 1.0]],
            [[0.0, 0.0]; 3],
        ] {
            assert!(check_faces(&[a, b], &options).is_ok());
        }
    }
}

#[cfg(test)]
mod output_tests {
    fn build(
        dir: &std::path::Path,
        options: &str,
    ) -> Result<crate::pipeline::PipelineOutput, crate::pipeline::PipelineError> {
        let desc = crate::qc::parse_qc_str(
            &format!("$modelname uv.mdl\n$body body mesh.obj\n$checkuv {options}\n"),
            dir,
        )
        .unwrap();
        crate::pipeline::build(&desc, dir, Default::default())
    }

    #[test]
    fn qc_uv_checks_fail_compilation_for_requested_defects_and_keep_materials_separate() {
        let dir = std::env::temp_dir().join(format!("mdlc-uv-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl one\nf 1/1/1 2/2/1 3/3/1\n";
        std::fs::write(dir.join("mesh.obj"), source).unwrap();
        let output = build(&dir, "0to1 inverse overlap").unwrap();
        let triangle = output.compiled.bodyparts[0].models[0].meshes[0].triangles[0];
        let vertices = &output.compiled.bodyparts[0].models[0].meshes[0].vertices;
        assert_eq!(
            triangle.map(|i| vertices[i as usize].pos),
            [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]
        );
        std::fs::write(dir.join("mesh.obj"), source.replace("vt 1 0", "vt 1.1 0")).unwrap();
        assert!(
            build(&dir, "0to1")
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        std::fs::write(
            dir.join("mesh.obj"),
            source.replace("1/1/1 2/2/1 3/3/1", "1/1/1 3/3/1 2/2/1"),
        )
        .unwrap();
        assert!(
            build(&dir, "inverse")
                .unwrap_err()
                .to_string()
                .contains("inverse")
        );
        std::fs::write(
            dir.join("mesh.obj"),
            format!("{source}f 1/1/1 2/2/1 3/3/1\n"),
        )
        .unwrap();
        assert!(
            build(&dir, "overlap")
                .unwrap_err()
                .to_string()
                .contains("overlap")
        );
        std::fs::write(
            dir.join("mesh.obj"),
            format!("{source}usemtl two\nf 1/1/1 2/2/1 3/3/1\n"),
        )
        .unwrap();
        assert!(build(&dir, "overlap").is_ok());
        assert!(build(&dir, "gutter 1024 8").is_ok());
        for invalid in ["gutter 0", "gutter 10 0", "unknown"] {
            assert!(crate::qc::parse_qc_str(&format!("$checkuv {invalid}\n"), &dir).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
