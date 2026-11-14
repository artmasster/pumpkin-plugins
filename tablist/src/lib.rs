//! TabList: the tab list for Pumpkin (TAB-style feature set, MCSV's own code).
//! `cfg` is plain Rust (unit-tested without a server); `plugin` is the WASM glue.
pub mod cfg;

#[cfg(target_family = "wasm")]
mod plugin;
