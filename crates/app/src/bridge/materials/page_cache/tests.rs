use super::super::reloadable::TextureSourceReader;
use super::super::texture_asset::TextureOrigin;
use super::super::{ContentPath, ContentSourceTier, TextureStorage};
use super::*;
use std::{
    ops::Range,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
    waiting: AtomicUsize,
    wait_for_release: bool,
}
impl Gate {
    fn release(&self) {
        *self.released.lock() = true;
        self.changed.notify_all();
    }
    fn wait(
        self: &Arc<Self>,
        cancellation: &PreparationCancellation,
    ) -> Result<(), TextureAssetError> {
        let wake = Arc::clone(self);
        let _registration = cancellation.register(move || {
            let _guard = wake.released.lock();
            wake.changed.notify_all();
        });
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let mut released = self.released.lock();
        while !*released {
            if !self.wait_for_release {
                cancellation.check()?;
            }
            self.changed.wait(&mut released);
        }
        cancellation.check()?;
        Ok(())
    }
}

#[derive(Debug)]
struct Reader {
    bytes: Vec<u8>,
    reads: Arc<AtomicUsize>,
    gate: Option<Arc<Gate>>,
    fail: bool,
}
impl TextureSourceReader for Reader {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancellation: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancellation.check()?;
        if range.start >= 80 {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.wait(cancellation)?;
            }
            if self.fail {
                return Err(TextureAssetError::SourceChanged);
            }
        }
        Ok(self.bytes[range.start as usize..range.end as usize].to_vec())
    }
}
fn source(gate: Option<Arc<Gate>>, fail: bool) -> (Arc<ReloadableTextureSource>, Arc<AtomicUsize>) {
    let mut bytes =
        crate::test_support::fixture_vtf_bytes(4, 4, vformats::vtf::VtfFormat::Rgba8888, &[&[]]);
    bytes.resize(80, 0);
    bytes[8..12].copy_from_slice(&5_u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
    bytes[24..26].copy_from_slice(&3_u16.to_le_bytes());
    bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
    for frame in 0..3 {
        bytes.extend(std::iter::repeat_n(frame as u8 + 10, 64));
    }
    let reads = Arc::new(AtomicUsize::new(0));
    let source = ReloadableTextureSource::inspect(
        TextureOrigin {
            path: ContentPath::new("materials/test/page.vtf").unwrap(),
            tier: ContentSourceTier::Addon,
        },
        Arc::new(Reader {
            bytes,
            reads: Arc::clone(&reads),
            gate,
            fail,
        }),
        vformats::Limits::default(),
        &PreparationCancellation::default(),
        &Arc::new(TextureWorkingBudget::new(4096)),
    )
    .unwrap();
    (source, reads)
}
fn request(source: &ReloadableTextureSource, frame: u32) -> TextureFrameRequest {
    source
        .frame_request(frame, 0..1, 0, 1, TextureStorage::Rgba8)
        .unwrap()
}
fn cache(bytes: usize, queued: usize) -> Arc<TexturePageCache> {
    TexturePageCache::new(
        PageCachePolicy {
            resident_bytes: bytes,
            max_entries: 8,
            queued_jobs: queued,
            workers: 1,
        },
        Arc::new(TextureWorkingBudget::new(4096)),
    )
    .unwrap()
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "page worker did not progress");
        thread::sleep(Duration::from_millis(1));
    }
}
fn ready(ticket: &PageTicket) -> Arc<CachedTexturePage> {
    wait(|| !matches!(ticket.status(), PageStatus::Pending));
    match ticket.status() {
        PageStatus::Ready(page) => page,
        other => panic!("expected ready page: {other:?}"),
    }
}

#[test]
fn concurrent_requests_share_one_decode_and_working_is_not_retained() {
    let gate = Arc::new(Gate::default());
    let (source, reads) = source(Some(Arc::clone(&gate)), false);
    let cache = cache(128, 4);
    let a = cache.request(Arc::clone(&source), request(&source, 0));
    wait(|| gate.waiting.load(Ordering::SeqCst) == 1);
    let b = cache.request(Arc::clone(&source), request(&source, 0));
    assert_eq!(cache.stats().resident_reserved_bytes, 64);
    assert_eq!(cache.stats().running, 1);
    gate.release();
    let a = ready(&a);
    let b = ready(&b);
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(cache.inner.working.current_bytes(), 0);
    assert_eq!(a.resident().layers()[0].mips[0].bytes.as_slice(), &[10; 64]);
}

#[test]
fn external_page_keeps_resident_charge_after_lru_eviction() {
    let (source, reads) = source(None, false);
    let cache = cache(64, 4);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    let page = ready(&first);
    drop(first);
    let blocked = cache.request(Arc::clone(&source), request(&source, 1));
    assert!(matches!(
        blocked.status(),
        PageStatus::PendingRetry(PageRetry::ResidentBytes { .. })
    ));
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().resident_reserved_bytes, 64);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let mut changed = cache.subscribe().receiver;
    changed.borrow_and_update();
    drop(page);
    assert!(changed.has_changed().unwrap());
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
    let second = cache.request(Arc::clone(&source), request(&source, 1));
    assert_eq!(
        ready(&second).resident().layers()[0].mips[0]
            .bytes
            .as_slice(),
        &[11; 64]
    );
}

#[test]
fn queue_and_resident_pressure_do_not_start_rejected_decoders() {
    let gate = Arc::new(Gate::default());
    let (source, reads) = source(Some(Arc::clone(&gate)), false);
    let zero = cache(0, 1);
    let denied = zero.request(Arc::clone(&source), request(&source, 0));
    assert!(matches!(
        denied.status(),
        PageStatus::PendingRetry(PageRetry::ResidentBytes { .. })
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let cache = cache(256, 1);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    wait(|| gate.waiting.load(Ordering::SeqCst) == 1);
    let queued = cache.request(Arc::clone(&source), request(&source, 1));
    let denied = cache.request(Arc::clone(&source), request(&source, 2));
    assert!(matches!(
        denied.status(),
        PageStatus::PendingRetry(PageRetry::Queue { capacity: 1 })
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    drop(queued);
    assert_eq!(cache.stats().queued, 0);
    drop(first);
    wait(|| cache.stats().running == 0);
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
    gate.release();
    let retry = cache.request(Arc::clone(&source), request(&source, 2));
    ready(&retry);
}

#[test]
fn last_interest_cancels_but_another_widget_can_keep_a_shared_request_alive() {
    let gate = Arc::new(Gate::default());
    let (source, reads) = source(Some(Arc::clone(&gate)), false);
    let cache = cache(128, 4);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    let second = cache.request(Arc::clone(&source), request(&source, 0));
    wait(|| gate.waiting.load(Ordering::SeqCst) == 1);
    drop(first);
    assert!(!second.interest.flight.cancellation.is_cancelled());
    drop(second);
    wait(|| cache.stats().running == 0);
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
    gate.release();
    let retry = cache.request(Arc::clone(&source), request(&source, 0));
    ready(&retry);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[test]
fn orphan_trim_releases_cached_pages_without_retaining_source_provider() {
    let (source, _) = source(None, false);
    let provider = Arc::downgrade(&source);
    let cache = cache(128, 4);
    let ticket = cache.request(Arc::clone(&source), request(&source, 0));
    let external = ready(&ticket);
    drop(source);
    assert!(provider.upgrade().is_some());
    drop(ticket);
    assert!(provider.upgrade().is_none());
    cache.trim_orphaned();
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().resident_reserved_bytes, 64);
    drop(external);
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
}

#[test]
fn typed_source_failure_is_shared_without_fallback_or_repeat_io() {
    let (source, reads) = source(None, true);
    let cache = cache(128, 4);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    wait(|| matches!(first.status(), PageStatus::Failed(_)));
    let second = cache.request(Arc::clone(&source), request(&source, 0));
    assert!(
        matches!(second.status(), PageStatus::Failed(error) if *error == TextureAssetError::SourceChanged)
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
}

#[test]
fn close_cancels_work_and_preserves_only_external_page_leases() {
    let gate = Arc::new(Gate::default());
    gate.release();
    let (source, _) = source(Some(Arc::clone(&gate)), false);
    let cache = cache(128, 4);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    let external = ready(&first);
    *gate.released.lock() = false;
    let second = cache.request(Arc::clone(&source), request(&source, 1));
    wait(|| gate.waiting.load(Ordering::SeqCst) == 2);
    cache.close();
    wait(|| cache.stats().running == 0);
    assert!(matches!(
        second.status(),
        PageStatus::PendingRetry(PageRetry::Closed)
    ));
    assert_eq!(cache.stats().resident_reserved_bytes, 64);
    assert_eq!(
        external.resident().layers()[0].mips[0].bytes.as_slice(),
        &[10; 64]
    );
    drop(external);
    assert_eq!(cache.stats().resident_reserved_bytes, 0);
}

#[test]
fn capability_decode_policy_is_checked_before_cold_and_cached_physical_pages() {
    for warm in [false, true] {
        let mut bytes =
            crate::test_support::fixture_vtf_bytes(2, 2, vformats::vtf::VtfFormat::Dxt1, &[&[]]);
        bytes.resize(80, 0);
        bytes[8..12].copy_from_slice(&5_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        let reads = Arc::new(AtomicUsize::new(0));
        let source = ReloadableTextureSource::inspect(
            TextureOrigin {
                path: ContentPath::new("materials/test/tiny_bc.vtf").unwrap(),
                tier: ContentSourceTier::Addon,
            },
            Arc::new(Reader {
                bytes,
                reads: Arc::clone(&reads),
                gate: None,
                fail: false,
            }),
            vformats::Limits::default(),
            &PreparationCancellation::default(),
            &Arc::new(TextureWorkingBudget::new(4096)),
        )
        .unwrap();
        let mut bc = source
            .frame_request(
                0,
                0..1,
                0,
                1,
                TextureStorage::Bc(vformats::vtf::BcFormat::Bc1),
            )
            .unwrap();
        bc.limits = super::super::TexturePageLimits {
            resident_bytes: 8,
            decode_working_bytes: 8,
        };
        assert_eq!(source.validate_page(&bc).unwrap().resident_bytes, 8);
        let mut normalized = bc.clone();
        normalized.storage = TextureStorage::Rgba8;
        assert_eq!(
            source.page_cost(&normalized).unwrap(),
            super::super::TexturePageCost {
                resident_bytes: 16,
                decode_working_bytes: 24,
            }
        );
        let mut permissive = normalized.clone();
        permissive.limits = super::super::TexturePageLimits {
            resident_bytes: 16,
            decode_working_bytes: 24,
        };
        assert_eq!(normalized.identity(), permissive.identity());
        assert_ne!(normalized, permissive);
        let cache = cache(64, 4);
        let cached = warm.then(|| ready(&cache.request(Arc::clone(&source), permissive.clone())));
        let denied = cache.request(Arc::clone(&source), normalized.clone());
        assert!(matches!(denied.status(), PageStatus::Failed(error)
            if matches!(*error, TextureAssetError::Vtf(vformats::vtf::VtfError::DecodedTooLarge { size: 16, max: 8 }))));
        let mut working_limited = permissive.clone();
        working_limited.limits.decode_working_bytes = 8;
        let denied = cache.request(Arc::clone(&source), working_limited);
        assert!(matches!(denied.status(), PageStatus::Failed(error)
            if matches!(*error, TextureAssetError::WorkingLimit { required: 24, capacity: 8 })));
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(warm));
        assert!(matches!(
            source.load_page(
                &normalized,
                &PreparationCancellation::default(),
                &Arc::new(TextureWorkingBudget::new(4096)),
            ),
            Err(TextureAssetError::Vtf(
                vformats::vtf::VtfError::DecodedTooLarge { size: 16, max: 8 }
            ))
        ));
        let recovered = ready(&cache.request(Arc::clone(&source), permissive.clone()));
        assert_eq!(recovered.resident().byte_len(), 16);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        if let Some(cached) = cached {
            assert!(Arc::ptr_eq(&cached, &recovered));
        }
        let mut other_policy = permissive;
        other_policy.limits.resident_bytes += 1;
        assert!(Arc::ptr_eq(
            &recovered,
            &ready(&cache.request(source, other_policy))
        ));
        assert_eq!(cache.stats().entries, 1);
    }
}

#[test]
fn last_worker_releases_orphaned_ready_pages_without_another_cache_visit() {
    struct ReleaseOnDrop(Arc<Gate>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let gate = Arc::new(Gate {
        wait_for_release: true,
        ..Gate::default()
    });
    let _release = ReleaseOnDrop(Arc::clone(&gate));
    gate.release();
    let (source, _) = source(Some(Arc::clone(&gate)), false);
    let provider = Arc::downgrade(&source);
    let cache = cache(128, 4);
    let first = cache.request(Arc::clone(&source), request(&source, 0));
    ready(&first);
    *gate.released.lock() = false;
    let second = cache.request(Arc::clone(&source), request(&source, 1));
    wait(|| gate.waiting.load(Ordering::SeqCst) == 2);
    drop(first);
    drop(second);
    drop(source);
    assert_eq!(
        provider.strong_count(),
        1,
        "only the blocked worker owns the source"
    );
    cache.trim_orphaned();
    assert_eq!(
        cache.stats().entries,
        1,
        "worker ownership temporarily keeps the warm page"
    );
    assert_eq!(cache.stats().resident_reserved_bytes, 128);
    assert_eq!(cache.stats().running, 1);
    gate.release();
    wait(|| {
        let stats = cache.stats();
        stats.running == 0 && stats.entries == 0 && stats.resident_reserved_bytes == 0
    });
    assert!(provider.upgrade().is_none());
    assert_eq!(cache.inner.working.current_bytes(), 0);
}
