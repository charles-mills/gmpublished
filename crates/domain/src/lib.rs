//! Renderer-facing Source preview domain shared by decoding and presentation.
//!
//! This crate deliberately contains no application configuration, Steam,
//! filesystem service, transaction, or executor state. Its values and
//! algorithms can therefore cross the backend/application boundary without
//! exposing either side's operational machinery.

pub mod math;
pub mod particles;
pub mod scene;
