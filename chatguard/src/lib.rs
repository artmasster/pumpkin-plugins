//! ChatGuard: bad words (Thai and English, seen through spaces, repeated letters and look-alike
//! digits), adverts for other servers, spam and shouting, for chat and signs.
//!
//! `defs` checks one line; `plugin` is the glue to Pumpkin (and the check Basics asks for).
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
