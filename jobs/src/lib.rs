//! Jobs: work that pays money (through Basics) and experience for what players do, with levels
//! that pay more. A window to join and leave, stats and a top list.
//!
//! `defs` reads what the owner wrote and works out levels; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
