use gmpublished_backend::CpuExecutor;
use gmpublished_domain::scene::map::{LIGHTMAP_PAGE_SIZE, LightmapPage};
use rayon::prelude::*;

pub(super) const MAX_JOBS: usize = 4;
pub(super) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn page_bytes(page: &LightmapPage) -> usize {
    let width = page.width as usize;
    let height = page.height as usize;
    // Preserve invalid requests at their ordinals so render_page reports errors in order.
    if width == 0 || height == 0 || width > LIGHTMAP_PAGE_SIZE || height > LIGHTMAP_PAGE_SIZE {
        0
    } else {
        width * height * 4
    }
}

pub(super) struct Wave {
    pub end: usize,
    pub indices: Vec<usize>,
    pub output_bytes: usize,
}

impl Wave {
    pub fn plan(
        start: usize,
        length: usize,
        maximum_jobs: usize,
        mut cost: impl FnMut(usize) -> Option<usize>,
    ) -> Self {
        assert!((1..=MAX_JOBS).contains(&maximum_jobs));
        let mut wave = Self {
            end: start,
            indices: Vec::new(),
            output_bytes: 0,
        };
        for index in start..length {
            if let Some(bytes) = cost(index) {
                if wave.indices.len() == maximum_jobs
                    || (!wave.indices.is_empty()
                        && wave
                            .output_bytes
                            .checked_add(bytes)
                            .is_none_or(|total| total > MAX_OUTPUT_BYTES))
                {
                    break;
                }
                wave.indices.push(index);
                wave.output_bytes = wave.output_bytes.saturating_add(bytes);
                if bytes > MAX_OUTPUT_BYTES {
                    wave.end = index + 1;
                    break;
                }
            }
            wave.end = index + 1;
        }
        wave
    }
}

pub(super) fn cook<T: Send, E: Send>(
    executor: &CpuExecutor,
    indices: &[usize],
    render: impl Fn(&usize) -> Result<T, E> + Sync,
) -> Vec<Result<T, E>> {
    assert!(indices.len() <= MAX_JOBS);
    if indices.len() <= 1 {
        return indices.iter().map(render).collect();
    }
    let render = &render;
    executor.install(|| indices.par_iter().map(render).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::{LightmapPageId, LightmapSet, LightmapSource};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn waves_cover_exact_missing_ordinals_with_independent_job_and_byte_bounds() {
        // None is an already cached page; zero is an error with no output.
        let options = [None, Some(0), Some(4 * 1024 * 1024), Some(9 * 1024 * 1024)];
        for encoded in 0..4_usize.pow(6) {
            let costs: Vec<_> = (0..6)
                .map(|index| options[(encoded / 4_usize.pow(index)) % 4])
                .collect();
            for maximum_jobs in 1..=MAX_JOBS {
                let mut actual = Vec::new();
                let mut start = 0;
                while start < costs.len() {
                    let wave = Wave::plan(start, costs.len(), maximum_jobs, |index| costs[index]);
                    assert!(wave.end > start);
                    assert!(wave.indices.len() <= maximum_jobs);
                    let sum: usize = wave
                        .indices
                        .iter()
                        .map(|&index| costs[index].unwrap())
                        .sum();
                    assert_eq!(wave.output_bytes, sum);
                    assert!(sum <= MAX_OUTPUT_BYTES);
                    actual.extend(wave.indices);
                    start = wave.end;
                }
                let expected: Vec<_> = costs
                    .iter()
                    .enumerate()
                    .filter_map(|(index, cost)| cost.map(|_| index))
                    .collect();
                assert_eq!(actual, expected);
            }
        }
        let oversized = Wave::plan(0, 2, MAX_JOBS, |_| Some(MAX_OUTPUT_BYTES + 1));
        assert_eq!(oversized.indices, [0]);
        assert_eq!(oversized.end, 1);
    }

    #[test]
    fn ordered_results_preserve_earlier_upload_error_over_later_cook_error_and_drop_suffix() {
        #[derive(Debug, PartialEq, Eq)]
        enum Failure {
            Cook(usize),
            Upload(usize),
        }
        struct Output(usize, Arc<AtomicUsize>);
        impl Drop for Output {
            fn drop(&mut self) {
                self.1.fetch_add(1, Ordering::SeqCst);
            }
        }
        let executor = CpuExecutor::build(4).unwrap();
        for upload_error in [None, Some(1), Some(0)] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let results = cook(&executor, &[0, 1, 2, 3], |&index| {
                if index == 2 {
                    Err(Failure::Cook(index))
                } else {
                    Ok(Output(index, Arc::clone(&dropped)))
                }
            });
            let mut committed = Vec::new();
            let consume = || -> Result<(), Failure> {
                for result in results {
                    let output = result?;
                    if upload_error == Some(output.0) {
                        return Err(Failure::Upload(output.0));
                    }
                    committed.push(output.0);
                }
                Ok(())
            };
            let actual = consume();
            let expected = upload_error.map_or(Failure::Cook(2), Failure::Upload);
            assert_eq!(actual, Err(expected));
            assert_eq!(
                committed,
                (0..upload_error.unwrap_or(2)).collect::<Vec<_>>()
            );
            assert_eq!(dropped.load(Ordering::SeqCst), 3);
        }
    }

    #[test]
    fn cooked_outputs_are_released_before_next_wave_and_use_the_owned_pool() {
        struct Charged(usize, Arc<AtomicUsize>);
        impl Drop for Charged {
            fn drop(&mut self) {
                self.1.fetch_sub(self.0, Ordering::SeqCst);
            }
        }
        let executor = CpuExecutor::build(4).unwrap();
        let live = Arc::new(AtomicUsize::new(0));
        let peak = AtomicUsize::new(0);
        let page_bytes = 4 * 1024 * 1024;
        let mut start = 0;
        while start < 9 {
            assert_eq!(live.load(Ordering::SeqCst), 0);
            let wave = Wave::plan(start, 9, MAX_JOBS, |_| Some(page_bytes));
            let results = cook(&executor, &wave.indices, |_| -> Result<Charged, ()> {
                let thread = std::thread::current();
                if wave.indices.len() > 1 {
                    assert!(thread.name().unwrap().starts_with("gmpublished-cpu-"));
                }
                let used = live.fetch_add(page_bytes, Ordering::SeqCst) + page_bytes;
                peak.fetch_max(used, Ordering::SeqCst);
                Ok(Charged(page_bytes, Arc::clone(&live)))
            });
            assert!(live.load(Ordering::SeqCst) <= MAX_OUTPUT_BYTES);
            for output in results {
                drop(output.unwrap());
            }
            start = wave.end;
        }
        assert_eq!(peak.load(Ordering::SeqCst), MAX_OUTPUT_BYTES);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn immutable_lightmap_cooks_match_scalar_bytes_at_all_worker_counts_and_frozen_styles() {
        let pages: Vec<_> = (0..11)
            .map(|index| vec![(0, [32 + index, 64, 96, 0]), (32, [16, 8 + index, 4, 255])])
            .collect();
        for source in [LightmapSource::Ldr, LightmapSource::Hdr] {
            let set = LightmapSet::style_pages_for_tests(source, &pages, &[(32, "amz")]);
            for time in [0.0, 0.1, 0.2, 0.3] {
                let weights = set.style_values(time);
                let expected: Vec<_> = (0..11)
                    .map(|index| set.render_page(LightmapPageId(index), &weights))
                    .collect();
                for workers in [1, 2, 4] {
                    let executor = CpuExecutor::build(workers).unwrap();
                    let mut actual = Vec::new();
                    let mut start = 0;
                    while start < 11 {
                        let wave = Wave::plan(start, 11, workers, |_| Some(64));
                        actual.extend(cook(&executor, &wave.indices, |&index| {
                            set.render_page(LightmapPageId(index as u32), &weights)
                        }));
                        start = wave.end;
                    }
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn invalid_metadata_has_no_output_reservation_and_keeps_its_ordinal() {
        let set =
            LightmapSet::style_pages_for_tests(LightmapSource::Ldr, &[vec![(0, [255; 4])]], &[]);
        let valid = set.page(LightmapPageId(0)).unwrap();
        assert_eq!(page_bytes(valid), 64);
        for dimensions in [[0, 4], [4, 0], [1025, 4], [4, 1025], [u32::MAX; 2]] {
            let mut invalid = valid.clone();
            invalid.width = dimensions[0];
            invalid.height = dimensions[1];
            let costs = [Some(page_bytes(valid)), Some(page_bytes(&invalid)), None];
            let wave = Wave::plan(0, costs.len(), MAX_JOBS, |index| costs[index]);
            assert_eq!(wave.indices, [0, 1]);
            assert_eq!(wave.output_bytes, 64);
            assert_eq!(wave.end, 3);
        }
    }

    #[test]
    fn missing_pages_keep_original_error_ordinal_after_successful_cooks() {
        let executor = CpuExecutor::build(4).unwrap();
        let set =
            LightmapSet::style_pages_for_tests(LightmapSource::Ldr, &[vec![(0, [255; 4])]], &[]);
        let ids = [0, 9, 0, 8];
        let results = cook(&executor, &[0, 1, 2, 3], |&index| {
            set.render_page(LightmapPageId(ids[index]), &[1.0; 256])
        });
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_ok());
        let first_error = results
            .into_iter()
            .enumerate()
            .find_map(|(index, result)| result.err().map(|_| index));
        assert_eq!(first_error, Some(1));
    }
}
