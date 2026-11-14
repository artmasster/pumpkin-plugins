//! Placeholders: MCSV's own answer to PlaceholderAPI for Pumpkin.
//!
//! A placeholder is `%id_params%` inside any text: `%player_name%`, `%server_online%`,
//! `%basics_balance%`. This plugin knows the player, world and server ones itself, reads the
//! Perms ones from the player, answers `%custom_<name>%` from its config, and asks the plugin
//! named `id` for everything else. Other plugins hand it a text and get it back filled in.
//!
//! `parse` and `cfg` are plain Rust (unit-tested without a server); `plugin` is the WASM glue.
pub mod cfg;
pub mod parse;

#[cfg(target_family = "wasm")]
mod plugin;
