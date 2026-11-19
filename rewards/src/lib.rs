//! Rewards: a reward for each day a player comes back in a row (`/daily`), and rewards for time
//! played. Items (game or CustomItems), money (Basics) and console commands.
//!
//! `defs` holds the rules and the counting; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
