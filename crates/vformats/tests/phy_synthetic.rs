//! Synthetic-fixture tests for the `.phy` parser, built from the byte-level
//! builders in `phy_common`.

mod phy_common;

use phy_common::{
    compact_phy, compact_phy_with_text, convex_header_phy, expected_tetra_source_vertex,
    flat_triangle_pair_phy, ledgetree_phy, phy_with_truncated_second_section, skip_count,
    two_compact_solids_phy, unsupported_then_compact_phy,
};
use vformats::Limits;
use vformats::phy::{
    IVP_METERS_TO_SOURCE_INCHES, PhyError, SkipReason, ivp_to_source, parse, parse_lossy,
};

fn lossy(bytes: &[u8]) -> vformats::phy::PhyFile<'_> {
    parse_lossy(bytes, &Limits::default()).expect("container header should read")
}

#[test]
fn empty_input_is_a_container_error() {
    assert!(matches!(
        parse_lossy(&[], &Limits::default()),
        Err(PhyError::Empty)
    ));
}

#[test]
fn ivp_points_convert_to_source_inches() {
    assert_eq!(
        ivp_to_source([1.0, -2.0, 3.0]),
        [39.370_08, 118.11024, 78.740_16]
    );
}

#[test]
fn compact_ledge_parser_extracts_triangles_and_vertices() {
    let bytes = compact_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(phy.stats.parsed_ledges, 1);
    assert!(phy.stats.skip_reasons.is_empty());
    let ledge = &phy.solids[0].ledges[0];
    assert_eq!(ledge.vertices.len(), 4);
    assert_eq!(ledge.ivp_vertices.len(), 4);
    assert_eq!(
        ledge.triangles,
        vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]]
    );
    assert_eq!(ledge.vertices[2], expected_tetra_source_vertex(2));
    assert_eq!(ledge.ivp_vertices[2], [0.0, 0.0, 1.0]);
}

#[test]
fn compact_ledge_with_three_vertices_is_valid_geometry() {
    let bytes = flat_triangle_pair_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(phy.stats.parsed_ledges, 1);
    assert_eq!(skip_count(&phy, SkipReason::NonConvexLedge), 0);
    assert!(phy.stats.skip_reasons.is_empty());
    let ledge = &phy.solids[0].ledges[0];
    assert_eq!(ledge.vertices.len(), 3);
    assert_eq!(ledge.triangles, vec![[0, 1, 2], [1, 0, 2]]);
    assert_eq!(ledge.vertices[2], expected_tetra_source_vertex(2));
}

#[test]
fn parser_keeps_prior_solids_when_later_section_is_bad() {
    let bytes = phy_with_truncated_second_section();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.declared_solids, 2);
    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(phy.stats.parsed_ledges, 1);
    assert_eq!(skip_count(&phy, SkipReason::SectionOutOfRange), 1);
    assert_eq!(phy.solids.len(), 1);
}

#[test]
fn multi_solid_sections_advance_past_size_field() {
    let bytes = two_compact_solids_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.declared_solids, 2);
    assert_eq!(phy.stats.parsed_solids, 2);
    assert_eq!(phy.stats.parsed_ledges, 3);
    assert!(phy.stats.skip_reasons.is_empty());
    assert_eq!(phy.solids[0].ledges.len(), 1);
    assert_eq!(phy.solids[1].ledges.len(), 2);
}

#[test]
fn ledgetree_parser_keeps_only_terminal_ledges() {
    let bytes = ledgetree_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(phy.stats.parsed_ledges, 2);
    assert!(phy.stats.skip_reasons.is_empty());
    assert_eq!(phy.solids[0].ledges.len(), 2);
}

#[test]
fn convex_header_fallback_extracts_ledge() {
    let bytes = convex_header_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(phy.stats.parsed_ledges, 1);
    assert!(phy.stats.skip_reasons.is_empty());
    assert_eq!(
        phy.solids[0].ledges[0].vertices[1],
        expected_tetra_source_vertex(1)
    );
}

#[test]
fn unsupported_sections_degrade_without_stopping_later_solids() {
    let bytes = unsupported_then_compact_phy();
    let phy = lossy(&bytes);

    assert_eq!(phy.stats.declared_solids, 2);
    assert_eq!(phy.stats.parsed_solids, 1);
    assert_eq!(skip_count(&phy, SkipReason::UnsupportedSection), 1);
    assert_eq!(
        phy.text.as_ref().map(|text| text.bytes),
        Some(b"surfaceprop\0".as_slice())
    );
}

#[test]
fn text_section_is_borrowed_and_parses_as_keyvalues() {
    let text = br#""solid" { "surfaceprop" "metal" "mass" "35.0" }"#;
    let mut with_nuls = text.to_vec();
    with_nuls.extend_from_slice(&[0, 0]);

    let bytes = compact_phy_with_text(&with_nuls);
    let phy = lossy(&bytes);

    let parsed_text = phy.text.expect("text section");
    assert_eq!(parsed_text.bytes, with_nuls.as_slice());
    assert!(parsed_text.as_str().is_some());

    let kv = parsed_text
        .keyvalues(&Limits::default())
        .expect("keyvalues trailer");
    let solid = kv.get("solid").and_then(|v| v.as_block()).expect("solid");
    assert_eq!(solid.get_str("surfaceprop"), Some("metal"));
    assert_eq!(solid.get_str("MASS"), Some("35.0"));
}

#[test]
fn source_space_fixture_extents_match_conversion_constant() {
    let bytes = compact_phy();
    let phy = lossy(&bytes);
    let ledge = &phy.solids[0].ledges[0];
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &ledge.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    assert_eq!(
        [max[0] - min[0], max[1] - min[1], max[2] - min[2]],
        [IVP_METERS_TO_SOURCE_INCHES; 3]
    );
}

#[test]
fn header_exposes_mdl_checksum_and_counts() {
    let mut bytes = compact_phy();
    bytes[12..16].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());

    let phy = lossy(&bytes);
    assert_eq!(phy.header.solid_count, 1);
    assert_eq!(phy.header.mdl_checksum, 0xDEAD_BEEF);
}

#[test]
fn parse_accepts_clean_files_and_rejects_first_anomaly() {
    assert!(parse(&compact_phy(), &Limits::default()).is_ok());

    assert!(matches!(
        parse(&[], &Limits::default()),
        Err(PhyError::Empty)
    ));

    let error = parse(&phy_with_truncated_second_section(), &Limits::default())
        .expect_err("truncated section is strict error");
    let PhyError::Anomaly(skip) = error else {
        panic!("expected anomaly, got {error:?}");
    };
    assert_eq!(skip.reason, SkipReason::SectionOutOfRange);
    assert_eq!(skip.solid_index, Some(1));
    assert!(skip.byte_offset.is_some());
    assert!(!error.to_string().is_empty());
}

#[test]
fn stat_detail_is_capped_but_counts_stay_accurate() {
    let bytes = phy_with_truncated_second_section();
    let no_detail = Limits {
        max_stat_records: 0,
        ..Limits::default()
    };
    let phy = parse_lossy(&bytes, &no_detail).expect("container ok");
    assert!(phy.stats.skips.is_empty());
    assert_eq!(phy.stats.total_skips(), 1);
    assert_eq!(skip_count(&phy, SkipReason::SectionOutOfRange), 1);
}

#[test]
fn input_limit_is_enforced() {
    let tiny = Limits {
        max_input_bytes: 4,
        ..Limits::default()
    };
    assert!(matches!(
        parse_lossy(&compact_phy(), &tiny),
        Err(PhyError::InputTooLarge { .. })
    ));
}

#[test]
fn strict_parse_fails_even_with_stat_detail_capped_to_zero() {
    let bytes = phy_with_truncated_second_section();
    let no_detail = Limits {
        max_stat_records: 0,
        ..Limits::default()
    };
    let error = parse(&bytes, &no_detail).expect_err("anomaly must still fail strict parse");
    assert!(matches!(error, PhyError::Anomaly(_)));
}

#[test]
fn truncated_header_before_the_checksum_field_is_bad_header() {
    // 14 bytes: past declared_size/solid_count but short of the
    // checksum field at offset 12..16.
    let short = compact_phy()[..14].to_vec();
    assert_eq!(
        parse_lossy(&short, &Limits::default()),
        Err(PhyError::BadHeader { offset: 12 })
    );
    assert_eq!(
        parse(&short, &Limits::default()),
        Err(PhyError::BadHeader { offset: 12 })
    );
}
