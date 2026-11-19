//! Market: players sell items to each other for money of the Basics plugin. Listings run for a
//! while, unsold items come back to a box, and the market can take a fee and a tax.
//!
//! `defs` holds the rules and the records; `plugin` is the glue to Pumpkin.
pub mod defs;

#[cfg(target_family = "wasm")]
mod plugin;
