//! CustomItems: MCSV's own answer to Oraxen, ItemsAdder and Nexo for Pumpkin.
//!
//! The server owner describes items in `items/*.toml` (a name, a texture or a model, a recipe)
//! and drops pictures into `textures/`. The plugin builds the resource pack that makes the
//! items look right, sends it to players when they join, hands the items out by command, by a
//! chest window and by crafting, and gives every glyph in `glyphs.toml` a character that can
//! be typed into any text.
//!
//! `defs` (what the owner wrote) and `pack` (the resource pack and its zip) are plain Rust,
//! unit-tested without a server; `plugin` is the WASM glue.
pub mod defs;
pub mod pack;

#[cfg(target_family = "wasm")]
mod plugin;
