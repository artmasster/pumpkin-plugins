//! Throwaway lab plugin (not shipped): a blocking `PlayerPermissionCheckEvent` handler that does
//! nothing, to measure what Pumpkin itself charges per check once any plugin listens.
#![cfg(target_family = "wasm")]

use pumpkin_plugin_api::events::{EventHandler, EventPriority, FromIntoEvent, PlayerPermissionCheckEvent};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

struct OnCheck;
impl EventHandler<PlayerPermissionCheckEvent> for OnCheck {
    fn handle(&self, _server: Server, e: <PlayerPermissionCheckEvent as FromIntoEvent>::Data) -> <PlayerPermissionCheckEvent as FromIntoEvent>::Data {
        e
    }
}

struct Noop;

impl Plugin for Noop {
    fn new() -> Self {
        Noop
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata { name: "permsnoop".into(), version: "0.0.0".into(), authors: vec!["MCSV".into()], description: "Perms lab baseline".into(), dependencies: vec![], permissions: vec![] }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        context.register_event_handler(OnCheck, EventPriority::Normal, true)?;
        Ok(())
    }
}

register_plugin!(Noop);
