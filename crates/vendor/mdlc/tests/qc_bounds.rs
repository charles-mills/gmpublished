use mdlc::{
    pipeline::{self, PipelineOptions},
    qc,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mdlc-qc-bounds-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("cube.smd"), include_str!("fixtures/cube.smd")).unwrap();
        Self(path)
    }

    fn desc(&self, extra: &str) -> mdlc::model::ModelDesc {
        qc::parse_qc_str(
            &format!(
                "$modelname tests/bounds.mdl\n$body body cube.smd\n$sequence idle cube.smd\n{extra}"
            ),
            &self.0,
        )
        .unwrap()
    }

    fn build(&self, extra: &str) -> Vec<u8> {
        pipeline::build(&self.desc(extra), &self.0, PipelineOptions::default())
            .unwrap()
            .mdl
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn vector(bytes: &[u8], offset: usize) -> [f32; 3] {
    std::array::from_fn(|axis| {
        f32::from_le_bytes(
            bytes[offset + axis * 4..offset + axis * 4 + 4]
                .try_into()
                .unwrap(),
        )
    })
}

#[test]
fn explicit_clipping_bounds_are_distinct_from_movement_bounds() {
    let project = Project::new();
    let bytes = project.build("$scale 3\n$bbox -1 -2 -3 4 5 6\n$cbox -10 -20 -30 40 50 60\n");
    assert_eq!(&bytes[..4], b"IDST");
    // studiohdr_t: hull_min/max at 104/116, view_bb_min/max at 128/140.
    assert_eq!(vector(&bytes, 104), [-1.0, -2.0, -3.0]);
    assert_eq!(vector(&bytes, 116), [4.0, 5.0, 6.0]);
    assert_eq!(vector(&bytes, 128), [-10.0, -20.0, -30.0]);
    assert_eq!(vector(&bytes, 140), [40.0, 50.0, 60.0]);
}

#[test]
fn absent_clipping_bounds_keep_zero_default_and_last_declaration_wins() {
    let project = Project::new();
    let bytes = project.build("$bbox -1 -2 -3 4 5 6\n");
    assert_eq!(vector(&bytes, 128), [0.0; 3]);
    assert_eq!(vector(&bytes, 140), [0.0; 3]);
    let bytes = project.build("$cbox -1 -2 -3 4 5 6\n$cbox 0 0 0 0 0 0\n");
    assert_eq!(vector(&bytes, 128), [0.0; 3]);
    assert_eq!(vector(&bytes, 140), [0.0; 3]);
}

#[test]
fn invalid_clipping_bounds_fail_in_qc_and_direct_descriptions() {
    let project = Project::new();
    for command in [
        "$cbox 3 0 0 1 1 1",
        "$cbox NaN 0 0 1 1 1",
        "$cbox 0 0 0 inf 1 1",
        "$cbox 0 0 0 1 1",
    ] {
        assert!(
            qc::parse_qc_str(command, &project.0).is_err(),
            "accepted {command}"
        );
    }
    let mut desc = project.desc("");
    desc.model.view_bb_min = Some([-1.0; 3]);
    assert!(desc.validate().is_err());
    desc.model.view_bb_max = Some([f32::NAN; 3]);
    assert!(desc.validate().is_err());
    desc.model.view_bb_max = Some([-2.0; 3]);
    assert!(desc.validate().is_err());
}
