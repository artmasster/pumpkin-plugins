//! What goes to Discord and what comes back, without any network or server types.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

pub const DEFAULT_API: &str = "https://discord.com/api/v10";
/// Discord's limit for one message.
const MAX_CONTENT: usize = 2000;
/// Lines waiting to go out; older ones are dropped beyond this.
const MAX_QUEUE: usize = 100;
/// Longest Discord message shown in game.
const MAX_INGAME: usize = 300;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Bot token: needed to read the channel (Discord to game) and to answer `!list`.
    pub bot_token: String,
    /// Channel the bridge reads and, without a webhook, writes.
    pub channel_id: String,
    /// Webhook of that channel: game chat then shows each player's name and head.
    pub webhook_url: String,
    /// Shown by `/discord`.
    pub invite: String,
    /// Seconds between looks at the channel.
    pub poll_seconds: u64,
    pub chat_to_discord: bool,
    pub chat_to_game: bool,
    pub join_leave: bool,
    pub deaths: bool,
    pub start_stop: bool,
    /// `!list` in the channel answers with who is online.
    pub list_command: bool,
    /// `{name}` and `{message}`.
    pub format_to_game: String,
    /// Bot mode only (a webhook shows the name by itself). `{name}` and `{message}`.
    pub format_to_discord: String,
    /// Player head for webhook messages. `{name}` is the player name.
    pub avatar_url: String,
    /// A second channel where the listed people type console commands (needs the bot token).
    /// The server cannot hand command output back, so the bot only confirms that it ran.
    pub command_channel_id: String,
    /// Discord user ids (numbers) allowed to run commands there. Empty = nobody.
    pub command_user_ids: Vec<String>,
    /// Channel topic, rewritten every `topic-minutes` (needs the bot to have Manage Channel).
    /// `{online}`, `{max}`, `{tps}`. Empty = leave the topic alone.
    pub topic: String,
    pub topic_minutes: u64,
    /// Leave alone. Another address is for testing against a stand-in server.
    pub api_base: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "th".into(),
            bot_token: String::new(),
            channel_id: String::new(),
            webhook_url: String::new(),
            invite: String::new(),
            poll_seconds: 3,
            chat_to_discord: true,
            chat_to_game: true,
            join_leave: true,
            deaths: true,
            start_stop: true,
            list_command: true,
            format_to_game: "&9[Discord] &f{name}&7: &f{message}".into(),
            format_to_discord: "**{name}**: {message}".into(),
            avatar_url: "https://mc-heads.net/avatar/{name}/64".into(),
            command_channel_id: String::new(),
            command_user_ids: Vec::new(),
            topic: String::new(),
            topic_minutes: 10,
            api_base: DEFAULT_API.into(),
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        self.bot_token = self.bot_token.trim().trim_start_matches("Bot ").to_owned();
        self.channel_id = self.channel_id.trim().to_owned();
        self.webhook_url = self.webhook_url.trim().to_owned();
        self.api_base = self.api_base.trim().trim_end_matches('/').to_owned();
        if self.api_base.is_empty() {
            self.api_base = DEFAULT_API.into();
        }
        self.poll_seconds = self.poll_seconds.clamp(2, 60);
        self.command_channel_id = self.command_channel_id.trim().to_owned();
        self.command_user_ids = self.command_user_ids.iter().map(|u| u.trim().to_owned()).filter(|u| !u.is_empty() && u.bytes().all(|b| b.is_ascii_digit())).collect();
        // Discord lets a channel be renamed or re-topiced twice in ten minutes
        self.topic_minutes = self.topic_minutes.clamp(6, 1440);
        self
    }

    pub fn thai(&self) -> bool {
        self.language != "en"
    }

    /// What is wrong with the settings, as message keys. Empty = usable.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let real = self.api_base == DEFAULT_API;
        if !self.channel_id.is_empty() && !self.channel_id.bytes().all(|b| b.is_ascii_digit()) {
            out.push("bad-channel");
        }
        if !self.webhook_url.is_empty() {
            let ok = ["https://discord.com/api/webhooks/", "https://discordapp.com/api/webhooks/", "https://ptb.discord.com/api/webhooks/", "https://canary.discord.com/api/webhooks/"]
                .iter()
                .any(|p| self.webhook_url.starts_with(p));
            if real && !ok {
                out.push("bad-webhook");
            }
        }
        // Reading the channel takes the token and the channel id together
        if self.bot_token.is_empty() != self.channel_id.is_empty() {
            out.push("need-both");
        }
        if !self.can_send() && !self.can_read() {
            out.push("nothing-set");
        }
        out
    }

    /// Bot token and channel: we can read the channel and post as the bot.
    pub fn can_read(&self) -> bool {
        !self.bot_token.is_empty() && !self.channel_id.is_empty() && self.channel_id.bytes().all(|b| b.is_ascii_digit())
    }

    /// Console commands from Discord: a token, a channel of its own and at least one allowed person.
    pub fn can_command(&self) -> bool {
        !self.bot_token.is_empty()
            && !self.command_channel_id.is_empty()
            && self.command_channel_id.bytes().all(|b| b.is_ascii_digit())
            && self.command_channel_id != self.channel_id
            && !self.command_user_ids.is_empty()
    }

    pub fn can_send(&self) -> bool {
        !self.webhook_url.is_empty() || self.can_read()
    }
}

/// `&a`-style codes and `§` codes out; what is left is what the player typed.
pub fn strip_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        if (ch == '&' || ch == '§') && it.peek().is_some_and(|n| n.is_ascii_alphanumeric()) {
            it.next();
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

/// Game text for Discord: no colour codes, and Markdown that would change how the line looks is escaped.
pub fn to_discord(text: &str) -> String {
    let plain = strip_codes(text);
    let mut out = String::with_capacity(plain.len() + 8);
    for ch in plain.chars() {
        if matches!(ch, '*' | '_' | '~' | '`' | '|' | '>' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Discord text for the game: one line, no colour codes a stranger could use to fake a prefix,
/// mentions and custom emoji as readable words.
pub fn to_game(content: &str, mentions: &[(String, String)]) -> String {
    let mut text = content.to_owned();
    for (id, name) in mentions {
        text = text.replace(&format!("<@{id}>"), &format!("@{name}")).replace(&format!("<@!{id}>"), &format!("@{name}"));
    }
    // <:name:id> and <a:name:id> → :name:
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let emoji = tail.find('>').and_then(|end| {
            let inner = &tail[1..end];
            let inner = inner.strip_prefix("a:").or_else(|| inner.strip_prefix(':'))?;
            let (name, id) = inner.split_once(':')?;
            (!name.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| (format!(":{name}:"), end + 1))
        });
        match emoji {
            Some((word, len)) => {
                out.push_str(&word);
                rest = &tail[len..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    let one_line: String = out.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    let clean = strip_codes(&one_line);
    let clean = clean.trim();
    if clean.chars().count() > MAX_INGAME {
        let cut: String = clean.chars().take(MAX_INGAME).collect();
        format!("{cut}…")
    } else {
        clean.to_owned()
    }
}

/// A message read from the channel.
#[derive(Debug, PartialEq)]
pub struct Incoming {
    pub id: u64,
    pub author: String,
    /// Discord's id of the author: the only thing that says who really wrote it.
    pub author_id: String,
    pub text: String,
    /// Sent by a bot or a webhook (ours included): never shown in game.
    pub automated: bool,
    pub attachments: usize,
}

#[derive(Deserialize)]
struct RawAuthor {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
    #[serde(default)]
    bot: bool,
}

#[derive(Deserialize)]
struct RawMessage {
    id: String,
    #[serde(default)]
    content: String,
    author: RawAuthor,
    #[serde(default)]
    webhook_id: Option<String>,
    #[serde(default)]
    mentions: Vec<RawAuthor>,
    #[serde(default)]
    attachments: Vec<serde_json::Value>,
}

/// The channel's answer (newest first) as messages oldest first. Anything unreadable is skipped.
pub fn parse_messages(json: &[u8]) -> Vec<Incoming> {
    let raw: Vec<serde_json::Value> = serde_json::from_slice(json).unwrap_or_default();
    let mut out: Vec<Incoming> = raw
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawMessage>(v).ok())
        .filter_map(|m| {
            let mentions: Vec<(String, String)> = m.mentions.iter().map(|u| (u.id.clone(), u.global_name.clone().unwrap_or_else(|| u.username.clone()))).collect();
            Some(Incoming {
                id: m.id.parse().ok()?,
                author_id: m.author.id.clone(),
                author: strip_codes(&m.author.global_name.unwrap_or(m.author.username)).chars().take(32).collect(),
                text: to_game(&m.content, &mentions),
                automated: m.author.bot || m.webhook_id.is_some(),
                attachments: m.attachments.len(),
            })
        })
        .collect();
    out.sort_by_key(|m| m.id);
    out
}

/// A console command typed in the command channel: one line, no leading slash, no control
/// characters. `None` for anything that is not plainly a command.
pub fn console_command(text: &str) -> Option<String> {
    let line = text.trim().trim_start_matches('/').trim();
    (!line.is_empty() && line.chars().count() <= 256 && !line.chars().any(char::is_control)).then(|| line.to_owned())
}

/// Seconds Discord asked us to wait (`{"retry_after": 1.5}`), rounded up, at least one.
pub fn retry_after(body: &[u8]) -> u64 {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("retry_after").and_then(serde_json::Value::as_f64))
        .map_or(5, |s| (s.ceil() as u64).clamp(1, 600))
}

/// One line on its way to Discord.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Player name for chat (shown as the webhook's name); empty for server notices.
    pub name: String,
    pub text: String,
}

/// What to send next.
#[derive(Debug, PartialEq)]
pub struct Outgoing {
    pub content: String,
    /// Webhook display name, when the whole message is one player's chat.
    pub username: Option<String>,
    /// Lines of the queue this message carries.
    pub lines: usize,
}

#[derive(Default)]
pub struct Outbox {
    queue: VecDeque<Line>,
    pub dropped: u64,
}

impl Outbox {
    pub fn push(&mut self, line: Line) {
        if self.queue.len() >= MAX_QUEUE {
            self.queue.pop_front();
            self.dropped += 1;
        }
        self.queue.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The next message, without taking it off the queue (`sent` does that once Discord took it).
    /// A short queue goes out line by line so each player keeps their own name and head;
    /// a backlog is packed into as few messages as possible.
    pub fn next(&self, webhook: bool, format: &str) -> Option<Outgoing> {
        let first = self.queue.front()?;
        let render = |l: &Line| {
            if l.name.is_empty() {
                l.text.clone()
            } else {
                format.replace("{name}", &to_discord(&l.name)).replace("{message}", &l.text)
            }
        };
        if webhook && self.queue.len() <= 3 && !first.name.is_empty() {
            return Some(Outgoing { content: clip(&first.text), username: Some(first.name.clone()), lines: 1 });
        }
        if self.queue.len() == 1 || (webhook && self.queue.len() <= 3) {
            return Some(Outgoing { content: clip(&render(first)), username: None, lines: 1 });
        }
        let mut content = String::new();
        let mut lines = 0;
        for l in &self.queue {
            let piece = render(l);
            if lines > 0 && content.chars().count() + piece.chars().count() + 1 > MAX_CONTENT {
                break;
            }
            if lines > 0 {
                content.push('\n');
            }
            content.push_str(&piece);
            lines += 1;
        }
        Some(Outgoing { content: clip(&content), username: None, lines })
    }

    /// Discord accepted (or finally refused) the message made of the first `lines` lines.
    pub fn sent(&mut self, lines: usize) {
        for _ in 0..lines.min(self.queue.len()) {
            self.queue.pop_front();
        }
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_CONTENT {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX_CONTENT - 1).collect();
    format!("{cut}…")
}

/// JSON body of a message. `allowed_mentions` is empty: nothing a player types can ping anyone.
pub fn payload(out: &Outgoing, avatar_url: &str) -> String {
    let mut v = serde_json::json!({ "content": out.content, "allowed_mentions": { "parse": [] } });
    if let Some(name) = &out.username {
        // Discord refuses webhook names containing "discord" or "clyde"
        let safe: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(32).collect();
        let lower = safe.to_lowercase();
        if !safe.is_empty() && !lower.contains("discord") && !lower.contains("clyde") {
            v["username"] = serde_json::Value::String(safe.clone());
            if !avatar_url.is_empty() {
                v["avatar_url"] = serde_json::Value::String(avatar_url.replace("{name}", &safe));
            }
        } else {
            v["content"] = serde_json::Value::String(clip(&format!("**{}**: {}", to_discord(name), out.content)));
        }
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_text_is_safe_on_discord() {
        assert_eq!(to_discord("&ahello &lworld"), "hello world");
        assert_eq!(to_discord("**bold** _x_ `c` ||s|| > q"), "\\*\\*bold\\*\\* \\_x\\_ \\`c\\` \\|\\|s\\|\\| \\> q");
        assert_eq!(to_discord("@everyone ไทย"), "@everyone ไทย");
        let out = Outgoing { content: "@everyone hi".into(), username: Some("Steve".into()), lines: 1 };
        let v: serde_json::Value = serde_json::from_str(&payload(&out, "https://h/{name}")).unwrap();
        assert_eq!(v["allowed_mentions"]["parse"].as_array().map(Vec::len), Some(0));
        assert_eq!(v["username"], "Steve");
        assert_eq!(v["avatar_url"], "https://h/Steve");
        // Names Discord refuses, or odd ones, go into the text instead
        let out = Outgoing { content: "hi".into(), username: Some("Discord_Fan".into()), lines: 1 };
        let v: serde_json::Value = serde_json::from_str(&payload(&out, "")).unwrap();
        assert!(v.get("username").is_none());
        assert_eq!(v["content"], "**Discord\\_Fan**: hi");
    }

    #[test]
    fn discord_text_is_safe_in_game() {
        let m = [("42".to_owned(), "Bob".to_owned())];
        assert_eq!(to_game("hi <@42> and <@!42>", &m), "hi @Bob and @Bob");
        assert_eq!(to_game("&c[Admin] &ffake §4x", &[]), "[Admin] fake x");
        assert_eq!(to_game("line1\nline2\ttab", &[]), "line1 line2 tab");
        assert_eq!(to_game("nice <:pog:123456> <a:dance:99> <3 <b>", &[]), "nice :pog: :dance: <3 <b>");
        assert_eq!(to_game(&"ก".repeat(400), &[]).chars().count(), MAX_INGAME + 1);
    }

    #[test]
    fn channel_answer_is_parsed_oldest_first() {
        let json = br#"[
          {"id":"30","content":"third","author":{"id":"1","username":"amy","global_name":"Amy"},"mentions":[],"attachments":[{"id":"9"}]},
          {"id":"20","content":"from hook","author":{"id":"2","username":"Steve","bot":true},"webhook_id":"77"},
          {"id":"10","content":"first","author":{"id":"3","username":"&4red"}},
          {"id":"nope","content":"x","author":{"username":"y"}},
          {"broken":true}
        ]"#;
        let got = parse_messages(json);
        assert_eq!(got.iter().map(|m| m.id).collect::<Vec<_>>(), vec![10, 20, 30]);
        assert_eq!(got[0].author, "red");
        assert!(got[1].automated && !got[2].automated);
        assert_eq!((got[2].author.as_str(), got[2].text.as_str(), got[2].attachments), ("Amy", "third", 1));
        assert!(parse_messages(b"{\"message\":\"401: Unauthorized\"}").is_empty());
        assert!(parse_messages(b"not json").is_empty());
        assert_eq!(retry_after(br#"{"message":"rate limited","retry_after":1.234,"global":false}"#), 2);
        assert_eq!(retry_after(b"oops"), 5);
        assert_eq!((got[2].author_id.as_str(), got[0].author_id.as_str()), ("1", "3"));
        assert_eq!(console_command(" /say hi there "), Some("say hi there".into()));
        assert_eq!(console_command("//"), None);
        assert_eq!(console_command(&"x".repeat(300)), None);
        let c = Config { bot_token: "t".into(), channel_id: "1".into(), command_channel_id: "2".into(), command_user_ids: vec![" 42 ".into(), "bob".into()], topic_minutes: 1, ..Config::default() }.cleaned();
        assert!(c.can_command());
        assert_eq!((c.command_user_ids.clone(), c.topic_minutes), (vec!["42".to_owned()], 6));
        // Never the chat channel itself, and never without a list of people
        assert!(!Config { command_channel_id: "1".into(), ..c.clone() }.can_command());
        assert!(!Config { command_user_ids: vec![], ..c }.can_command());
    }

    #[test]
    fn outbox_keeps_names_when_short_and_packs_a_backlog() {
        let mut o = Outbox::default();
        let chat = |n: &str, t: &str| Line { name: n.into(), text: t.into() };
        o.push(chat("Amy", "one"));
        o.push(Line { name: String::new(), text: "Bob joined".into() });
        // Webhook, short queue: the player's own name
        assert_eq!(o.next(true, "**{name}**: {message}"), Some(Outgoing { content: "one".into(), username: Some("Amy".into()), lines: 1 }));
        // Bot: the format carries the name
        assert_eq!(o.next(false, "**{name}**: {message}").unwrap().content, "**Amy**: one\nBob joined");
        o.sent(1);
        assert_eq!(o.next(true, "x").unwrap(), Outgoing { content: "Bob joined".into(), username: None, lines: 1 });
        o.sent(1);
        assert!(o.next(true, "x").is_none());
        // A backlog is packed, and never beyond Discord's limit
        for i in 0..50 {
            o.push(chat("Amy", &format!("{i} {}", "x".repeat(100))));
        }
        let packed = o.next(true, "**{name}**: {message}").unwrap();
        assert!(packed.lines > 10 && packed.lines < 50 && packed.content.chars().count() <= MAX_CONTENT, "{}", packed.lines);
        assert!(packed.username.is_none());
        o.sent(packed.lines);
        assert_eq!(o.len(), 50 - packed.lines);
        // The queue is bounded
        for i in 0..500 {
            o.push(chat("Amy", &i.to_string()));
        }
        assert_eq!(o.len(), MAX_QUEUE);
        assert!(o.dropped > 0);
        // One giant line is clipped
        let mut o = Outbox::default();
        o.push(chat("Amy", &"y".repeat(5000)));
        assert_eq!(o.next(false, "{message}").unwrap().content.chars().count(), MAX_CONTENT);
    }

    #[test]
    fn config_checks() {
        let c = Config::default().cleaned();
        assert_eq!(c.problems(), vec!["nothing-set"]);
        let c = Config { bot_token: " Bot abc ".into(), channel_id: "123".into(), ..Config::default() }.cleaned();
        assert_eq!(c.bot_token, "abc");
        assert!(c.can_read() && c.can_send() && c.problems().is_empty());
        let c = Config { webhook_url: "https://discord.com/api/webhooks/1/x".into(), ..Config::default() }.cleaned();
        assert!(c.can_send() && !c.can_read() && c.problems().is_empty());
        let c = Config { webhook_url: "https://evil.example/hook".into(), channel_id: "12a".into(), ..Config::default() }.cleaned();
        assert_eq!(c.problems(), vec!["bad-channel", "bad-webhook", "need-both"]);
        // A stand-in server for tests may be plain http
        let c = Config { webhook_url: "http://127.0.0.1:1/hook".into(), api_base: "http://127.0.0.1:1/".into(), ..Config::default() }.cleaned();
        assert!(c.problems().is_empty());
        assert_eq!(c.api_base, "http://127.0.0.1:1");
        let c = Config { bot_token: "abc".into(), ..Config::default() }.cleaned();
        assert_eq!(c.problems(), vec!["need-both", "nothing-set"]);
        assert_eq!(Config { poll_seconds: 0, ..Config::default() }.cleaned().poll_seconds, 2);
    }
}
