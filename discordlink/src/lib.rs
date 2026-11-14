//! DiscordLink: Discord bridge for Pumpkin (DiscordSRV-style, MCSV's own code).
//! `bridge` is plain Rust (settings, the outgoing queue, text in both directions; unit-tested
//! without a server or a network), `plugin` is the WASM glue.
pub mod bridge;

#[cfg(target_family = "wasm")]
mod plugin;
