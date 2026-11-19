//! Npcs: characters that stand, turn to players, walk a path and do things when clicked:
//! messages, commands, dialog windows and quests. A creature of the game, or a model of the
//! Models plugin drawn over one.
//!
//! `defs` is what an NPC is and what its actions say; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
