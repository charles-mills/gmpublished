use super::*;

fn save(root: &Path, bytes: &[u8], inputs: &BTreeSet<PathBuf>) -> Result<(), ModelCompileError> {
    let stage = tempfile::tempdir_in(root).unwrap();
    let files = stage.path().join("files");
    fs::create_dir_all(files.join("one_sources")).unwrap();
    fs::write(files.join("one_sources/mesh.smd"), bytes).unwrap();
    publish(
        stage,
        &files,
        root,
        vec!["one_sources/mesh.smd".into()],
        OsStr::new("one.qc"),
        inputs,
    )
}

#[test]
fn malformed_and_unsafe_registries_never_claim_existing_files() {
    let root = tempfile::tempdir().unwrap();
    for bytes in [
        b"personal notes".to_vec(),
        serde_json::to_vec(&serde_json::json!({
            "format": FORMAT, "version": 1,
            "projects": { "one.qc": { "../outside": { "size": 1, "crc32": 1, "managed": true } } }
        }))
        .unwrap(),
        serde_json::to_vec(&serde_json::json!({
            "format": FORMAT, "version": 999, "projects": {}
        }))
        .unwrap(),
    ] {
        fs::write(root.path().join(REGISTRY), &bytes).unwrap();
        assert!(save(root.path(), b"new", &BTreeSet::new()).is_err());
        assert_eq!(fs::read(root.path().join(REGISTRY)).unwrap(), bytes);
        assert!(!root.path().join("one_sources").exists());
    }
}

#[test]
fn edited_owned_files_and_unowned_namespaces_remain_untouched() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("one_sources")).unwrap();
    let path = root.path().join("one_sources/mesh.smd");
    fs::write(&path, b"personal source").unwrap();
    assert!(save(root.path(), b"new", &BTreeSet::new()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"personal source");
    assert!(!root.path().join(REGISTRY).exists());
    fs::remove_file(&path).unwrap();
    save(root.path(), b"first", &BTreeSet::new()).unwrap();
    fs::write(&path, b"user edit").unwrap();
    let registry = fs::read(root.path().join(REGISTRY)).unwrap();
    assert!(save(root.path(), b"second", &BTreeSet::new()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"user edit");
    assert_eq!(fs::read(root.path().join(REGISTRY)).unwrap(), registry);
}

#[test]
fn hardlinks_to_imported_inputs_override_prior_ownership() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), b"first", &BTreeSet::new()).unwrap();
    let target = root.path().join("one_sources/mesh.smd");
    let input = root.path().join("imported.smd");
    fs::hard_link(&target, &input).unwrap();
    let inputs = BTreeSet::from([super::super::canonical(&input).unwrap()]);
    let error = save(root.path(), b"replacement", &inputs).unwrap_err();
    assert!(
        error.to_string().contains("overwrite an imported source"),
        "{error}"
    );
    assert_eq!(fs::read(target).unwrap(), b"first");
    assert_eq!(fs::read(input).unwrap(), b"first");
}

#[cfg(unix)]
#[test]
fn ownership_registry_and_output_ancestor_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let notes = outside.path().join("notes.json");
    fs::write(
        &notes,
        br#"{"format":"gmpublished-qc-projects","version":1,"projects":{}}"#,
    )
    .unwrap();
    let original = fs::read(&notes).unwrap();
    symlink(&notes, root.path().join(REGISTRY)).unwrap();
    assert!(save(root.path(), b"new", &BTreeSet::new()).is_err());
    assert_eq!(fs::read(&notes).unwrap(), original);
    fs::remove_file(root.path().join(REGISTRY)).unwrap();
    symlink(outside.path(), root.path().join("one_sources")).unwrap();
    assert!(save(root.path(), b"new", &BTreeSet::new()).is_err());
    assert!(!outside.path().join("mesh.smd").exists());
}

#[test]
fn a_registry_hardlinked_to_an_imported_dependency_is_never_rewritten() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), b"first", &BTreeSet::new()).unwrap();
    let input = root.path().join("input.bin");
    fs::hard_link(root.path().join(REGISTRY), &input).unwrap();
    let original = fs::read(&input).unwrap();
    let inputs = BTreeSet::from([super::super::canonical(&input).unwrap()]);
    assert!(save(root.path(), b"second", &inputs).is_err());
    assert_eq!(fs::read(input).unwrap(), original);
    assert_eq!(
        fs::read(root.path().join("one_sources/mesh.smd")).unwrap(),
        b"first"
    );
}

#[test]
fn shared_asset_case_aliases_block_changes() {
    let root = tempfile::tempdir().unwrap();
    save(root.path(), b"first", &BTreeSet::new()).unwrap();
    let mut registry = read_registry(root.path()).unwrap();
    let asset = registry.projects["one.qc"]["one_sources/mesh.smd"].clone();
    registry.projects.insert(
        "other.qc".into(),
        BTreeMap::from([("ONE_sources/MESH.SMD".into(), asset)]),
    );
    fs::write(
        root.path().join(REGISTRY),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let error = save(root.path(), b"second", &BTreeSet::new()).unwrap_err();
    assert!(
        error.to_string().contains("used by another project"),
        "{error}"
    );
    assert_eq!(
        fs::read(root.path().join("one_sources/mesh.smd")).unwrap(),
        b"first"
    );
}
