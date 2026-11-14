//! WorldMap: a map of the world for Pumpkin (MCSV's own answer to BlueMap, Dynmap and squaremap).
//!
//! What it is: a 2D top-down map, one pixel per block, shaded by height like the vanilla map
//! item, of every chunk that was loaded while the plugin ran. It is kept on disk as tiles and
//! shown two ways: a live page served by the plugin itself on one of the server's extra ports
//! (`web`, like squaremap's built-in web server), and one self-contained HTML file exported on
//! request. What it is not: 3D. A WASM plugin cannot load chunks or read region files.
//!
//! `color`, `tile`, `store`, `export`, `web` and `cfg` are plain Rust (unit-tested without a server);
//! `plugin` is the WASM glue.
pub mod cfg;
pub mod color;
pub mod export;
mod mapcolors;
pub mod store;
pub mod tile;
pub mod web;

#[cfg(target_family = "wasm")]
mod plugin;
