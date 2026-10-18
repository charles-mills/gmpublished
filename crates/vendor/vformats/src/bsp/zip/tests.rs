use super::*;

#[test]
fn shared_archive_caches_success_and_error_once_under_concurrent_readers() {
    for bytes in [b"".as_slice(), b"not a ZIP".as_slice()] {
        let archive = ZipArchive::new(Arc::<[u8]>::from(bytes));
        let equal_uninitialized = ZipArchive::new(Arc::<[u8]>::from(bytes));
        let expected = if bytes.is_empty() {
            Ok(0)
        } else {
            Err(ZipError::MissingDirectory)
        };
        std::thread::scope(|scope| {
            let jobs = (0..16)
                .map(|_| {
                    let archive = archive.clone();
                    scope.spawn(move || archive.reader().map(|reader| reader.entries().len()))
                })
                .collect::<Vec<_>>();
            for job in jobs {
                assert_eq!(job.join().unwrap(), expected);
            }
        });
        assert_eq!(archive.inner.preflight_runs.load(Ordering::Relaxed), 1);
        assert_eq!(
            archive.inner.parses.load(Ordering::Relaxed),
            usize::from(bytes.is_empty())
        );
        assert!(equal_uninitialized.inner.directory.get().is_none());
        assert_eq!(archive, equal_uninitialized);
        assert!(!format!("{archive:?}").contains("not a ZIP"));
    }
}

fn directory(names: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for name in names {
        let mut record = [0; CENTRAL_BYTES];
        record[..4].copy_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        record[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&record);
        bytes.extend_from_slice(name);
    }
    let size = bytes.len();
    let mut end = [0; EOCD_BYTES];
    end[..4].copy_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    end[8..10].copy_from_slice(&(names.len() as u16).to_le_bytes());
    end[10..12].copy_from_slice(&(names.len() as u16).to_le_bytes());
    end[12..16].copy_from_slice(&(size as u32).to_le_bytes());
    bytes.extend_from_slice(&end);
    bytes
}

#[test]
fn profiles_are_checked_before_materialization_and_on_warm_cache() {
    let archive = ZipArchive::new(directory(&[b"materials/a.vmt", &[0xff, 0xff]]));
    let cost = archive.index_cost().unwrap();
    assert_eq!(cost.string_bytes, 15 + 6);
    let strict = ArchiveIndexLimits {
        retained_bytes: cost.retained_bytes().unwrap() - 1,
        ..ArchiveIndexLimits::default()
    };
    assert!(matches!(
        archive.reader_with_index_limits(strict),
        Err(ZipError::IndexLimit(_))
    ));
    assert!(archive.inner.directory.get().is_none());
    let reader = archive.reader().unwrap();
    let clone = archive.clone();
    assert_eq!(
        reader.entries().as_ptr(),
        clone.reader().unwrap().entries().as_ptr()
    );
    assert!(matches!(
        clone.reader_with_index_limits(strict),
        Err(ZipError::IndexLimit(_))
    ));
    assert_eq!(archive.inner.parses.load(Ordering::Relaxed), 1);
    let weak = Arc::downgrade(&archive.inner);
    drop(reader);
    drop(archive);
    assert!(weak.upgrade().is_some());
    drop(clone);
    assert!(weak.upgrade().is_none());
}

#[test]
fn declared_directory_bounds_are_checked_before_capacity_reservation() {
    let mut bytes = directory(&[b"x"]);
    let end = bytes.len() - EOCD_BYTES;
    bytes[end + 8..end + 12].fill(0xff);
    assert_eq!(ZipArchive::new(bytes).index_cost(), Err(ZipError::Corrupt));
}
