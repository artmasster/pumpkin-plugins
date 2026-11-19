//! Regions: boxes of the world with flags (build, interact, pvp, mob-damage, mob-spawning,
//! explosions, fire-spread, entry, item-drop, fall-damage, invincible, hunger, greeting,
//! farewell), members and owners, priorities and a global region per world.
//!
//! `defs` holds the regions and how flags are decided; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
