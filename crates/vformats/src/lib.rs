//! Parsers for Source 1 engine formats with one consistent, hardened API.
//!
//! Design contract:
//!
//! - **Sans-io**: input is `&str`/`&[u8]`; the caller does I/O.
//! - **Hardened**: every entry point takes a [`Limits`]; malformed or
//!   adversarial input returns an error or a documented lossy skip —
//!   never a panic (`unsafe_code` is forbidden crate-wide).
//! - **Zero-copy where the bytes allow it**: borrowed views (`Cow`,
//!   `&[u8]`) when data is already in final form; owned buffers only
//!   where transformation is inherent.
//! - **Strict vs. lossy by format role**: archive formats fail loudly on
//!   corruption; renderer-feeding formats skip damaged structures and
//!   report what was skipped. Both always validate the container header.
//!
//! One module per format behind an additive cargo feature (default
//! all): [`keyvalues`] (and its dialects [`vmt`] and [`soundscript`]),
//! [`vtf`], [`phy`], [`vpk`], [`gma`], [`mdl`], and [`bsp`].
//!
//! # Example
//!
//! Every format follows the same shape: hand a byte slice (or `&str`)
//! and a [`Limits`] to the module's parse entry point, get a typed
//! view back.
//!
//! ```
//! use vformats::keyvalues;
//! use vformats::Limits;
//!
//! let limits = Limits::default();
//! let doc = keyvalues::parse(
//!     r#""Material" { "$basetexture" "concrete/floor01" }"#,
//!     &limits,
//! )?;
//! let material = doc.get("material").and_then(|v| v.as_block()).unwrap();
//! assert_eq!(material.get_str("$basetexture"), Some("concrete/floor01"));
//! # Ok::<(), vformats::keyvalues::KvError>(())
//! ```

#![warn(missing_docs, missing_debug_implementations)]
#![warn(clippy::must_use_candidate)]
#![forbid(unsafe_code)]

// Compile the README's examples as doctests so they cannot rot.
#[cfg(all(feature = "keyvalues", doctest))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod limits;
pub use limits::Limits;

mod math;

#[cfg(any(feature = "vtf", feature = "vpk", feature = "gma", feature = "mdl"))]
mod reader;

mod crc32;
pub use crc32::crc32_ieee;

mod entry_path;
pub use entry_path::is_unsafe_entry_path;

mod sink;
pub use sink::{IoSink, Sink};

#[cfg(feature = "bsp")]
pub mod bsp;

#[cfg(feature = "gma")]
pub mod gma;

#[cfg(feature = "keyvalues")]
pub mod keyvalues;

#[cfg(feature = "mdl")]
pub mod mdl;

#[cfg(feature = "phy")]
pub mod phy;

#[cfg(feature = "soundscript")]
pub mod soundscript;

#[cfg(feature = "vmt")]
pub mod vmt;

#[cfg(feature = "vpk")]
pub mod vpk;

#[cfg(feature = "vtf")]
pub mod vtf;
