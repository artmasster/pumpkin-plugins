//! Crops: plants of your own that players plant with a seed, that grow through stages with
//! time and are picked when ripe. Their stages look like items (of the game or of CustomItems)
//! or like animations of a model of the Models plugin.
//!
//! `defs` reads what the owner wrote and works out growing; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
