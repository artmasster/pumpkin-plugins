//! BlockLog: block logging, inspection and rollback for Pumpkin (CoreProtect-style commands,
//! MCSV's own code). Everything but `plugin` is plain Rust, unit-tested without a server.
pub mod engine;
pub mod ipc;
/// The item text, shared with other plugins through `mcsv_common` (one file, read here too
/// so the plain-Rust part of BlockLog keeps building and testing without the server API).
#[path = "../../common/src/items.rs"]
pub mod items;
pub mod params;
pub mod query;
pub mod record;
pub mod store;
pub mod timefmt;

#[cfg(target_family = "wasm")]
mod plugin;
