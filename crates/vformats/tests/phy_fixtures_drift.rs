//! The committed fixture files are generated from the synthetic builders in
//! `tests/common`. The drift test keeps them byte-identical forever; the
//! ignored test regenerates them after intentional builder changes:
//!
//! ```sh
//! cargo test --test phy_fixtures_drift -- --ignored regenerate_fixtures
//! ```

#[path = "phy_wild_common/mod.rs"]
mod common;

use common::compact_phy;

const TETRA_PATH: &str = "tests/fixtures/tetra.phy";

#[test]
fn committed_fixtures_match_their_builders() {
    let committed = std::fs::read(TETRA_PATH).expect("read committed tetra.phy");
    assert_eq!(
        committed,
        compact_phy(),
        "tests/fixtures/tetra.phy has drifted from common::compact_phy(); \
         regenerate it with `cargo test --test phy_fixtures_drift -- --ignored regenerate_fixtures`"
    );
}

#[test]
#[ignore = "writes tests/fixtures from the synthetic builders"]
fn regenerate_fixtures() {
    std::fs::write(TETRA_PATH, compact_phy()).expect("write tetra.phy");
}
