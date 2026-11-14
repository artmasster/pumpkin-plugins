//! Login: login for offline-mode Pumpkin servers (AuthMe-style, MCSV's own code).
//! `auth` is plain Rust (accounts, password rules, hashing, limits; unit-tested without a
//! server), `lang` the messages, `plugin` the WASM glue.
pub mod auth;
pub mod lang;

#[cfg(target_family = "wasm")]
mod plugin;
