use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ViewState {
    ordinal: usize,
    cache: Vec<usize>,
    plans: Vec<usize>,
    queue: Vec<(usize, usize)>,
}

fn prepare(state: &mut ViewState, revision: usize) {
    state.cache.clear();
    state.cache.extend([state.ordinal, revision]);
    state.plans = state.cache.iter().map(|value| value * 3).collect();
    state.plans.push(state.ordinal + revision);
    state.queue = state.plans.iter().copied().enumerate().collect();
}

#[test]
fn scoped_view_updates_match_serial_in_original_slot_order_and_join_before_return() {
    let initial: Vec<_> = (0..11)
        .map(|ordinal| ViewState {
            ordinal,
            cache: vec![usize::MAX],
            plans: vec![usize::MAX],
            queue: vec![],
        })
        .collect();
    let mut serial = initial.clone();
    let mut parallel = initial;
    let executor = CpuExecutor::build(3).unwrap();
    for revision in [0, 1, 7, 2] {
        for state in &mut serial {
            prepare(state, revision);
        }
        execute(&mut parallel, &executor, |state, _| {
            prepare(state, revision);
        });
        assert_eq!(parallel, serial);
    }
    parallel.clear();
    parallel.push(serial[8].clone());
    execute(&mut parallel, &executor, |state, _| prepare(state, 19));
    prepare(&mut serial[8], 19);
    assert_eq!(parallel, [serial[8].clone()]);
}

#[test]
fn bounded_waves_use_explicit_pool_and_do_not_start_the_next_wave_early() {
    let executor = CpuExecutor::build(6).unwrap();
    let barrier = Arc::new(Barrier::new(MAX_JOBS));
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let mut jobs: Vec<_> = (0..MAX_JOBS * 2).collect();
    execute(&mut jobs, &executor, |ordinal, parallel| {
        assert!(parallel);
        assert_eq!(rayon::current_num_threads(), 6);
        let thread = std::thread::current();
        assert!(thread.name().unwrap().starts_with("gmpublished-cpu-"));
        if *ordinal >= MAX_JOBS {
            assert!(completed.load(Ordering::SeqCst) >= MAX_JOBS);
        }
        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(current, Ordering::SeqCst);
        barrier.wait();
        active.fetch_sub(1, Ordering::SeqCst);
        completed.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(completed.load(Ordering::SeqCst), jobs.len());
    assert_eq!(peak.load(Ordering::SeqCst), MAX_JOBS);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn single_job_and_single_worker_execute_on_calling_thread() {
    let caller = std::thread::current().id();
    for (workers, count) in [(4, 1), (1, 7)] {
        let executor = CpuExecutor::build(workers).unwrap();
        let mut jobs = vec![false; count];
        execute(&mut jobs, &executor, |done, parallel| {
            assert!(!parallel);
            assert_eq!(std::thread::current().id(), caller);
            *done = true;
        });
        assert!(jobs.iter().all(|done| *done));
    }
    let executor = CpuExecutor::build(2).unwrap();
    execute::<()>(&mut [], &executor, |_, _| {
        panic!("empty work must not execute")
    });
}
