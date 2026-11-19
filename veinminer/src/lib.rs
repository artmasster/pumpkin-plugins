//! VeinMiner: breaking one block of an ore vein or a tree, sneaking with the right tool, breaks
//! the rest of it too, with the drops in the inventory and the tool worn for each block.
//!
//! `defs` holds the rules and walks the vein; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
