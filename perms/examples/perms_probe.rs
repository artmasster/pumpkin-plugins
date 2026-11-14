//! Throwaway lab plugin (not shipped): reads what Perms publishes on a player the way Basics or
//! TabList would, and asks the server about nodes from another plugin's point of view.
//! `/cprobe <player> [node …]`
#![cfg(target_family = "wasm")]

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault};
use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let Some(p) = w.first().and_then(|n| server.get_player_by_name(n)) else {
            c::reply(&sender, "PROBE no such player online");
            return Ok(0);
        };
        // The exact calls a reader plugin makes
        let prefix = p.get_string("perms", "prefix");
        let suffix = p.get_string("perms", "suffix");
        let group = p.get_string("perms", "group");
        let weight = p.get_int("perms", "weight");
        // A value of our own that nobody clears: shows whether player custom data is saved at all
        if w.get(1).is_some_and(|a| a == "mark") {
            p.set_string("permsprobe", "mark", "kept");
        }
        let mark = p.get_string("permsprobe", "mark");
        c::reply(&sender, &format!("PROBE {} prefix={prefix:?} suffix={suffix:?} group={group:?} weight={weight:?} mark={mark:?}", p.get_name()));
        // Cost of one permission check as another plugin pays it
        if w.get(1).is_some_and(|a| a == "bench") {
            let started = std::time::Instant::now();
            let mut yes = 0u32;
            for i in 0..2000u32 {
                if p.has_permission(if i % 2 == 0 { "basics:command.fly" } else { "minecraft:command.gamemode" }) {
                    yes += 1;
                }
            }
            let micros = started.elapsed().as_micros();
            c::reply(&sender, &format!("PROBE bench 2000 checks in {micros} us = {:.1} us each ({yes} true)", micros as f64 / 2000.0));
            return Ok(1);
        }
        for node in w.iter().skip(1).filter(|n| *n != "mark") {
            c::reply(&sender, &format!("PROBE {} {node} = {}", p.get_name(), p.has_permission(node)));
        }
        Ok(1)
    }
}

struct Probe;

impl Plugin for Probe {
    fn new() -> Self {
        Probe
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "permsprobe".into(),
            version: "0.0.0".into(),
            authors: vec!["MCSV".into()],
            description: "Perms lab probe".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let reg = |node: &str, default: PermissionDefault| {
            let _ = context.register_permission(&Permission { node: node.into(), description: "probe".into(), default, children: vec![] });
        };
        reg("permsprobe:use", PermissionDefault::Allow);
        // Marker nodes like TabList's groups: nobody has them unless they are granted
        reg("permsprobe:group.admin", PermissionDefault::Deny);
        reg("permsprobe:marker", PermissionDefault::Deny);
        let cmd = Command::new(&["cprobe".to_owned()], "Perms lab probe")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, "permsprobe:use");
        Ok(())
    }
}

register_plugin!(Probe);
