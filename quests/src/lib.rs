//! Quests: goals (break, kill, fish, craft, talk to an NPC, bring items …) with rewards (items,
//! money, commands), given and handed in at NPCs of the Npcs plugin or taken from a window,
//! with requirements and cooldowns.
//!
//! `defs` reads what the owner wrote and works out progress; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
