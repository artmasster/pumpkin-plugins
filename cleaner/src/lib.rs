//! Cleaner: ground item and entity cleanup for Pumpkin (ClearLag-style, MCSV's own code).
//! `plan` is plain Rust (what to remove, when to warn; unit-tested without a server),
//! `plugin` is the WASM glue.
pub mod plan;

#[cfg(target_family = "wasm")]
mod plugin;
