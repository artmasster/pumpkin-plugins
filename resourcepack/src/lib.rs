//! ResourcePack: MCSV's own answer to Resource Pack Downloader and ForcePack for Pumpkin.
//!
//! The server owner lists packs (a name and a download link) in the config; the plugin sends
//! them to players when they join, by world if wanted, works out each pack's SHA-1 itself,
//! and acts when a player declines a pack that is required.
//!
//! `cfg` is plain Rust (unit-tested without a server); `plugin` is the WASM glue.
pub mod cfg;

#[cfg(target_family = "wasm")]
mod plugin;
