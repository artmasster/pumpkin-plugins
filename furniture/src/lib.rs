//! Furniture: pieces of your own that players place with an item, sit on and take back by
//! hitting them. Drawn as item models (of CustomItems or any pack) or models of the Models
//! plugin; a piece may be solid (a barrier block).
//!
//! `defs` reads what the owner wrote; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
