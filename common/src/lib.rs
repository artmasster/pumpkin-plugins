//! Shared helpers for MCSV Pumpkin plugins: messages, senders, arguments,
//! config files and command suggestions.
#[cfg(feature = "http")]
pub mod http;
pub mod sha1;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pumpkin_plugin_api::command::{
    Arg, CommandSender, CommandSuggestion, CommandSuggestions, ConsumedArgs, SuggestionRequest,
};
use pumpkin_plugin_api::command_wit::CommandSenderType;
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;
use serde::Serialize;
use serde::de::DeserializeOwned;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Remember the plugin data folder (call once in `on_load`).
pub fn init_data_dir(path: String) {
    let _ = DATA_DIR.set(PathBuf::from(path));
}

/// Path inside the plugin data folder.
pub fn data_path(rel: &str) -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(|| PathBuf::from(".")).join(rel)
}

/// Text with `&` color codes (`&a`, `&l`, `&#rrggbb` is not supported by Pumpkin legacy parsing).
pub fn color(text: &str) -> TextComponent {
    TextComponent::from_legacy_string_with_code(text, '&')
}

/// Send a colored message to any command sender.
pub fn reply(sender: &CommandSender, text: &str) {
    sender.send_message(color(text));
}

/// Send a colored chat message to a player.
pub fn tell(player: &Player, text: &str) {
    player.send_system_message(color(text), false);
}

/// Send a colored action-bar message to a player.
pub fn action_bar(player: &Player, text: &str) {
    player.send_system_message(color(text), true);
}

/// The player behind a sender, if any.
pub fn sender_player(sender: &CommandSender) -> Option<Player> {
    match sender.get_command_sender_type() {
        CommandSenderType::Player(p) => Some(p),
        _ => None,
    }
}

/// Stable key for per-sender state ("console" for non-players).
pub fn sender_key(sender: &CommandSender) -> String {
    match sender_player(sender) {
        Some(p) => p.get_name(),
        None => "console".into(),
    }
}

/// Console, RCON and command blocks pass; players need the node (ops get defaults).
pub fn has_perm(sender: &CommandSender, node: &str) -> bool {
    match sender_player(sender) {
        Some(p) => p.has_permission(node),
        None => true,
    }
}

/// Split the greedy `args` argument of a command into words.
pub fn words(args: &ConsumedArgs, key: &str) -> Vec<String> {
    match args.get_value(key) {
        Arg::Simple(s) | Arg::Msg(s) => s.split_whitespace().map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// Suggest values for the last word of a greedy argument.
pub fn suggest_last(request: &SuggestionRequest, candidates: &[String]) -> CommandSuggestions {
    let remaining = request.remaining.as_str();
    let last_start = remaining.rfind(' ').map_or(0, |i| i + 1);
    let last = &remaining[last_start..];
    let lower = last.to_lowercase();
    let values = candidates
        .iter()
        .filter(|c| c.to_lowercase().starts_with(&lower))
        .map(|c| CommandSuggestion { value: c.clone(), tooltip: None })
        .collect();
    CommandSuggestions {
        start: request.start + last_start as u32,
        length: last.len() as u32,
        values,
    }
}

/// Number of whole words already typed before the word being completed.
pub fn typed_words(request: &SuggestionRequest) -> Vec<String> {
    let remaining = request.remaining.as_str();
    match remaining.rfind(' ') {
        Some(i) => remaining[..i].split_whitespace().map(str::to_owned).collect(),
        None => Vec::new(),
    }
}

/// Load a TOML config from the data folder, writing the defaults when it is missing.
/// Unknown keys are ignored and missing keys take their default, so old files keep working;
/// a file that lacks options of this version is rewritten with them (comments are not kept).
pub fn load_config<T: Serialize + DeserializeOwned + Default>(rel: &str) -> T {
    let path = data_path(rel);
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<T>(&text) {
            Ok(cfg) => {
                // A newer plugin version has more options: add them to the file so owners can see them
                if let (Ok(full), Ok(have)) = (toml::to_string_pretty(&cfg), text.parse::<toml::Table>()) {
                    let known = full.parse::<toml::Table>().unwrap_or_default();
                    if known.keys().any(|k| !have.contains_key(k)) {
                        let _ = write_atomic(&path, full.as_bytes());
                    }
                }
                cfg
            }
            Err(e) => {
                tracing::warn!("{rel}: {e}; using defaults");
                T::default()
            }
        },
        Err(_) => {
            let cfg = T::default();
            if let Ok(text) = toml::to_string_pretty(&cfg) {
                let _ = write_atomic(&path, text.as_bytes());
            }
            cfg
        }
    }
}

/// Write a TOML config to the data folder (after a command changed it).
pub fn save_config<T: Serialize>(rel: &str, cfg: &T) -> bool {
    match toml::to_string_pretty(cfg) {
        Ok(text) => write_atomic(&data_path(rel), text.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// One plain line in the server console (`[plugin] <message>`, without tracing's source fields).
pub fn log_info(message: &str) {
    pumpkin_plugin_api::logging::log(pumpkin_plugin_api::logging::LogLevel::Info, message);
}

/// Strip `&x` color codes for console logs.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '&' && it.peek().is_some_and(|n| n.is_ascii_alphanumeric()) {
            it.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Read JSON state from the data folder.
pub fn load_json<T: DeserializeOwned>(rel: &str) -> Option<T> {
    let text = std::fs::read_to_string(data_path(rel)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write JSON state to the data folder atomically.
pub fn save_json<T: Serialize>(rel: &str, value: &T) -> bool {
    match serde_json::to_vec_pretty(value) {
        Ok(bytes) => write_atomic(&data_path(rel), &bytes).is_ok(),
        Err(_) => false,
    }
}

/// Remove a state file from the data folder.
pub fn remove_file(rel: &str) {
    let _ = std::fs::remove_file(data_path(rel));
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where the Login plugin marks a player it is still waiting on: custom data `login` /
/// `pending_until` (epoch seconds). A time, not a flag, so a stale mark ends by itself.
pub const LOGIN_NS: &str = "login";
pub const LOGIN_KEY: &str = "pending_until";

/// True while Login (login for offline-mode servers) is still waiting for this player's password.
/// Plugins skip chat, logging and scripts for such a player: Pumpkin runs handlers in plugin load
/// order, so a handler can run before Login's own handler has cancelled the event.
pub fn awaiting_login(player: &Player) -> bool {
    use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
    player.get_long(LOGIN_NS, LOGIN_KEY).is_some_and(|until| until > now_secs() as i64)
}

/// Prefix and suffix the Perms plugin (permissions) gives this player, when it has any.
/// `None` when Perms is not installed or has nothing for the player: use your own.
/// Read it when you need it, not once at join: Perms's join handler may run after yours.
pub fn perms_decor(player: &Player) -> Option<(String, String)> {
    use pumpkin_plugin_api::persistent_data::PersistentDataHolder;
    let prefix = player.get_string("perms", "prefix")?;
    let suffix = player.get_string("perms", "suffix").unwrap_or_default();
    (!prefix.is_empty() || !suffix.is_empty()).then_some((prefix, suffix))
}

/// Send a JSON message to another plugin and get its JSON answer.
/// `None` when that plugin is not installed, refused, or answered something unreadable.
/// The call is synchronous: the other plugin's handler runs before this returns.
pub fn ipc(recipient: &str, message: &serde_json::Value) -> Option<serde_json::Value> {
    let bytes = serde_json::to_vec(message).ok()?;
    match pumpkin_plugin_api::ipc::send_ipc_message(recipient, &bytes) {
        Ok(Ok(answer)) => serde_json::from_slice(&answer).ok(),
        _ => None,
    }
}

/// Fill `%placeholders%` (`%player_name%`, `%basics_balance%`, …) through the Placeholders plugin.
/// `player` is the name of the player the text is for, if any. The texts come back as they are
/// when none has a `%`, when Placeholders is not installed, or when it is busy.
pub fn placeholders_many(texts: &[String], player: Option<&str>) -> Vec<String> {
    if !texts.iter().any(|t| t.contains('%')) {
        return texts.to_vec();
    }
    let answer = ipc("placeholders", &serde_json::json!({ "kind": "parse_many", "texts": texts, "player": player }));
    let filled: Option<Vec<String>> = answer.and_then(|v| v.get("texts")?.as_array().map(|a| a.iter().map(|t| t.as_str().unwrap_or("").to_owned()).collect()));
    match filled {
        Some(out) if out.len() == texts.len() => out,
        _ => texts.to_vec(),
    }
}

/// One text through [`placeholders_many`].
pub fn placeholders(text: &str, player: Option<&str>) -> String {
    placeholders_many(&[text.to_owned()], player).pop().unwrap_or_else(|| text.to_owned())
}

/// `h:mm:ss`
pub fn hms(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}
