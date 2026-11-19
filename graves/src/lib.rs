//! Graves: at a death the player's items go into a grave where they died, which its owner opens
//! to get everything back in place; after a while anyone may open it, and later it is gone.
//!
//! `defs` holds the rules; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
