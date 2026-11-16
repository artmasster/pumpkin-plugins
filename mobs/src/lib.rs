//! Mobs: creatures of the game with their own stats, behaviour, skills, drops and spawners.
//!
//! `defs` reads what the owner wrote and needs no server; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
