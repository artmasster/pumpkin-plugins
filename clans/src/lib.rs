//! Clans: groups of players with a tag, ranks (leader, officers, members), invites, a home,
//! clan chat (`/cc`) and no friendly fire. Tags reach chat and the tab list through Placeholders.
//!
//! `defs` holds the clans and their rules; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
