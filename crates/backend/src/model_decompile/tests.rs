use super::*;
use mdlc::{pipeline, qc};

fn fixture() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("rig.smd"),
        include_str!("../../../vendor/mdlc/tests/fixtures/rig.smd").replace(
            "0 -2 -3 -4 0 0 1 0 0 1 0 1",
            "0 -2 -3 -4 0 0 1 0 0 2 0 0.75 1 0.25",
        ),
    )
    .unwrap();
    let qc_path = project.path().join("input.qc");
    fs::write(&qc_path, "$modelname models/tests/export.mdl\n$cdmaterials models/tests\n$surfaceprop metal\n$eyeposition 1 2 3\n$illumposition 4 5 6\n$bodygroup parts\n{\n studio rig.smd\n blank\n}\n$sequence idle rig.smd\n").unwrap();
    let desc = qc::parse_qc_file(&qc_path).unwrap();
    let out = pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&out, project.path()).unwrap();
    project
}

#[test]
fn reference_export_preserves_bones_weights_uvs_and_front_faces() {
    let project = fixture();
    let source = project.path().join("models/tests/export.mdl");
    let dest = project.path().join("decompiled");
    let report = decompile_model(&source, &dest).unwrap();
    let smd =
        mdlc::smd::parse_smd(&fs::read_to_string(dest.join("reference_0_0.smd")).unwrap()).unwrap();
    let original =
        mdlc::smd::parse_smd(&fs::read_to_string(project.path().join("rig.smd")).unwrap()).unwrap();
    assert!(
        original
            .triangles
            .iter()
            .flat_map(|face| &face.vertices)
            .any(|vertex| vertex.links.len() == 2)
    );
    assert_eq!(smd.nodes, original.nodes);
    assert_eq!(smd.frames, original.frames);
    assert_eq!(smd.triangles.len(), original.triangles.len());
    let mut unmatched: Vec<_> = original.triangles.iter().collect();
    for face in &smd.triangles {
        let index = unmatched
            .iter()
            .position(|other| {
                face.material == other.material
                    && (0..3).any(|start| {
                        (0..3).all(|corner| {
                            let a = &face.vertices[corner];
                            let b = &other.vertices[(start + corner) % 3];
                            a.position == b.position
                                && a.normal == b.normal
                                && a.uv == b.uv
                                && a.links == b.links
                        })
                    })
            })
            .expect("triangle attributes or winding changed during decompilation");
        unmatched.swap_remove(index);
    }
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(&desc, &dest, pipeline::PipelineOptions::default()).unwrap();
    let original_mdl = fs::read(&source).unwrap();
    assert_eq!(&original_mdl[80..152], &rebuilt.mdl[80..152]);
    assert!(
        fs::read_to_string(&report.qc_path)
            .unwrap()
            .contains("$surfaceprop \"metal\"")
    );
    let rebuilt_mdl = mdl::parse_mdl(&rebuilt.mdl, &vformats::Limits::default()).unwrap();
    let source_mdl = mdl::parse_mdl(&original_mdl, &vformats::Limits::default()).unwrap();
    assert_eq!(
        rebuilt_mdl.body_parts[0].models[0].name,
        source_mdl.body_parts[0].models[0].name
    );
    assert_eq!(rebuilt_mdl.body_parts[0].models.len(), 2);
    assert!(rebuilt_mdl.body_parts[0].models[1].meshes.is_empty());
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("Animation sequences"))
    );
    assert_eq!(
        report.bytes_written,
        report
            .files
            .iter()
            .map(|path| fs::metadata(path).unwrap().len())
            .sum::<u64>()
    );
}

#[test]
fn checksum_mismatch_leaves_no_partial_export() {
    let project = fixture();
    let source = project.path().join("models/tests/export.mdl");
    let vvd = source.with_extension("vvd");
    let mut bytes = fs::read(&vvd).unwrap();
    bytes[8] ^= 1;
    fs::write(vvd, bytes).unwrap();
    let destination = project.path().join("bad");
    assert!(matches!(
        decompile_model(&source, &destination),
        Err(DecompileError::InvalidModel(_))
    ));
    assert!(!destination.exists());
    assert!(!fs::read_dir(project.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".model-export-")
    }));
}

#[test]
fn cancellation_and_existing_destination_preserve_files() {
    let project = fixture();
    let source = project.path().join("models/tests/export.mdl");
    let destination = project.path().join("existing");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), "original").unwrap();
    assert!(matches!(
        decompile_model(&source, &destination),
        Err(DecompileError::DestinationExists(_))
    ));
    assert_eq!(
        fs::read_to_string(destination.join("keep")).unwrap(),
        "original"
    );
    let destination = project.path().join("cancelled");
    assert_eq!(
        decompile_model_with_cancel(&source, &destination, &AtomicBool::new(true)),
        Err(DecompileError::Cancelled)
    );
    assert!(!destination.exists());
}

#[test]
fn companion_lookup_handles_uppercase_names() {
    let project = fixture();
    let source = project.path().join("models/tests/export.mdl");
    fs::rename(source.with_extension("vvd"), source.with_extension("VVD")).unwrap();
    fs::rename(
        source.with_extension("dx90.vtx"),
        source.with_extension("DX90.VTX"),
    )
    .unwrap();
    assert!(decompile_model(&source, &project.path().join("decompiled")).is_ok());
}

#[test]
fn unsafe_names_cannot_inject_qc_or_escape_output() {
    assert!(safe_model_name("../escape.mdl").is_err());
    assert!(safe_model_name("C:/escape.mdl").is_err());
    assert!(quoted("safe\"\n$include bad.qci").is_err());
    assert!(validate_material("material\nend").is_err());
}

fn lod_fixture() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let skeleton = "version 1\nnodes\n0 \"root\" -1\n1 \"detail\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 5 0 0 0\nend\ntriangles\n";
    let triangle = |material: &str, shift: i32| {
        format!(
            "{material}\n0 {shift} 0 0 0 0 1 0.2 0.3 2 0 0.75 1 0.25\n0 {} 0 0 0 0 1 0.7 0.3 1 0 1\n1 {shift} 4 0 0 0 1 0.2 0.9 1 1 1\n",
            shift + 3
        )
    };
    for (name, triangles) in [
        ("high", triangle("paint", 0) + &triangle("trim", 10)),
        ("low", triangle("paint", 1)),
        ("other", triangle("paint", 20) + &triangle("trim", 30)),
        ("other_low", triangle("paint", 22) + &triangle("trim", 32)),
        ("empty", String::new()),
    ] {
        fs::write(
            project.path().join(format!("{name}.smd")),
            format!("{skeleton}{triangles}end\n"),
        )
        .unwrap();
    }
    let path = project.path().join("input.qc");
    fs::write(&path, "$modelname models/tests/lods.mdl\n$bodygroup choices {\n studio high.smd\n blank\n studio other.smd\n}\n$body extra other_low.smd\n$sequence idle high.smd\n$texturegroup skins { { paint trim } { red chrome } }\n$lod 25 { replacemodel high.smd low.smd replacemodel other.smd other_low.smd }\n$lod 75 { replacemodel high.smd empty.smd replacemodel other.smd low.smd }\n").unwrap();
    let desc = qc::parse_qc_file(&path).unwrap();
    let output =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&output, project.path()).unwrap();
    project
}

fn lod_geometry(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> Vec<Vec<Vec<Vec<String>>>> {
    let limits = vformats::Limits::default();
    let model = mdl::parse_mdl(mdl, &limits).unwrap();
    let vertices = mdl::parse_vvd(vvd, &limits).unwrap();
    let topology = mdl::parse_vtx(vtx, &limits).unwrap();
    let metadata = lods::Metadata::read(mdl, vvd, vtx, &model, &vertices, &topology).unwrap();
    assert_eq!(metadata.switch_points.len(), 3);
    model
        .body_parts
        .iter()
        .zip(&topology.body_parts)
        .enumerate()
        .map(|(group, (part, vtx_part))| {
            part.models
                .iter()
                .zip(&vtx_part.models)
                .enumerate()
                .map(|(choice, (source, vtx))| {
                    vtx.lods
                        .iter()
                        .map(|lod| {
                            let text = reference_smd(
                                &model,
                                source,
                                lod,
                                &vertices,
                                &metadata.mesh_vertices[group][choice],
                                &AtomicBool::new(false),
                            )
                            .unwrap();
                            let smd = mdlc::smd::parse_smd(&text).unwrap();
                            let mut triangles: Vec<_> = smd
                                .triangles
                                .iter()
                                .map(|triangle| {
                                    let corners: Vec<_> = triangle
                                        .vertices
                                        .iter()
                                        .map(|vertex| {
                                            format!(
                                                "{:?}/{:?}/{:?}/{:?}",
                                                vertex.position,
                                                vertex.normal,
                                                vertex.uv,
                                                vertex.links
                                            )
                                        })
                                        .collect();
                                    let face = (0..3)
                                        .map(|start| {
                                            format!(
                                                "{}|{}|{}",
                                                corners[start],
                                                corners[(start + 1) % 3],
                                                corners[(start + 2) % 3]
                                            )
                                        })
                                        .min()
                                        .unwrap();
                                    format!("{}:{face}", triangle.material)
                                })
                                .collect();
                            triangles.sort();
                            triangles
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

#[test]
fn lod_roundtrip_preserves_switches_removed_meshes_bodygroups_skins_and_weights() {
    let project = lod_fixture();
    let source = project.path().join("models/tests/lods.mdl");
    let mdl = fs::read(&source).unwrap();
    let vvd = fs::read(source.with_extension("vvd")).unwrap();
    let vtx = fs::read(source.with_extension("dx90.vtx")).unwrap();
    assert!(
        u32_at(&vvd, 48).unwrap() > 1,
        "exercise nontrivial VVD fixups"
    );
    let original = lod_geometry(&mdl, &vvd, &vtx);
    assert!(
        original[0][0][0]
            .iter()
            .any(|face| face.starts_with("paint:[0.0, 0.0, 0.0]"))
    );
    assert!(
        original[0][2][0]
            .iter()
            .any(|face| face.starts_with("paint:[20.0, 0.0, 0.0]"))
    );
    assert_eq!(original[0][0][0].len(), 2);
    assert_eq!(original[0][0][1].len(), 1);
    assert!(original[0][0][2].is_empty());
    assert!(original[0][1].iter().all(Vec::is_empty));
    let destination = project.path().join("decompiled");
    let report = decompile_model(&source, &destination).unwrap();
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("LOD 0"))
    );
    let qc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(&qc, &destination, pipeline::PipelineOptions::default()).unwrap();
    assert_eq!(
        original,
        lod_geometry(&rebuilt.mdl, &rebuilt.vvd, &rebuilt.vtx)
    );
    let limits = vformats::Limits::default();
    let original_model = mdl::parse_mdl(&mdl, &limits).unwrap();
    let rebuilt_model = mdl::parse_mdl(&rebuilt.mdl, &limits).unwrap();
    assert_eq!(original_model.skin_table, rebuilt_model.skin_table);
    assert_eq!(original_model.textures, rebuilt_model.textures);
    let checksum = u32_at(&rebuilt.mdl, 8).unwrap();
    assert_eq!(u32_at(&rebuilt.vvd, 8), Some(checksum));
    assert_eq!(u32_at(&rebuilt.vtx, 16), Some(checksum));
}

#[test]
fn malformed_lod_metadata_cannot_publish_partial_output() {
    for corruption in [
        "fixup_lod",
        "fixup_overlap",
        "lod_count",
        "switch_nan",
        "mesh_vertex",
    ] {
        let project = lod_fixture();
        let source = project.path().join("models/tests/lods.mdl");
        let path = source.with_extension(if corruption.starts_with("fixup") {
            "vvd"
        } else {
            "dx90.vtx"
        });
        let mut bytes = fs::read(&path).unwrap();
        match corruption {
            "fixup_lod" => {
                let base = u32_at(&bytes, 52).unwrap() as usize;
                bytes[base..base + 4].copy_from_slice(&99i32.to_le_bytes());
            }
            "fixup_overlap" => {
                let base = u32_at(&bytes, 52).unwrap() as usize;
                let source = bytes[base + 4..base + 8].to_vec();
                bytes[base + 16..base + 20].copy_from_slice(&source);
            }
            "lod_count" => bytes[20..24].copy_from_slice(&8i32.to_le_bytes()),
            "switch_nan" | "mesh_vertex" => {
                let body = u32_at(&bytes, 32).unwrap() as usize;
                let model = body + u32_at(&bytes, body + 4).unwrap() as usize;
                let lod = model + u32_at(&bytes, model + 4).unwrap() as usize;
                if corruption == "switch_nan" {
                    bytes[lod + 20..lod + 24].copy_from_slice(&f32::NAN.to_le_bytes());
                } else {
                    let mesh = lod + u32_at(&bytes, lod + 4).unwrap() as usize;
                    let group = mesh + u32_at(&bytes, mesh + 4).unwrap() as usize;
                    let vertex = group + u32_at(&bytes, group + 4).unwrap() as usize;
                    bytes[vertex + 4..vertex + 6].copy_from_slice(&u16::MAX.to_le_bytes());
                }
            }
            _ => unreachable!(),
        }
        fs::write(path, bytes).unwrap();
        let destination = project.path().join("bad");
        assert!(
            matches!(
                decompile_model(&source, &destination),
                Err(DecompileError::InvalidModel(_))
            ),
            "{corruption}"
        );
        assert!(!destination.exists());
    }
}

#[test]
fn shadow_lod_material_replacements_and_model_removals_roundtrip() {
    let project = lod_fixture();
    let input = project.path().join("input.qc");
    let qc_text = fs::read_to_string(&input).unwrap()
        .replace("$lod 25 {", "$lod 25 { replacematerial paint low_paint removemesh models/tests/trim")
        .replace("$lod 75 { replacemodel high.smd empty.smd replacemodel other.smd low.smd }", "$shadowlod { removemodel high.smd replacematerial paint shadow_paint use_shadowlod_materials nofacial }");
    fs::write(&input, format!("$cdmaterials models/tests\n{qc_text}")).unwrap();
    let desc = qc::parse_qc_file(&input).unwrap();
    let original =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&original, project.path()).unwrap();
    let destination = project.path().join("shadow_export");
    let report =
        decompile_model(&project.path().join("models/tests/lods.mdl"), &destination).unwrap();
    let exported_qc = fs::read_to_string(&report.qc_path).unwrap();
    assert!(exported_qc.contains("$shadowlod\n{"));
    assert!(exported_qc.contains("removemodel \"reference_0_0.smd\""));
    assert!(exported_qc.contains("replacematerial \"paint\" \"low_paint\""));
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt =
        pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
    assert_eq!(
        lod_geometry(&original.mdl, &original.vvd, &original.vtx),
        lod_geometry(&rebuilt.mdl, &rebuilt.vvd, &rebuilt.vtx)
    );
    let read_metadata = |mdl: &[u8], vvd: &[u8], vtx: &[u8]| {
        let limits = vformats::Limits::default();
        let model = mdl::parse_mdl(mdl, &limits).unwrap();
        let vertices = mdl::parse_vvd(vvd, &limits).unwrap();
        let topology = mdl::parse_vtx(vtx, &limits).unwrap();
        lods::Metadata::read(mdl, vvd, vtx, &model, &vertices, &topology).unwrap()
    };
    let original_metadata = read_metadata(&original.mdl, &original.vvd, &original.vtx);
    let rebuilt_metadata = read_metadata(&rebuilt.mdl, &rebuilt.vvd, &rebuilt.vtx);
    assert_eq!(original_metadata.switch_points, [0.0, 25.0, -1.0]);
    assert_eq!(
        original_metadata.switch_points,
        rebuilt_metadata.switch_points
    );
    assert_eq!(
        original_metadata.material_replacements,
        rebuilt_metadata.material_replacements
    );
    assert!(rebuilt_metadata.shadow_materials);
    assert_eq!(
        u32_at(&original.mdl, 152).unwrap() & 0x140,
        u32_at(&rebuilt.mdl, 152).unwrap() & 0x140
    );
}

#[test]
fn static_lod_roundtrip_keeps_source_orientation_and_bounds() {
    let project = lod_fixture();
    let input = project.path().join("input.qc");
    let text = format!("$staticprop\n{}", fs::read_to_string(&input).unwrap());
    fs::write(&input, text).unwrap();
    let desc = qc::parse_qc_file(&input).unwrap();
    let original =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&original, project.path()).unwrap();
    let destination = project.path().join("static_lods");
    let report =
        decompile_model(&project.path().join("models/tests/lods.mdl"), &destination).unwrap();
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt =
        pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
    assert_eq!(
        lod_geometry(&original.mdl, &original.vvd, &original.vtx),
        lod_geometry(&rebuilt.mdl, &rebuilt.vvd, &rebuilt.vtx)
    );
    assert_eq!(&original.mdl[80..152], &rebuilt.mdl[80..152]);
}

#[test]
fn malformed_lod_replacement_lists_fail_without_partial_exports() {
    let project = lod_fixture();
    let input = project.path().join("input.qc");
    let text = fs::read_to_string(&input)
        .unwrap()
        .replace("$lod 25 {", "$lod 25 { replacematerial paint low_paint");
    fs::write(&input, text).unwrap();
    let desc = qc::parse_qc_file(&input).unwrap();
    let output =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&output, project.path()).unwrap();
    let source = project.path().join("models/tests/lods.mdl");
    let vtx_path = source.with_extension("dx90.vtx");
    let lists = u32_at(&output.vtx, 24).unwrap() as usize;
    let header = lists + 8;
    let record = header + u32_at(&output.vtx, header + 4).unwrap() as usize;
    for corruption in ["slot", "name", "count", "shadow_flag"] {
        let mut vtx = output.vtx.clone();
        let mut mdl = output.mdl.clone();
        match corruption {
            "slot" => vtx[record..record + 2].copy_from_slice(&i16::MAX.to_le_bytes()),
            "name" => vtx[record + 2..record + 6].copy_from_slice(&i32::MAX.to_le_bytes()),
            "count" => vtx[header..header + 4].copy_from_slice(&i32::MAX.to_le_bytes()),
            "shadow_flag" => {
                let flags = u32_at(&mdl, 152).unwrap() | 0x40;
                mdl[152..156].copy_from_slice(&flags.to_le_bytes());
            }
            _ => unreachable!(),
        }
        fs::write(&vtx_path, vtx).unwrap();
        fs::write(&source, mdl).unwrap();
        let destination = project.path().join(corruption);
        assert!(
            matches!(
                decompile_model(&source, &destination),
                Err(DecompileError::InvalidModel(_) | DecompileError::SizeLimit)
            ),
            "{corruption}"
        );
        assert!(!destination.exists());
    }
}

#[test]
fn lod_replacement_reverse_corrects_winding_without_flipping_normals() {
    let project = lod_fixture();
    let input = project.path().join("input.qc");
    let text = fs::read_to_string(&input).unwrap().replace(
        "replacemodel high.smd low.smd",
        "replacemodel high.smd high.smd reverse",
    );
    fs::write(&input, text).unwrap();
    let desc = qc::parse_qc_file(&input).unwrap();
    assert!(desc.bodyparts[0].models[0].lods[0].reverse);
    let output =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let limits = vformats::Limits::default();
    let model = mdl::parse_mdl(&output.mdl, &limits).unwrap();
    let vertices = mdl::parse_vvd(&output.vvd, &limits).unwrap();
    let topology = mdl::parse_vtx(&output.vtx, &limits).unwrap();
    let metadata = lods::Metadata::read(
        &output.mdl,
        &output.vvd,
        &output.vtx,
        &model,
        &vertices,
        &topology,
    )
    .unwrap();
    for level in 0..2 {
        let text = reference_smd(
            &model,
            &model.body_parts[0].models[0],
            &topology.body_parts[0].models[0].lods[level],
            &vertices,
            &metadata.mesh_vertices[0][0],
            &AtomicBool::new(false),
        )
        .unwrap();
        let smd = mdlc::smd::parse_smd(&text).unwrap();
        for triangle in &smd.triangles {
            let [a, b, c] = triangle.vertices.each_ref().map(|vertex| vertex.position);
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            assert!(if level == 0 { area > 0.0 } else { area < 0.0 });
            assert!(
                triangle
                    .vertices
                    .iter()
                    .all(|vertex| vertex.normal == [0.0, 0.0, 1.0])
            );
        }
    }
}

#[test]
fn preview_lods_preserve_actual_geometry_materials_bodygroups_and_animated_bindings() {
    let project = lod_fixture();
    let path = project.path().join("input.qc");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("$lod 25 {", "$lod 25 { replacematerial paint low_paint")
        + "$sequence moving motion.smd fps 2 noautoik\n";
    fs::write(project.path().join("motion.smd"), "version 1\nnodes\n0 \"root\" -1\n1 \"detail\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 5 0 0 0\ntime 1\n0 8 0 0 0 0 0\n1 0 0 5 0 0 0\nend\n").unwrap();
    fs::write(&path, &source).unwrap();
    let build = |minimum| {
        let desc =
            qc::parse_qc_str(&format!("{source}\n$minlod {minimum}\n"), project.path()).unwrap();
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap()
    };
    let output = build(0);
    assert!(
        u32_at(&output.vvd, 48).unwrap() > 1,
        "fixture must exercise VVD fixups"
    );
    let preview = load_model_lod_preview(&output.mdl, &output.vvd, &output.vtx).unwrap();
    assert_eq!(preview.root_lod, 0);
    assert_eq!(preview.levels.len(), 3);
    assert_eq!(
        preview
            .levels
            .iter()
            .map(|level| level.triangle_count)
            .collect::<Vec<_>>(),
        [6, 5, 3]
    );
    assert_eq!(preview.levels[0].bodygroups, [3, 1]);
    for level in &preview.levels {
        assert_eq!(level.bodygroups, preview.levels[0].bodygroups);
        assert_eq!(level.skin_tables, preview.levels[0].skin_tables);
        assert_eq!(
            level.vertex_count as usize,
            level
                .meshes
                .iter()
                .map(|mesh| mesh.vertices.len())
                .sum::<usize>()
        );
    }
    assert!(preview.levels[0].material_names.contains(&"paint".into()));
    assert!(
        preview.levels[1]
            .material_names
            .contains(&"low_paint".into())
    );
    assert!(
        preview.levels[2]
            .meshes
            .iter()
            .filter(|mesh| mesh.bodygroup == 0 && mesh.bodygroup_choice == 0)
            .all(|mesh| mesh.indices.is_empty())
    );
    let lower_vertices: Vec<_> = preview.levels[1]
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup == 0 && mesh.bodygroup_choice == 0)
        .flat_map(|mesh| &mesh.vertices)
        .collect();
    let lower_first = lower_vertices
        .iter()
        .find(|vertex| vertex.uv == [0.2, 0.7])
        .unwrap_or_else(|| panic!("Missing lower-only source vertex: {lower_vertices:?}"));
    for (actual, expected) in lower_first.position.into_iter().zip([1.0, 0.0, 0.0]) {
        assert!((actual - expected).abs() < 0.001, "{lower_first:?}");
    }
    assert_eq!(lower_first.normal, [0.0, 0.0, 1.0]);
    let binary = mdl::parse_mdl(&output.mdl, &vformats::Limits::default()).unwrap();
    let translation_step = binary.bones[0].position_scale[1];
    let encoded_translation = (8.0 / translation_step).trunc() * translation_step;
    assert!((encoded_translation - 8.0).abs() <= translation_step);
    let animation = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    for (lod, geometry) in preview.levels.iter().enumerate() {
        assert!(
            animation
                .mesh_vertex_counts_lod(lod)
                .eq(geometry.meshes.iter().map(|mesh| mesh.vertices.len()))
        );
        let animated = animation.evaluate_lod(1, 1.0, &[], lod).unwrap();
        for (mesh, animated) in geometry.meshes.iter().zip(animated) {
            for (vertex, animated) in mesh.vertices.iter().zip(animated) {
                for axis in 0..3 {
                    let expected = [
                        -vertex.position[1],
                        vertex.position[0] + encoded_translation,
                        vertex.position[2],
                    ][axis];
                    assert!(
                        (animated.position[axis] - expected).abs() < 0.002,
                        "LOD {lod}: {animated:?} expected {expected} axis {axis}"
                    );
                    let expected_normal =
                        [-vertex.normal[1], vertex.normal[0], vertex.normal[2]][axis];
                    assert!((animated.normal[axis] - expected_normal).abs() < 0.002);
                }
            }
        }
    }
    assert!(animation.evaluate_lod(1, 1.0, &[], 3).is_none());
    for root in [1, 2] {
        let output = build(root);
        let stripped = load_model_lod_preview(&output.mdl, &output.vvd, &output.vtx).unwrap();
        assert_eq!(stripped.root_lod, root as usize);
        assert_eq!(stripped.levels.len(), 3 - root as usize);
        for (actual, expected) in stripped.levels.iter().zip(&preview.levels[root as usize..]) {
            assert_eq!(actual.meshes, expected.meshes);
            assert_eq!(actual.material_names, expected.material_names);
            assert_eq!(actual.skin_tables, expected.skin_tables);
            assert_eq!(actual.bodygroups, expected.bodygroups);
        }
        let animation =
            load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
        assert!(animation.evaluate_lod(1, 1.0, &[], root as usize).is_some());
        assert!(
            animation
                .evaluate_lod(1, 1.0, &[], root as usize - 1)
                .is_none()
        );
    }
    let mut mismatched = output.vtx.clone();
    mismatched[16] ^= 1;
    assert!(load_model_lod_preview(&output.mdl, &output.vvd, &mismatched).is_err());
    let mut invalid_root = output.mdl.clone();
    invalid_root[0x179] = 8;
    assert!(load_model_lod_preview(&invalid_root, &output.vvd, &output.vtx).is_err());
}
