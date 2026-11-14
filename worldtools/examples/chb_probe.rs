//! Throwaway lab plugin (not shipped): stands in for BlockLog to check what WorldTools sends.
//! It takes the plugin id `blocklog`, reads every `CHB1` message exactly as the format says,
//! logs what it saw and answers `ok`. `/chbprobe total` prints the running totals,
//! `/chbprobe quiet` stops the line per message (to measure the bare cost of the call).
#![cfg(target_family = "wasm")]

use std::sync::Mutex;

use mcsv_common as c;
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::permission::{Permission, PermissionDefault};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin};

#[derive(Default)]
struct Seen {
    messages: u64,
    blocks: u64,
    bytes: u64,
    bad: u64,
    quiet: bool,
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

struct Change {
    at: (i32, i32, i32),
    old: u16,
    new: u16,
}

/// `CHB1`, u16 + name, u16 + dimension, u32 count, count x (i32 x, y, z, u16 old, u16 new).
fn parse(m: &[u8]) -> Result<(String, String, Vec<Change>), &'static str> {
    fn take<'a>(m: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8], &'static str> {
        let s = m.get(*at..*at + n).ok_or("too short")?;
        *at += n;
        Ok(s)
    }
    fn text(m: &[u8], at: &mut usize) -> Result<String, &'static str> {
        let len = take(m, at, 2)?;
        let len = usize::from(u16::from_le_bytes([len[0], len[1]]));
        String::from_utf8(take(m, at, len)?.to_vec()).map_err(|_| "not UTF-8")
    }
    let mut at = 0usize;
    if take(m, &mut at, 4)? != b"CHB1" {
        return Err("not CHB1");
    }
    let name = text(m, &mut at)?;
    let dim = text(m, &mut at)?;
    let n = take(m, &mut at, 4)?;
    let n = u32::from_le_bytes([n[0], n[1], n[2], n[3]]) as usize;
    if n > 4096 {
        return Err("more than 4096 changes");
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let b = take(m, &mut at, 16)?;
        let i = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        out.push(Change { at: (i(0), i(4), i(8)), old: u16::from_le_bytes([b[12], b[13]]), new: u16::from_le_bytes([b[14], b[15]]) });
    }
    if at != m.len() {
        return Err("bytes left over");
    }
    Ok((name, dim, out))
}

struct Cmd;

impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, _server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let mut guard = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let seen = guard.get_or_insert_with(Seen::default);
        match w.first().map(String::as_str) {
            Some("quiet") => seen.quiet = true,
            Some("loud") => seen.quiet = false,
            Some("reset") => *seen = Seen { quiet: seen.quiet, ..Seen::default() },
            _ => {}
        }
        c::reply(&sender, &format!("CHB total messages={} blocks={} bytes={} bad={} quiet={}", seen.messages, seen.blocks, seen.bytes, seen.bad, seen.quiet));
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
            // The id WorldTools sends to
            name: "blocklog".into(),
            version: "0.0.0".into(),
            authors: vec!["MCSV".into()],
            description: "WorldTools lab probe standing in for BlockLog".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let _ = context.register_permission(&Permission { node: "blocklog:probe".into(), description: "probe".into(), default: PermissionDefault::Allow, children: vec![] });
        let cmd = Command::new(&["chbprobe".to_owned()], "WorldTools lab probe")
            .execute(Cmd)
            .then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, "blocklog:probe");
        Ok(())
    }

    fn handle_ipc_message(&self, sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut guard = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let seen = guard.get_or_insert_with(Seen::default);
        match parse(&message) {
            Ok((name, dim, changes)) => {
                seen.messages += 1;
                seen.blocks += changes.len() as u64;
                seen.bytes += message.len() as u64;
                if !seen.quiet {
                    let show = |ch: Option<&Change>| ch.map_or_else(|| "-".to_owned(), |ch| format!("{},{},{} {}->{}", ch.at.0, ch.at.1, ch.at.2, ch.old, ch.new));
                    let same = changes.iter().filter(|ch| ch.old == ch.new).count();
                    c::log_info(&format!(
                        "CHB #{} from={sender} player={name} dim={dim} count={} bytes={} first=[{}] last=[{}] unchanged={same} total_blocks={}",
                        seen.messages,
                        changes.len(),
                        message.len(),
                        show(changes.first()),
                        show(changes.last()),
                        seen.blocks
                    ));
                }
                Ok(b"ok".to_vec())
            }
            Err(e) => {
                seen.bad += 1;
                c::log_info(&format!("CHB BAD from={sender} bytes={}: {e}", message.len()));
                Err(e.to_owned())
            }
        }
    }
}

register_plugin!(Probe);
