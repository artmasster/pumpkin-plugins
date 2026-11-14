//! WorldTools: world editing for Pumpkin (WorldEdit-style commands, MCSV's own code).
//! `geo` (regions, shapes), `spec` (block states, patterns, masks), `xform` (rotate and flip)
//! and `schem` (schematic files) are plain Rust, unit-tested without a server; `plugin` is the
//! WASM glue.
pub mod geo;
pub mod schem;
pub mod spec;
pub mod xform;

#[cfg(target_family = "wasm")]
mod plugin;
