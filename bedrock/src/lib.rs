//! Bedrock: shows the models of the Models plugin to Bedrock players.
//!
//! A Bedrock client cannot draw the item displays a model is made of on Java. It can draw an
//! entity of its own kind when a resource pack says how that looks, so every model becomes one
//! (`models:<name>`), and this plugin tells Bedrock players where the models are and what
//! they do.
//!
//! `pack` and `ids` are plain Rust (unit-tested without a server); `plugin` is the native
//! Pumpkin glue.
pub mod ids;
pub mod pack;

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
pub struct BedrockPlugin;

#[cfg(feature = "plugin")]
impl BedrockPlugin {
    #[must_use]
    pub fn new() -> Self {
        BedrockPlugin
    }
}

#[cfg(feature = "plugin")]
impl Default for BedrockPlugin {
    fn default() -> Self {
        Self::new()
    }
}
