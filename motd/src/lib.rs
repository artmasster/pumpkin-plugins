//! Motd: the server list message and a maintenance mode for Pumpkin
//! (MiniMOTD and Maintenance style, MCSV's own code).
//! `sign` is plain Rust (settings, which message to show, who may join; unit-tested without a
//! server), `plugin` is the WASM glue.
pub mod sign;

#[cfg(target_family = "wasm")]
mod plugin;
