use super::{Arc, MaterialResolver, Mutex, TextureAssetError};
use parking_lot::Condvar;
use std::collections::VecDeque;

#[derive(Debug)]
pub struct TextureWorkingBudget {
    capacity: usize,
    state: Mutex<WorkingState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct WorkingState {
    bytes: usize,
    peak: usize,
    next_ticket: u64,
    waiting: VecDeque<u64>,
}

impl TextureWorkingBudget {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(WorkingState::default()),
            changed: Condvar::new(),
        }
    }
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
    pub(crate) fn peak_bytes(&self) -> usize {
        self.state.lock().peak
    }
    #[cfg(test)]
    pub(crate) fn current_bytes(&self) -> usize {
        self.state.lock().bytes
    }
    #[cfg(test)]
    pub(super) fn queued_requests(&self) -> usize {
        self.state.lock().waiting.len()
    }
    pub(super) fn check_required(&self, bytes: usize) -> Result<(), TextureAssetError> {
        let capacity = self.capacity();
        if bytes > capacity {
            Err(TextureAssetError::WorkingLimit {
                required: bytes,
                capacity,
            })
        } else {
            Ok(())
        }
    }
    pub(super) fn request(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<TextureWorkingRequest, TextureAssetError> {
        self.check_required(bytes)?;
        Ok(TextureWorkingRequest {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

pub(super) struct TextureWorkingRequest {
    budget: Arc<TextureWorkingBudget>,
    bytes: usize,
}

impl TextureWorkingRequest {
    #[cfg(test)]
    pub(super) fn acquire(self) -> TextureWorkingLease {
        self.acquire_cancellable(&crate::preparation::PreparationCancellation::default())
            .expect("uncancelled working request")
    }

    pub(super) fn acquire_cancellable(
        self,
        cancellation: &crate::preparation::PreparationCancellation,
    ) -> Result<TextureWorkingLease, crate::preparation::Cancelled> {
        cancellation.check()?;
        let wake_budget = Arc::clone(&self.budget);
        let _registration = cancellation.register(move || {
            let state = wake_budget.state.lock();
            wake_budget.changed.notify_all();
            drop(state);
        });
        let budget = &self.budget;
        let mut state = budget.state.lock();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.waiting.push_back(ticket);
        while state.waiting.front() != Some(&ticket)
            || state
                .bytes
                .checked_add(self.bytes)
                .is_none_or(|total| total > budget.capacity)
        {
            if cancellation.is_cancelled() {
                state.waiting.retain(|queued| *queued != ticket);
                drop(state);
                budget.changed.notify_all();
                return Err(crate::preparation::Cancelled);
            }
            budget.changed.wait(&mut state);
        }
        if cancellation.is_cancelled() {
            state.waiting.pop_front();
            drop(state);
            budget.changed.notify_all();
            return Err(crate::preparation::Cancelled);
        }
        state.waiting.pop_front();
        state.bytes += self.bytes;
        state.peak = state.peak.max(state.bytes);
        drop(state);
        budget.changed.notify_all();
        Ok(TextureWorkingLease {
            budget: self.budget,
            bytes: self.bytes,
        })
    }
}

pub(super) struct TextureWorkingLease {
    budget: Arc<TextureWorkingBudget>,
    bytes: usize,
}

impl Drop for TextureWorkingLease {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock();
        state.bytes -= self.bytes;
        drop(state);
        self.budget.changed.notify_all();
    }
}

impl MaterialResolver {
    pub(crate) fn with_texture_working_budget(
        &self,
        budget: Arc<super::TextureWorkingBudget>,
    ) -> Self {
        self.variant(super::ResolverConfig {
            texture_working_budget: Some(budget),
            ..self.config.clone()
        })
    }
    pub(crate) fn with_texture_resource_limit(&self, bytes: usize) -> Self {
        self.variant(super::ResolverConfig {
            texture_resource_limit: Some(bytes),
            ..self.config.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::TextureLoadOutcome;
    use crate::test_support::fixture_vtf_bytes;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use vformats::vtf::VtfFormat;

    fn wait_queued(budget: &TextureWorkingBudget, count: usize) {
        let end = Instant::now() + Duration::from_secs(2);
        while budget.state.lock().waiting.len() < count {
            assert!(Instant::now() < end, "working request did not queue");
            thread::yield_now();
        }
    }

    #[test]
    fn weighted_fifo_waits_without_skipping_a_large_request() {
        let budget = Arc::new(TextureWorkingBudget::new(8));
        let held = budget.request(8).unwrap().acquire();
        let (event_tx, event_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let large_budget = Arc::clone(&budget);
        let large_tx = event_tx.clone();
        let large = thread::spawn(move || {
            let _lease = large_budget.request(8).unwrap().acquire();
            large_tx.send("large").unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        wait_queued(&budget, 1);
        let small_budget = Arc::clone(&budget);
        let small = thread::spawn(move || {
            let _lease = small_budget.request(2).unwrap().acquire();
            event_tx.send("small").unwrap();
        });
        wait_queued(&budget, 2);
        drop(held);
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "large"
        );
        assert!(event_rx.try_recv().is_err());
        assert_eq!(budget.current_bytes(), 8);
        release_tx.send(()).unwrap();
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "small"
        );
        large.join().unwrap();
        small.join().unwrap();
        assert_eq!(budget.current_bytes(), 0);
        assert_eq!(budget.peak_bytes(), 8);
        assert!(matches!(
            budget.request(9),
            Err(TextureAssetError::WorkingLimit { .. })
        ));
    }

    #[test]
    fn cancelled_fifo_head_releases_its_successor_without_waiting_for_capacity() {
        let budget = Arc::new(TextureWorkingBudget::new(8));
        let held = budget.request(6).unwrap().acquire();
        let cancellation = crate::preparation::PreparationCancellation::default();
        let large_budget = Arc::clone(&budget);
        let large_token = cancellation.clone();
        let (large_tx, large_rx) = mpsc::channel();
        let large = thread::spawn(move || {
            large_tx
                .send(
                    large_budget
                        .request(8)
                        .unwrap()
                        .acquire_cancellable(&large_token)
                        .is_err(),
                )
                .unwrap();
        });
        wait_queued(&budget, 1);
        let small_budget = Arc::clone(&budget);
        let (small_tx, small_rx) = mpsc::channel();
        let small = thread::spawn(move || {
            let _lease = small_budget.request(2).unwrap().acquire();
            small_tx.send(()).unwrap();
        });
        wait_queued(&budget, 2);
        cancellation.cancel();
        assert!(large_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        small_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        large.join().unwrap();
        small.join().unwrap();
        assert_eq!(budget.current_bytes(), 6);
        assert!(budget.state.lock().waiting.is_empty());
        drop(held);
        assert_eq!(budget.current_bytes(), 0);
    }

    #[test]
    fn lease_releases_on_unwind() {
        let budget = Arc::new(TextureWorkingBudget::new(8));
        let worker_budget = Arc::clone(&budget);
        assert!(
            thread::spawn(move || {
                let _lease = worker_budget.request(8).unwrap().acquire();
                panic!("decode failure");
            })
            .join()
            .is_err()
        );
        assert_eq!(budget.current_bytes(), 0);
        let lease = budget.request(8).unwrap().acquire();
        assert_eq!(budget.current_bytes(), 8);
        drop(lease);
        assert_eq!(budget.current_bytes(), 0);
    }

    #[test]
    fn declared_resource_limit_applies_consistently_to_base_frames_and_cube_selections() {
        let mut cube = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[63; 24]]);
        cube.splice(64..64, [0; 16]);
        cube[8..12].copy_from_slice(&5_u32.to_le_bytes());
        cube[12..16].copy_from_slice(&80_u32.to_le_bytes());
        cube[63..65].copy_from_slice(&1_u16.to_le_bytes());
        cube[20..24].copy_from_slice(&vformats::vtf::texture_flags::ENVMAP.to_le_bytes());
        cube[26..28].copy_from_slice(&u16::MAX.to_le_bytes());
        let resolver = MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/image.vmt",br#"LightmappedGeneric { "$basetexture" "test/image" "$bumpmap" "test/image" "$envmap" "test/cube" }"#.to_vec()),
            ("materials/test/image.vtf",fixture_vtf_bytes(2,2,VtfFormat::Rgba8888,&[&[7;16]])),
            ("materials/test/cube.vtf",cube),
        ],Vec::new());
        for (limit, expected) in [
            (15, TextureLoadOutcome::QualityRejected),
            (24, TextureLoadOutcome::Loaded),
        ] {
            let material = resolver
                .with_texture_resource_limit(limit)
                .resolve_primary(&[], "test/image")
                .into_material()
                .unwrap();
            assert_eq!(material.texture.as_ref().unwrap().load_outcome(), expected);
            let surface = material.shader.surface().unwrap();
            assert_eq!(surface.normal.as_ref().unwrap().load_outcome(), expected);
            let super::super::EnvironmentMapSource::Named(cube) =
                &surface.envmap.as_ref().unwrap().source
            else {
                panic!("named cube");
            };
            assert_eq!(cube.load_outcome(), expected);
        }
    }

    #[test]
    fn independent_converted_resources_share_the_declared_aggregate_gate() {
        const SIDE: u16 = 512;
        let mut files = Vec::new();
        for index in 0_u8..4 {
            files.push((
                format!("materials/test/image{index}.vmt"),
                format!(r#"LightmappedGeneric {{ "$basetexture" "test/image{index}" }}"#)
                    .into_bytes(),
            ));
            files.push((
                format!("materials/test/image{index}.vtf"),
                fixture_vtf_bytes(
                    SIDE,
                    SIDE,
                    VtfFormat::Rgb888,
                    &[&vec![index; usize::from(SIDE) * usize::from(SIDE) * 3]],
                ),
            ));
        }
        let resolver = MaterialResolver::prop_lighting_fixture(
            files
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.clone()))
                .collect(),
            Vec::new(),
        );
        let working = Arc::new(TextureWorkingBudget::new(
            usize::from(SIDE) * usize::from(SIDE) * 7,
        ));
        let resolver = Arc::new(resolver.with_texture_working_budget(Arc::clone(&working)));
        let workers = (0_u8..4)
            .map(|index| {
                let resolver = Arc::clone(&resolver);
                thread::spawn(move || {
                    let material = resolver
                        .resolve_primary(&[], &format!("test/image{index}"))
                        .into_material()
                        .unwrap();
                    assert_eq!(
                        material.texture.as_ref().unwrap().load_outcome(),
                        TextureLoadOutcome::Loaded
                    );
                    let texture = material.texture.as_ref().unwrap();
                    let (source, request) = texture.on_demand().unwrap();
                    let page = source
                        .load_page(
                            request,
                            &Default::default(),
                            resolver.config.texture_working_budget.as_ref().unwrap(),
                        )
                        .unwrap();
                    assert_eq!(
                        &page.resident().layers()[0].mips[0].bytes.as_slice()[..4],
                        &[index, index, index, 255]
                    );
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(working.current_bytes(), 0);
        assert_eq!(working.peak_bytes(), working.capacity());
    }
}
