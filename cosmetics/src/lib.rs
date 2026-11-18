//! Cosmetics: hats, things on the back and balloons that players put on themselves from a
//! wardrobe window. The Models plugin draws them: a model of its own, or an item of a resource
//! pack (CustomItems) shown as a model.
//!
//! `defs` reads what the owner wrote and needs no server; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
