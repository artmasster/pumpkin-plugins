//! Voice: MCSV's proximity voice for Pumpkin. Sends where players are to the voice bridge five
//! times a second (signed UDP datagrams, the same wire as the Paper and Endstone plugins), links
//! the app with `/voice <code>`, marks who is talking, and lets ops mute a player.
//!
//! `defs` is the wire format; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
