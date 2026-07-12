/// Caps applied to untrusted input. Every parse entry point takes one.
///
/// The defaults are tuned against real Source-engine game and community
/// content (the largest known legitimate files fit comfortably); tighten
/// them when parsing input from less trusted sources, or raise them for
/// offline batch tooling.
///
/// These are the *generic* caps. Some formats additionally enforce
/// fixed structural bounds of their own (for example [`crate::phy`]
/// caps solids and ledges per file); those are documented in the
/// format's module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Whole-input cap in bytes, checked before any parsing.
    pub max_input_bytes: u64,
    /// Cap on a single contained entry (GMA/VPK/pakfile payloads).
    pub max_entry_bytes: u64,
    /// Cap on entry/pair counts (archive entry tables, KeyValues pairs).
    pub max_entries: usize,
    /// Maximum KeyValues block nesting before parsing errors out.
    pub max_kv_depth: usize,
    /// Cap on *detailed* skip records collected by lossy readers. Counts
    /// stay accurate past the cap; only per-record detail stops growing,
    /// so a corrupt file cannot exhaust memory through diagnostics.
    pub max_stat_records: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024 * 1024,
            max_entry_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_kv_depth: 64,
            max_stat_records: 1024,
        }
    }
}
