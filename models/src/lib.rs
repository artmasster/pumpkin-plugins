//! Models: animated models made in Blockbench, shown with item displays.
//!
//! Everything that needs no server is here and tested natively: the math, reading a
//! `.bbmodel`, the bones and their poses, the animations, the resource pack. `plugin` is the
//! glue to Pumpkin.
pub mod anim;
pub mod bbmodel;
pub mod defs;
pub mod math;
pub mod molang;
pub mod pack;
pub mod rig;

#[cfg(target_family = "wasm")]
mod plugin;
