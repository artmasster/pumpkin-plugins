//! Skins: MCSV's own answer to SkinsRestorer for Pumpkin.
//!
//! On an offline-mode server nobody has a skin. This plugin gives every player the skin of the
//! Mojang account with the same name (or one of the server's default skins), lets players pick
//! any other account's skin or an image link with `/skin`, and remembers the choice.
//!
//! `cfg` and `store` are plain Rust (unit-tested without a server); `plugin` is the WASM glue.
pub mod cfg;
pub mod store;

#[cfg(target_family = "wasm")]
mod plugin;
