use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mdlc-qc-compat-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("reference.smd"),
            include_str!("../../tests/fixtures/cube.smd"),
        )
        .unwrap();
        Self(path)
    }

    fn parse(&self, commands: &str) -> crate::model::ModelDesc {
        let mut description = crate::qc::parse_qc_str(
            &format!("$modelname props/output.mdl\n$body body reference.smd\n{commands}"),
            &self.0,
        )
        .unwrap();
        description.model.version = Some(48);
        description
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn internal_name_flags_and_hitgroup_reach_the_binary_without_renaming_output() {
    let fixture = Fixture::new();
    let description =
        fixture.parse("$internalname alternate.mdl\n$obsolete\n$subd\n$hgroup 7 root\n");
    let output = crate::pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let integer =
        |offset: usize| i32::from_le_bytes(output.mdl[offset..offset + 4].try_into().unwrap());
    assert_eq!(output.stem, "props/output");
    assert_eq!(&output.mdl[12..26], b"alternate.mdl\0");
    assert_eq!(integer(152) & (0x200 | (1 << 19)), 0x200 | (1 << 19));
    let sets = integer(176) as usize;
    let boxes =
        sets + i32::from_le_bytes(output.mdl[sets + 8..sets + 12].try_into().unwrap()) as usize;
    assert_eq!(
        i32::from_le_bytes(output.mdl[boxes + 4..boxes + 8].try_into().unwrap()),
        7
    );
}

#[test]
fn attachment_redefinition_keeps_its_index_and_other_attachments() {
    let fixture = Fixture::new();
    let description = fixture.parse("$attachment muzzle root 1 2 3\n$attachment other root 8 9 10\n$redefineattachment MUZZLE root 4 5 6 world_align\n");
    assert_eq!(description.attachments.len(), 2);
    assert_eq!(description.attachments[0].position, Some([4.0, 5.0, 6.0]));
    assert_eq!(description.attachments[0].flags, Some(0x10000));
    assert_eq!(description.attachments[1].name, "other");
    let output = crate::pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let count = i32::from_le_bytes(output.mdl[240..244].try_into().unwrap());
    assert_eq!(count, 2);
    assert!(
        crate::qc::parse_qc_str("$redefineattachment missing root 0 0 0", &fixture.0).unwrap_err()
            [0]
        .message
        .contains("missing attachment")
    );
}

#[test]
fn source_search_and_assertions_follow_qc_order() {
    let fixture = Fixture::new();
    let source = fs::read(fixture.0.join("reference.smd")).unwrap();
    fs::create_dir(fixture.0.join("sources")).unwrap();
    fs::write(fixture.0.join("sources/mesh.smd"), &source).unwrap();
    let qc = "$modelname test.mdl\n$addsearchdir sources\n$qcassert boneexists ROOT mesh true\n$qcassert boneexists missing mesh false\n$qcassert importboneexists root false\n$body body mesh\n";
    let description = crate::qc::parse_qc_str(qc, &fixture.0).unwrap();
    let compiled = crate::compile::compile(&description, &fixture.0).unwrap();
    assert_eq!(compiled.total_vertices(), 8);
    let failure = crate::qc::parse_qc_str(
        "$qcassert boneexists missing reference.smd true",
        &fixture.0,
    )
    .unwrap_err();
    assert_eq!(failure[0].line, 1);
    assert!(failure[0].message.contains("QC assertion failed"));
}

#[test]
fn malformed_ik_ranges_and_world_bounds_are_rejected_at_the_qc_line() {
    for command in [
        "$sequence idle mesh.smd ikrule leg footstep range bad 1 2 3",
        "$clampworldspace 1 0 0 -1 0 0",
        "$clampworldspace nan 0 0 1 1 1",
    ] {
        let errors = crate::qc::parse_qc_str(command, std::path::Path::new(".")).unwrap_err();
        assert_eq!(errors[0].line, 1);
        assert_eq!(errors[0].file, "<memory>");
    }
}

#[test]
fn additional_search_directories_resolve_includes_and_preserve_primary_precedence() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("shared")).unwrap();
    fs::write(
        fixture.0.join("shared/materials.qci"),
        "$surfaceprop metal\n",
    )
    .unwrap();
    let commands = "$addsearchdir shared\n$include materials.qci\n";
    assert_eq!(
        fixture.parse(commands).model.surface_prop.as_deref(),
        Some("metal")
    );
    fs::write(fixture.0.join("materials.qci"), "$surfaceprop wood\n").unwrap();
    assert_eq!(
        fixture.parse(commands).model.surface_prop.as_deref(),
        Some("wood")
    );
}

#[test]
fn dmx_lod_replacements_match_extensionless_source_names() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("reference.dmx"),
        include_bytes!("../../tests/fixtures/dmx.dmx"),
    )
    .unwrap();
    let description = crate::qc::parse_qc_str(
        "$modelname tests/dmx_lod.mdl\n$body body reference.dmx\n$lod 25 { replacemodel reference reference.dmx }\n",
        &fixture.0,
    ).unwrap();
    let output = crate::pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let vertices = vformats::mdl::parse_vvd(&output.vvd, &vformats::Limits::default()).unwrap();
    assert_eq!(
        i32::from_le_bytes(output.vvd[12..16].try_into().unwrap()),
        2
    );
    assert_eq!(vertices.vertices.len(), 3);
    vformats::mdl::parse_vtx(&output.vtx, &vformats::Limits::default()).unwrap();
}

#[test]
fn explicit_and_automatic_mass_follow_qc_order_in_and_outside_collision_blocks() {
    let fixture = Fixture::new();
    for (commands, automatic, mass) in [
        ("$mass 12\n$automass\n", true, None),
        ("$automass\n$mass 12\n", false, Some(12.0)),
        (
            "$collisionmodel reference.smd { $mass 12 $automass }\n",
            true,
            None,
        ),
        (
            "$collisionmodel reference.smd { $automass $mass 12 }\n",
            false,
            Some(12.0),
        ),
    ] {
        let description = fixture.parse(commands);
        assert_eq!(description.physics.auto_mass, automatic);
        assert_eq!(description.physics.mass, mass);
    }
}

#[test]
fn worldspace_sequence_sets_post_and_unknown_animation_options_are_errors() {
    let fixture = Fixture::new();
    let description = fixture.parse("$sequence idle reference.smd worldspace\n");
    let output = crate::pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let sequence = i32::from_le_bytes(output.mdl[192..196].try_into().unwrap()) as usize;
    assert_eq!(
        i32::from_le_bytes(output.mdl[sequence + 12..sequence + 16].try_into().unwrap()) & 0x4010,
        0x4010
    );
    assert!(crate::qc::parse_qc_str("$animation idle reference.smd 12345\n", &fixture.0).is_err());
}

#[test]
fn cyclepose_registers_a_parameter_without_an_explicit_declaration() {
    let fixture = Fixture::new();
    let description = fixture.parse("$sequence idle reference.smd cyclepose clock\n");
    assert_eq!(description.model.pose_parameters.len(), 1);
    assert_eq!(description.model.pose_parameters[0].name, "clock");
    let output = crate::pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let sequence = i32::from_le_bytes(output.mdl[192..196].try_into().unwrap()) as usize;
    assert_ne!(
        i32::from_le_bytes(output.mdl[sequence + 12..sequence + 16].try_into().unwrap()) & 0x0080,
        0
    );
    assert_eq!(
        i32::from_le_bytes(
            output.mdl[sequence + 180..sequence + 184]
                .try_into()
                .unwrap()
        ),
        0
    );
}
