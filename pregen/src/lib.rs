//! Pregen: chunk pre-generator for Pumpkin with the command set of Chunky.
//! `shape`, `pattern`, `task` and `lang` are plain Rust (unit-tested without a
//! server); `plugin` is the native Pumpkin glue.
pub mod args;
pub mod lang;
pub mod pattern;
pub mod shape;
pub mod task;

#[cfg(feature = "plugin")]
mod plugin;

#[cfg(feature = "plugin")]
use pumpkin_api_macros::{plugin_impl, plugin_method};

#[cfg(feature = "plugin")]
#[plugin_method]
async fn on_load(&self, context: std::sync::Arc<pumpkin::plugin::Context>) -> Result<(), String> {
    plugin::load(&context)
}

#[cfg(feature = "plugin")]
#[plugin_method]
async fn on_unload(&self, _context: std::sync::Arc<pumpkin::plugin::Context>) -> Result<(), String> {
    plugin::unload()
}

#[cfg(feature = "plugin")]
#[plugin_impl]
pub struct PregenPlugin;

#[cfg(feature = "plugin")]
impl PregenPlugin {
    #[must_use]
    pub fn new() -> Self {
        PregenPlugin
    }
}

#[cfg(feature = "plugin")]
impl Default for PregenPlugin {
    fn default() -> Self {
        Self::new()
    }
}
