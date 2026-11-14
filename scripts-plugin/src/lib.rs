//! Scripts: scripting for Pumpkin in Skript's syntax (MCSV's own implementation).
//! `value`, `pattern`, `ast`, `parse`, `engine` and `cfg` are plain Rust (unit-tested without a
//! server); `plugin` is the WASM glue.
pub mod ast;
pub mod cfg;
pub mod engine;
pub mod parse;
pub mod pattern;
pub mod value;

#[cfg(test)]
mod tests;

#[cfg(target_family = "wasm")]
mod plugin;
#[cfg(target_family = "wasm")]
mod tables;
