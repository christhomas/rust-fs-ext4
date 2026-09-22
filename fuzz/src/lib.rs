//! Shared setup for the fuzz targets.
//!
//! The device and the walk live in `fuzz/shared/walk.rs`, which
//! `tests/fuzz_decoders.rs` includes too -- see that file for why it is
//! shared textually rather than as a dependency.

include!("../shared/walk.rs");
