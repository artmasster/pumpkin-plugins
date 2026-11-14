//! ImageMap: MCSV's own answer to ImageOnMap for Pumpkin.
//!
//! A picture (a link, or a file in the plugin folder) becomes map items: one map, or a poster of
//! several maps to hang side by side in item frames. The picture is scaled, matched to the 248
//! colours a Minecraft map can show (with dithering), kept on disk, and sent to every player.
//!
//! `cfg`, `palette`, `picture` and `store` are plain Rust (unit-tested without a server);
//! `plugin` is the WASM glue.
pub mod cfg;
pub mod palette;
pub mod picture;
pub mod store;

// The vanilla map colour table has one owner: WorldMap's generated file.
#[path = "../../worldmap/src/mapcolors.rs"]
mod mapcolors;

#[cfg(target_family = "wasm")]
mod plugin;
