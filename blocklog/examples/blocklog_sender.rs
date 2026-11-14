//! Throwaway lab plugin (not shipped): reports block changes to BlockLog the way WorldTools does, to
//! test BlockLog's receiving side and measure what a message costs.
//! `/chbsend real <n>`  set n blocks above the player to stone and report them (one message)
//! `/chbsend clear <n>` set the same n blocks to air and report that
//! `/chbsend load <n>`  report n changes without touching the world, 4096 a message; prints the cost
//! `/chbsend chat <name> <text>` send the chat notice a chat plugin would send
//! `/chbsend bad`       send a message BlockLog must refuse
//! `/chbsend sign <x> <y> <z> <text>` write on the sign there (the bot cannot edit signs)
#![cfg(target_family = "wasm")]

use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::wit::pumpkin::plugin::common::BlockPos;
use pumpkin_plugin_api::wit::pumpkin::plugin::world as host_world;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::BlockFlags;
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

type Change = (i32, i32, i32, u16, u16);

/// The `CHB1` message exactly as LAB.md specifies it.
fn message(user: &str, dimension: &str, changes: &[Change]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + user.len() + dimension.len() + changes.len() * 16);
    out.extend_from_slice(b"CHB1");
    out.extend_from_slice(&(user.len() as u16).to_le_bytes());
    out.extend_from_slice(user.as_bytes());
    out.extend_from_slice(&(dimension.len() as u16).to_le_bytes());
    out.extend_from_slice(dimension.as_bytes());
    out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
    for (x, y, z, old, new) in changes {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
        out.extend_from_slice(&z.to_le_bytes());
        out.extend_from_slice(&old.to_le_bytes());
        out.extend_from_slice(&new.to_le_bytes());
    }
    out
}

fn send(bytes: &[u8]) -> String {
    match pumpkin_plugin_api::ipc::send_ipc_message("blocklog", bytes) {
        Ok(Ok(answer)) => format!("answer {:?}", String::from_utf8_lossy(&answer)),
        Ok(Err(e)) => format!("refused: {e}"),
        Err(()) => "no such plugin".to_owned(),
    }
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let number = |i: usize, max: usize| w.get(i).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).clamp(1, max);
        let stone = host_world::get_block_by_name("stone").map_or(1, |b| b.default_state_id);
        let line = match (w.first().map(String::as_str), c::sender_player(&sender)) {
            (Some(sub @ ("real" | "clear")), Some(p)) => {
                let world = p.get_world();
                let (x, y, z) = p.get_position();
                let (x, y, z) = (x.floor() as i32, y.floor() as i32 + 3, z.floor() as i32);
                let want = if sub == "real" { stone } else { 0 };
                let flags = BlockFlags::NOTIFY_LISTENERS | BlockFlags::FORCE_STATE | BlockFlags::SKIP_DROPS;
                let mut changes = Vec::new();
                for i in 0..number(1, 4096) as i32 {
                    let at = (x + i % 16, y + i / 256, z + i / 16 % 16);
                    let pos = BlockPos { x: at.0, y: at.1, z: at.2 };
                    let old = world.get_block_state_id(pos);
                    if old != want {
                        world.set_block_state(pos, want, flags);
                        changes.push((at.0, at.1, at.2, old, want));
                    }
                }
                format!("SENDER {sub}: {} blocks from {x} {y} {z}, {}", changes.len(), send(&message(&p.get_name(), &world.get_dimension(), &changes)))
            }
            (Some("load"), _) => {
                let total = number(1, 1_000_000);
                let name = c::sender_player(&sender).map_or_else(|| "Console".to_owned(), |p| p.get_name());
                let (mut sent, mut messages, mut worst, mut answer) = (0usize, 0u32, 0u128, String::new());
                let began = Instant::now();
                while sent < total {
                    let n = (total - sent).min(4096);
                    // Spread over a few regions and heights, like a big edit
                    let changes: Vec<Change> = (sent..sent + n).map(|i| ((i % 700) as i32, 200 + (i / 490_000) as i32, (i / 700 % 700) as i32, 0, stone)).collect();
                    let bytes = message(&name, "minecraft:overworld", &changes);
                    let one = Instant::now();
                    answer = send(&bytes);
                    worst = worst.max(one.elapsed().as_micros());
                    sent += n;
                    messages += 1;
                }
                let micros = began.elapsed().as_micros();
                format!("SENDER load: {total} changes in {messages} messages, {micros} us in all, {:.0} us a message (worst {worst}), last {answer}", micros as f64 / f64::from(messages))
            }
            (Some("chat"), _) if w.len() >= 3 => {
                let notice = serde_json::json!({ "kind": "chat", "name": w[1], "message": w[2..].join(" ") });
                format!("SENDER chat: {}", send(notice.to_string().as_bytes()))
            }
            (Some("sign"), Some(p)) if w.len() >= 5 => {
                use pumpkin_plugin_api::block_entity::{BlockEntityType, DyeColor, SignText};
                let at = |i: usize| w[i].parse::<i32>().unwrap_or(0);
                match p.get_world().get_block_entity(BlockPos { x: at(1), y: at(2), z: at(3) }) {
                    Some(BlockEntityType::SignBlockEntity(sign)) => {
                        sign.set_front_text(&SignText { messages: vec![w[4..].join(" "), "second line".into(), String::new(), String::new()], color: DyeColor::Black, has_glowing_text: false });
                        "SENDER sign: written".to_owned()
                    }
                    _ => "SENDER sign: no sign there".to_owned(),
                }
            }
            (Some("bad"), _) => format!("SENDER bad: {} / {} / {}", send(b"CHB1\x05\x00Al"), send(b"hello"), send(br#"{"kind":"chat"}"#)),
            _ => "SENDER usage: real <n> | clear <n> | load <n> | chat <name> <text> | bad".to_owned(),
        };
        c::reply(&sender, &line);
        Ok(1)
    }
}

struct Sender;

impl Plugin for Sender {
    fn new() -> Self {
        Sender
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata { name: "blocklogsender".into(), version: "0.0.0".into(), authors: vec!["MCSV".into()], description: "BlockLog lab sender".into(), dependencies: vec![], permissions: vec![] }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let _ = context.register_permission(&Permission { node: "blocklogsender:use".into(), description: "lab".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let cmd = Command::new(&["chbsend".to_owned()], "BlockLog lab sender")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, "blocklogsender:use");
        Ok(())
    }
}

register_plugin!(Sender);
