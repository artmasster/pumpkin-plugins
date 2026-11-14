//! Perms: permissions for Pumpkin (LuckPerms-style feature set, MCSV's own code).
//! `node`, `model`, `store`, `lang` and `cmd` are plain Rust (unit-tested without a server);
//! `plugin` is the WASM glue.
pub mod cmd;
pub mod lang;
pub mod model;
pub mod node;
pub mod store;

#[cfg(target_family = "wasm")]
mod plugin;
