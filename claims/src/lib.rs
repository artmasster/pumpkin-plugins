//! Claims: land claims for Pumpkin (GriefPrevention-style, MCSV's own code).
//! `land` is plain Rust (claims, trust, claim blocks; unit-tested without a server),
//! `plugin` is the WASM glue.
pub mod land;

#[cfg(target_family = "wasm")]
mod plugin;
