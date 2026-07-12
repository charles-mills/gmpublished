//! Ignored deterministic mutation harness for the `.phy` parser.
//!
//! Run a bounded smoke:
//! `FUZZ_SECONDS=30 cargo test --test phy_fuzz -- --ignored --nocapture`
//!
//! Run a longer local pass:
//! `FUZZ_SECONDS=600 FUZZ_SEED=0xfeed_f00d_5653_5048 cargo test --test phy_fuzz -- --ignored --nocapture`

#[path = "phy_wild_common/mod.rs"]
mod common;

use std::{
    collections::BTreeSet,
    env,
    fmt::{self, Write as _},
    fs,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::all_synthetic_seeds;

const DEFAULT_FUZZ_SECONDS: u64 = 30;
const DEFAULT_RUN_SEED: u64 = 0xfeed_f00d_5653_5048;
const MINIMIZE_BUDGET: Duration = Duration::from_secs(15);
const MAX_CORPUS_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[test]
#[ignore = "panic-hunting harness: set FUZZ_SECONDS to choose duration"]
fn fuzz_phy_parser() {
    let findings_dir = env::var_os("FUZZ_FINDINGS_DIR")
        .map_or_else(|| PathBuf::from("target/fuzz-findings"), PathBuf::from);
    fs::create_dir_all(&findings_dir).expect("fuzz findings directory");

    let fuzz_seconds = env_u64("FUZZ_SECONDS", DEFAULT_FUZZ_SECONDS);
    let run_seed = env_u64("FUZZ_SEED", DEFAULT_RUN_SEED);
    let seeds = discover_seeds();
    eprintln!(
        "FUZZ_CONFIG target=phy seconds={} run_seed={:#018x} findings_dir={} seeds={}",
        fuzz_seconds,
        run_seed,
        findings_dir.display(),
        seeds.len(),
    );

    let report = fuzz_target(
        &seeds,
        Duration::from_secs(fuzz_seconds),
        run_seed,
        &findings_dir,
        |bytes| {
            let _ = vformats::phy::parse_lossy(bytes, &vformats::Limits::default());
        },
    );
    print_report(&report);
    write_summary(&findings_dir.join("summary.txt"), &report).expect("write fuzz summary");

    assert_eq!(report.stats.panic_count, 0, "fuzz findings: {report:#?}");
}

#[test]
#[ignore = "local replay helper: set FUZZ_REPRO_PATH"]
fn replay_fuzz_reproducer() {
    let Some(path) = env::var_os("FUZZ_REPRO_PATH").map(PathBuf::from) else {
        eprintln!("skipping reproducer replay: FUZZ_REPRO_PATH is not set");
        return;
    };
    let bytes = fs::read(&path).expect("reproducer bytes should be readable");
    let _ = vformats::phy::parse_lossy(&bytes, &vformats::Limits::default());
}

#[derive(Debug, Clone)]
struct FuzzSeed {
    name: String,
    bytes: Vec<u8>,
}

impl FuzzSeed {
    fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct PanicSignature {
    message: String,
    location: String,
}

impl PanicSignature {
    fn unknown() -> Self {
        Self {
            message: "<panic payload unavailable>".to_owned(),
            location: "<unknown>".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
struct FuzzFinding {
    signature: PanicSignature,
    seed_name: String,
    iteration: u64,
    prng_seed: u64,
    original_path: PathBuf,
    minimized_path: PathBuf,
    original_len: usize,
    minimized_len: usize,
}

#[derive(Debug, Clone)]
struct FuzzStats {
    seed_count: usize,
    iterations: u64,
    panic_count: u64,
    unique_panic_count: usize,
    elapsed: Duration,
}

#[derive(Debug, Clone)]
struct FuzzReport {
    stats: FuzzStats,
    findings: Vec<FuzzFinding>,
}

fn fuzz_target<F>(
    seeds: &[FuzzSeed],
    duration: Duration,
    run_seed: u64,
    findings_dir: &Path,
    mut parser: F,
) -> FuzzReport
where
    F: FnMut(&[u8]),
{
    let started = Instant::now();
    let mut iterations = 0_u64;
    let mut panic_count = 0_u64;
    let mut unique = BTreeSet::<PanicSignature>::new();
    let mut findings = Vec::new();

    if seeds.is_empty() || duration.is_zero() {
        return FuzzReport {
            stats: FuzzStats {
                seed_count: seeds.len(),
                iterations,
                panic_count,
                unique_panic_count: 0,
                elapsed: started.elapsed(),
            },
            findings,
        };
    }

    let deadline = started + duration;
    let mut scheduler = SplitMix64::new(run_seed ^ 0x5650_4859_0000_0001);
    while Instant::now() < deadline {
        let seed_index = iterations as usize % seeds.len();
        let seed = &seeds[seed_index];
        let prng_seed = scheduler.next_u64();
        let mut mutator = SplitMix64::new(prng_seed);
        let bytes = mutate_bytes(&seed.bytes, &mut mutator);

        if let Some(signature) = capture_panic(&mut parser, &bytes) {
            panic_count += 1;
            if unique.insert(signature.clone()) {
                let original_len = bytes.len();
                let hash = fnv1a64(&bytes);
                let original_path = findings_dir.join(format!("phy-{hash:016x}.bin"));
                fs::write(&original_path, &bytes).expect("fuzz finding should be writable");

                let minimized = minimize(&mut parser, &bytes, &seed.bytes, &signature);
                let minimized_len = minimized.len();
                let minimized_path = findings_dir.join(format!("phy-{hash:016x}-min.bin"));
                fs::write(&minimized_path, &minimized)
                    .expect("minimized fuzz finding should be writable");

                let finding = FuzzFinding {
                    signature,
                    seed_name: seed.name.clone(),
                    iteration: iterations,
                    prng_seed,
                    original_path,
                    minimized_path,
                    original_len,
                    minimized_len,
                };
                eprintln!(
                    "FUZZ_FINDING target=phy seed={} iteration={} prng_seed={:#018x} panic={} location={} minimized={} bytes",
                    finding.seed_name,
                    finding.iteration,
                    finding.prng_seed,
                    finding.signature.message,
                    finding.signature.location,
                    finding.minimized_len
                );
                findings.push(finding);
            }
        }

        iterations += 1;
    }

    FuzzReport {
        stats: FuzzStats {
            seed_count: seeds.len(),
            iterations,
            panic_count,
            unique_panic_count: unique.len(),
            elapsed: started.elapsed(),
        },
        findings,
    }
}

fn capture_panic<F>(parser: &mut F, bytes: &[u8]) -> Option<PanicSignature>
where
    F: FnMut(&[u8]),
{
    let slot = Arc::new(Mutex::new(None::<PanicSignature>));
    let hook_slot = Arc::clone(&slot);
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let message = panic_info.payload().downcast_ref::<&str>().map_or_else(
            || {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map_or_else(|| "<non-string panic payload>".to_owned(), Clone::clone)
            },
            |message| (*message).to_owned(),
        );
        let location = panic_info.location().map_or_else(
            || "<unknown>".to_owned(),
            |location| format!("{}:{}", location.file(), location.line()),
        );
        *hook_slot.lock().expect("panic capture lock") = Some(PanicSignature { message, location });
    }));

    let result = panic::catch_unwind(AssertUnwindSafe(|| parser(bytes)));
    panic::set_hook(previous);

    match result {
        Ok(()) => None,
        Err(_) => Some(
            slot.lock()
                .expect("panic capture lock")
                .clone()
                .unwrap_or_else(PanicSignature::unknown),
        ),
    }
}

fn minimize<F>(parser: &mut F, bytes: &[u8], seed: &[u8], signature: &PanicSignature) -> Vec<u8>
where
    F: FnMut(&[u8]),
{
    let deadline = Instant::now() + MINIMIZE_BUDGET;
    let mut current = bytes.to_vec();
    minimize_by_removing_chunks(parser, &mut current, signature, deadline);
    minimize_by_reverting_to_seed(parser, &mut current, seed, signature, deadline);
    current
}

fn minimize_by_removing_chunks<F>(
    parser: &mut F,
    current: &mut Vec<u8>,
    signature: &PanicSignature,
    deadline: Instant,
) where
    F: FnMut(&[u8]),
{
    let mut chunk = current.len().saturating_div(2).max(1);
    while chunk > 0 && !current.is_empty() && Instant::now() < deadline {
        let mut offset = 0;
        let mut changed = false;
        while offset < current.len() && Instant::now() < deadline {
            let end = offset.saturating_add(chunk).min(current.len());
            let mut candidate = Vec::with_capacity(current.len() - (end - offset));
            candidate.extend_from_slice(&current[..offset]);
            candidate.extend_from_slice(&current[end..]);
            if same_panic(parser, &candidate, signature) {
                *current = candidate;
                changed = true;
            } else {
                offset = end;
            }
        }
        if changed {
            chunk = chunk.min(current.len().max(1));
        } else {
            chunk /= 2;
        }
    }
}

fn minimize_by_reverting_to_seed<F>(
    parser: &mut F,
    current: &mut Vec<u8>,
    seed: &[u8],
    signature: &PanicSignature,
    deadline: Instant,
) where
    F: FnMut(&[u8]),
{
    let comparable = current.len().min(seed.len());
    if comparable == 0 {
        return;
    }

    let mut chunk = comparable.saturating_div(2).max(1);
    while chunk > 0 && Instant::now() < deadline {
        let mut offset = 0;
        let mut changed = false;
        while offset < comparable && Instant::now() < deadline {
            let end = offset.saturating_add(chunk).min(comparable);
            if current[offset..end] == seed[offset..end] {
                offset = end;
                continue;
            }
            let mut candidate = current.clone();
            candidate[offset..end].copy_from_slice(&seed[offset..end]);
            if same_panic(parser, &candidate, signature) {
                *current = candidate;
                changed = true;
            }
            offset = end;
        }
        if changed {
            chunk = chunk.min(comparable.max(1));
        } else {
            chunk /= 2;
        }
    }
}

fn same_panic<F>(parser: &mut F, bytes: &[u8], signature: &PanicSignature) -> bool
where
    F: FnMut(&[u8]),
{
    capture_panic(parser, bytes).as_ref() == Some(signature)
}

fn mutate_bytes(seed: &[u8], rng: &mut SplitMix64) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    let mutation_count = 1 + rng.usize(8);
    for _ in 0..mutation_count {
        match rng.usize(5) {
            0 => flip_or_set_byte(&mut bytes, rng),
            1 => swap_chunks(&mut bytes, rng),
            2 => truncate(&mut bytes, rng),
            3 => perturb_dword(&mut bytes, rng),
            _ => zero_fill(&mut bytes, rng),
        }
    }
    bytes
}

fn flip_or_set_byte(bytes: &mut [u8], rng: &mut SplitMix64) {
    if bytes.is_empty() {
        return;
    }
    let offset = rng.usize(bytes.len());
    if rng.bool() {
        bytes[offset] ^= 1_u8 << rng.usize(8);
    } else {
        bytes[offset] = rng.next_u8();
    }
}

fn swap_chunks(bytes: &mut [u8], rng: &mut SplitMix64) {
    if bytes.len() < 2 {
        return;
    }
    let max_span = bytes.len().saturating_div(4).clamp(1, 256);
    let span = 1 + rng.usize(max_span.min(bytes.len()));
    let left = rng.usize(bytes.len() - span + 1);
    let right = rng.usize(bytes.len() - span + 1);
    for index in 0..span {
        bytes.swap(left + index, right + index);
    }
}

fn truncate(bytes: &mut Vec<u8>, rng: &mut SplitMix64) {
    if bytes.is_empty() {
        return;
    }
    bytes.truncate(rng.usize(bytes.len() + 1));
}

fn perturb_dword(bytes: &mut [u8], rng: &mut SplitMix64) {
    if bytes.len() < 4 {
        return;
    }
    let offset = rng.usize(bytes.len() - 3);
    let current = u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]);
    let value = match rng.usize(8) {
        0 => 0,
        1 => 1,
        2 => bytes.len() as u32,
        3 => (bytes.len() as u32).saturating_add(rng.next_u8() as u32),
        4 => 0x7fff,
        5 => 0xffff,
        6 => current.wrapping_add(1 + rng.usize(4096) as u32),
        _ => current ^ (rng.next_u64() as u32),
    };
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn zero_fill(bytes: &mut [u8], rng: &mut SplitMix64) {
    if bytes.is_empty() {
        return;
    }
    let start = rng.usize(bytes.len());
    let remaining = bytes.len() - start;
    let span = 1 + rng.usize(remaining.min(256));
    bytes[start..start + span].fill(0);
}

#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    fn usize(&mut self, upper_exclusive: usize) -> usize {
        debug_assert!(upper_exclusive > 0);
        (self.next_u64() as usize) % upper_exclusive
    }
}

fn discover_seeds() -> Vec<FuzzSeed> {
    let mut seeds = all_synthetic_seeds()
        .into_iter()
        .map(|(name, bytes)| FuzzSeed::new(format!("synthetic/{name}.phy"), bytes))
        .collect::<Vec<_>>();
    if let Some(root) = env::var_os("VPHY_CORPUS_DIR").map(PathBuf::from) {
        collect_phy_files(&root, &mut seeds);
    }
    seeds
}

fn collect_phy_files(root: &Path, seeds: &mut Vec<FuzzSeed>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("phy"))
            {
                push_file_seed(seeds, &path);
            }
        }
    }
}

fn push_file_seed(seeds: &mut Vec<FuzzSeed>, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() > MAX_CORPUS_FILE_BYTES {
        return;
    }
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    seeds.push(FuzzSeed::new(path.display().to_string(), bytes));
}

fn print_report(report: &FuzzReport) {
    let elapsed = report.stats.elapsed.as_secs_f64();
    let throughput = if elapsed > 0.0 {
        report.stats.iterations as f64 / elapsed
    } else {
        0.0
    };
    eprintln!(
        "FUZZ_TARGET_DONE target=phy seeds={} iterations={} elapsed={elapsed:.3}s throughput={throughput:.1}/s panics={} unique={}",
        report.stats.seed_count,
        report.stats.iterations,
        report.stats.panic_count,
        report.stats.unique_panic_count
    );
}

fn write_summary(path: &Path, report: &FuzzReport) -> std::io::Result<()> {
    let elapsed = report.stats.elapsed.as_secs_f64();
    let throughput = if elapsed > 0.0 {
        report.stats.iterations as f64 / elapsed
    } else {
        0.0
    };
    let mut out = String::new();
    out.push_str("target,seed_count,iterations,elapsed_seconds,iterations_per_second,panic_count,unique_panic_count\n");
    write!(
        out,
        "phy,{},{},{elapsed:.3},{throughput:.1},{},{}\n\n",
        report.stats.seed_count,
        report.stats.iterations,
        report.stats.panic_count,
        report.stats.unique_panic_count
    )
    .expect("writing to a String is infallible");
    out.push_str("panic|location|seed|iteration|prng_seed|original_path|original_len|minimized_path|minimized_len\n");
    for finding in &report.findings {
        writeln!(
            out,
            "{}|{}|{}|{}|{:#018x}|{}|{}|{}|{}",
            finding.signature.message.replace('|', " "),
            finding.signature.location.replace('|', " "),
            finding.seed_name.replace('|', " "),
            finding.iteration,
            finding.prng_seed,
            finding.original_path.display(),
            finding.original_len,
            finding.minimized_path.display(),
            finding.minimized_len
        )
        .expect("writing to a String is infallible");
    }
    fs::write(path, out)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| {
            value.strip_prefix("0x").map_or_else(
                || value.parse::<u64>().ok(),
                |hex| u64::from_str_radix(hex, 16).ok(),
            )
        })
        .unwrap_or(default)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl fmt::Display for PanicSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.message, self.location)
    }
}
