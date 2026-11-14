//! Basics: server essentials for Pumpkin (CMI-style feature set, MCSV's own code).
//! `store` and `lang` are plain Rust; `plugin` is the WASM glue.
pub mod lang;
pub mod store;

#[cfg(target_family = "wasm")]
mod extra;
#[cfg(target_family = "wasm")]
mod gui;
#[cfg(target_family = "wasm")]
mod holo;
#[cfg(target_family = "wasm")]
mod more;
#[cfg(target_family = "wasm")]
mod plugin;
#[cfg(target_family = "wasm")]
mod plus;
