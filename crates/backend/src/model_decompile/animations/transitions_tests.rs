use std::{fs, path::Path};

use mdlc::{pipeline, qc};

use super::*;

fn build(directory: &Path, name: &str) -> Vec<u8> {
    let desc = qc::parse_qc_file(&directory.join(name)).unwrap();
    let built = pipeline::build(&desc, directory, pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, directory).unwrap();
    fs::read(directory.join(&desc.model.name)).unwrap()
}

fn fixture(multistage: bool, skip: bool) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("rig.smd"),
        include_str!("../../../../vendor/mdlc/tests/fixtures/rig.smd"),
    )
    .unwrap();
    let qc = format!(
        "$modelname models/tests/transitions.mdl\n$body body rig.smd\n$skiptransition spare\n$sequence idle rig.smd node stand\n$sequence sit rig.smd rtransition stand sit\n$sequence lie rig.smd transition sit lie\n$sequence return rig.smd transition lie stand\n{}{}",
        if multistage { "$calctransitions\n" } else { "" },
        if skip {
            "$skiptransition stand lie\n"
        } else {
            ""
        }
    );
    fs::write(directory.path().join("source.qc"), qc).unwrap();
    directory
}

fn node_names(reader: &Reader<'_>) -> Vec<String> {
    reader
        .table(248, 256, 4)
        .unwrap()
        .into_iter()
        .map(|record| reader.string(0, record).unwrap().into())
        .collect()
}

#[test]
fn transition_nodes_routes_and_reversible_flags_roundtrip() {
    for (multistage, skip) in [(false, false), (true, false), (true, true)] {
        let directory = fixture(multistage, skip);
        let original = build(directory.path(), "source.qc");
        let reader = Reader(&original);
        let records = reader.table(188, 192, 212).unwrap();
        let names = node_names(&reader);
        assert_eq!(names, ["spare", "stand", "sit", "lie"]);
        let offset = reader.count(252).unwrap();
        let routes = reader.bytes(offset, 16).unwrap();
        assert_eq!(routes[6], 3);
        assert_eq!(routes[2 * 4 + 1], 2);
        assert_eq!(routes[2 * 4 + 3], 4);
        assert_eq!(routes[7], if multistage && !skip { 3 } else { 0 });
        assert_eq!(routes[3 * 4 + 1], if skip { 0 } else { 2 });
        assert_eq!(entry(&reader, records[1], 4).unwrap(), [2, 3, 1]);
        let destination = directory.path().join("exported");
        let report = crate::model_decompile::decompile_model(
            &directory.path().join("models/tests/transitions.mdl"),
            &destination,
        )
        .unwrap();
        let rebuilt = build(
            &destination,
            report.qc_path.file_name().unwrap().to_str().unwrap(),
        );
        let rebuilt = Reader(&rebuilt);
        assert_eq!(node_names(&rebuilt), names);
        let rebuilt_offset = rebuilt.count(252).unwrap();
        assert_eq!(rebuilt.bytes(rebuilt_offset, 16).unwrap(), routes);
        let rebuilt_records = rebuilt.table(188, 192, 212).unwrap();
        for (a, b) in records.into_iter().zip(rebuilt_records) {
            assert_eq!(
                entry(&reader, a, 4).unwrap(),
                entry(&rebuilt, b, 4).unwrap()
            );
        }
    }
}

#[test]
fn malformed_node_tables_and_custom_routes_are_rejected() {
    let directory = fixture(false, false);
    let original = build(directory.path(), "source.qc");
    let reader = Reader(&original);
    let records = reader.table(188, 192, 212).unwrap();
    let node_names = reader.count(256).unwrap();
    for (at, value) in [
        (248, 256),
        (252, i32::MAX),
        (256, i32::MAX),
        (node_names, i32::MAX),
        (records[0] + 112, 99),
        (records[0] + 120, 2),
    ] {
        let mut raw = original.clone();
        raw[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(
            prelude(&Reader(&raw), &records).is_err(),
            "accepted malformed field at {at}"
        );
    }
    let offset = reader.count(252).unwrap();
    let mut raw = original.clone();
    raw[offset + 7] = 4;
    assert!(prelude(&Reader(&raw), &records).is_err());
    let mut raw = original.clone();
    raw[offset + 6] = 0;
    assert!(
        prelude(&Reader(&raw), &records).is_err(),
        "accepted one-way suppression that standard skiptransition cannot represent"
    );
}
