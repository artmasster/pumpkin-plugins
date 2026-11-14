//! Monitor: performance monitoring for Pumpkin (spark-style commands, MCSV's own code).
//! `stats` is plain Rust (tick history, percentiles, recordings; unit-tested without a server),
//! `plugin` is the WASM glue.
pub mod stats;

#[cfg(target_family = "wasm")]
mod plugin;
