//! Skills: abilities that level up with what players do (mining, woodcutting, combat, fishing,
//! or any the owner writes), with perks: a chance of double drops, more damage.
//!
//! `defs` reads what the owner wrote and works out levels and perks; `plugin` is the glue.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
