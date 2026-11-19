//! Crates: blocks opened with keys. A window spins through the rewards and stops on the one
//! drawn by weight: items (of the game or CustomItems), money (Basics) or console commands.
//!
//! `defs` reads what the owner wrote and draws; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
