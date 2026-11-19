//! Pets: creatures (or models of the Models plugin) that follow their players, cannot be hurt
//! and can be ridden. Players call them out from a window and give them names.
//!
//! `defs` reads what the owner wrote and works out riding; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
