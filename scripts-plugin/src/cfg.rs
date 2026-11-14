//! Settings, the sentences Scripts says (Thai and English) and the example scripts.
use serde::{Deserialize, Serialize};

use crate::engine::{Limits, Messages};
use crate::parse::ScriptError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// th | en
    pub language: String,
    /// Hours added to UTC when a script prints a date (`now`).
    pub timezone_offset_hours: f64,
    /// Seconds between saves of variables.json (10..=3600); it is also saved on reload and unload.
    pub save_interval_seconds: u64,
    /// Instructions one trigger may run without a `wait` before it is stopped (endless loop guard).
    pub max_instructions_per_run: u64,
    /// Instructions all scripts together may run in one server tick.
    pub max_instructions_per_tick: u64,
    /// Global variables kept at most.
    pub max_variables: usize,
    /// Entries one list (or one loop over a list) may have.
    pub max_list_size: usize,
    /// Triggers that may be waiting (`wait 5 seconds`) at the same time.
    pub max_pending_waits: usize,
    /// Largest script file that is loaded, in bytes.
    pub max_script_bytes: u64,
    /// Let scripts run console commands (`execute console command "..."`).
    pub allow_console_commands: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "th".into(),
            timezone_offset_hours: 7.0,
            save_interval_seconds: 60,
            max_instructions_per_run: 50_000,
            max_instructions_per_tick: 200_000,
            max_variables: 100_000,
            max_list_size: 10_000,
            max_pending_waits: 2000,
            max_script_bytes: 262_144,
            allow_console_commands: true,
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Self {
        if self.language != "en" {
            self.language = "th".into();
        }
        self.timezone_offset_hours = if self.timezone_offset_hours.is_finite() { self.timezone_offset_hours.clamp(-12.0, 14.0) } else { 7.0 };
        self.save_interval_seconds = self.save_interval_seconds.clamp(10, 3600);
        self.max_instructions_per_run = self.max_instructions_per_run.clamp(1000, 5_000_000);
        self.max_instructions_per_tick = self.max_instructions_per_tick.clamp(self.max_instructions_per_run, 20_000_000);
        self.max_variables = self.max_variables.clamp(100, 2_000_000);
        self.max_list_size = self.max_list_size.clamp(10, 200_000);
        self.max_pending_waits = self.max_pending_waits.clamp(10, 100_000);
        self.max_script_bytes = self.max_script_bytes.clamp(1024, 4 * 1024 * 1024);
        self
    }

    pub fn th(&self) -> bool {
        self.language != "en"
    }

    pub fn limits(&self) -> Limits {
        Limits {
            run_ops: self.max_instructions_per_run,
            tick_ops: self.max_instructions_per_tick,
            max_variables: self.max_variables,
            max_list: self.max_list_size,
            max_waits: self.max_pending_waits,
            ..Limits::default()
        }
    }

    pub fn messages(&self) -> Messages {
        let t = |key: &str| tr(self.th(), key).to_owned();
        Messages {
            no_permission: t("no_permission"),
            cooldown: t("cooldown"),
            players_only: t("players_only"),
            console_only: t("console_only"),
            usage: t("usage"),
            no_player: t("no_player"),
            limit: t("limit"),
            tick_limit: t("tick_limit"),
            wait_in_value: t("wait_in_value"),
            too_many_waits: t("too_many_waits"),
            too_many_variables: t("too_many_variables"),
            recursion: t("recursion"),
        }
    }
}

pub const PREFIX: &str = "&8[&bScripts&8]&r ";

/// (key, Thai, English)
const TEXTS: &[(&str, &str, &str)] = &[
    ("no_permission", "&cคุณไม่มีสิทธิ์ใช้คำสั่งนี้", "&cYou don't have permission to use this command."),
    ("cooldown", "&cต้องรออีก {time} ก่อนใช้คำสั่งนี้อีกครั้ง", "&cYou have to wait {time} before using this command again."),
    ("players_only", "&cคำสั่งนี้ใช้ได้เฉพาะผู้เล่น", "&cOnly players can use this command."),
    ("console_only", "&cคำสั่งนี้ใช้ได้เฉพาะคอนโซล", "&cOnly the console can use this command."),
    ("usage", "&cวิธีใช้: {usage}", "&cUsage: {usage}"),
    ("no_player", "&cไม่พบผู้เล่น {name}", "&cPlayer not found: {name}"),
    ("limit", "หยุดทำงานหลังรัน {n} คำสั่งโดยไม่มี wait (น่าจะวนไม่จบ)", "stopped after {n} instructions without a wait (endless loop?)"),
    ("tick_limit", "ข้ามไป: สคริปต์ใช้โควตาของ tick นี้หมดแล้ว", "skipped: the scripts used up this tick's budget"),
    ("wait_in_value", "wait ในฟังก์ชันที่ถูกใช้เป็นค่าจะไม่ทำงาน", "`wait` inside a function that is used as a value is ignored"),
    ("too_many_waits", "มี trigger ที่รอ wait มากเกินไป ตัวนี้ถูกยกเลิก", "too many waiting triggers; this one was dropped"),
    ("too_many_variables", "ตัวแปรเต็มแล้ว ค่านี้ไม่ถูกเก็บ", "variable limit reached; the value was not stored"),
    ("recursion", "ฟังก์ชันเรียกกันลึกเกินไป", "functions call each other too deeply"),
    ("console_off", "config ปิดการรันคำสั่งคอนโซลจากสคริปต์ไว้", "console commands from scripts are turned off in the config"),
    ("no_world", "ไม่พบโลก {name}", "world not found: {name}"),
    ("chunk_unloaded", "ตำแหน่งนั้นยังไม่ถูกโหลด บล็อกไม่ถูกเปลี่ยน", "that place is not loaded; the block was not changed"),
    ("bad_block", "ไม่รู้จักบล็อก {name}", "unknown block: {name}"),
    ("bad_item", "ไม่รู้จักไอเทม {name}", "unknown item: {name}"),
    ("bad_potion", "ไม่รู้จักเอฟเฟกต์ {name}", "unknown potion effect: {name}"),
    ("bad_entity", "ไม่รู้จักสิ่งมีชีวิต {name}", "unknown entity: {name}"),
    ("bad_file", "ชื่อไฟล์ log ใช้ไม่ได้: {name}", "log file name is not allowed: {name}"),
    ("not_settable", "ค่านี้เปลี่ยนไม่ได้: {name}", "this cannot be changed: {name}"),
    ("money_absent", "ไม่ได้ติดตั้ง Basics จึงไม่มีระบบเงิน: balance ไม่มีค่าและแก้ไม่ได้", "Basics is not installed, so there is no money: balance is not set and cannot be changed"),
    ("money_short", "เงินของ {name} ไม่พอ ไม่ได้หักเงิน", "{name} does not have enough money; nothing was taken"),
    ("money_refused", "Basics ไม่รับคำสั่งเรื่องเงินของ {name}: {error}", "Basics refused the money request for {name}: {error}"),
    ("money_amount", "จำนวนเงินต้องเป็นตัวเลขที่ไม่ติดลบ", "the amount of money must be a number that is not negative"),
    ("no_menu", "ผู้เล่นไม่ได้เปิดเมนูของสคริปต์อยู่", "the player has no script menu open"),
    ("no_target", "ไม่มีผู้เล่นหรือตำแหน่งให้ทำ", "there is no player or place to act on"),
    // admin command
    ("help", "&eScripts &f/sk reload <ชื่อ|all> &7| &f/sk enable <ชื่อ> &7| &f/sk disable <ชื่อ> &7| &f/sk list &7| &f/sk info &7| &f/sk version &7· สคริปต์อยู่ที่ plugins/data/scripts/scripts", "&eScripts &f/sk reload <name|all> &7| &f/sk enable <name> &7| &f/sk disable <name> &7| &f/sk list &7| &f/sk info &7| &f/sk version &7· scripts are in plugins/data/scripts/scripts"),
    ("busy", "&cScripts กำลังทำงานอยู่ ลองใหม่อีกครั้ง", "&cScripts is busy, try again"),
    ("reloaded", "&aโหลด {name} แล้ว: {commands} คำสั่ง {triggers} event {functions} ฟังก์ชัน", "&aLoaded {name}: {commands} commands, {triggers} events, {functions} functions"),
    ("reloaded_errors", "&eโหลด {name} แล้ว แต่มี {errors} จุดที่ผิด: {commands} คำสั่ง {triggers} event {functions} ฟังก์ชัน", "&eLoaded {name} with {errors} errors: {commands} commands, {triggers} events, {functions} functions"),
    ("reload_all", "&aโหลดสคริปต์ {files} ไฟล์ ผิด {errors} จุด", "&aLoaded {files} script files, {errors} errors"),
    ("not_found", "&cไม่พบสคริปต์ {name}", "&cScript not found: {name}"),
    ("is_disabled", "&cสคริปต์ {name} ปิดอยู่ ใช้ /sk enable {name}", "&cScript {name} is disabled, use /sk enable {name}"),
    ("too_big", "&cไฟล์ {name} ใหญ่เกิน {max} ไบต์ ไม่โหลด", "&cFile {name} is larger than {max} bytes and was not loaded"),
    ("unreadable", "&cอ่านไฟล์ {name} ไม่ได้", "&cCannot read file {name}"),
    ("enabled", "&aเปิดสคริปต์ {name} แล้ว", "&aScript {name} enabled"),
    ("disabled", "&aปิดสคริปต์ {name} แล้ว", "&aScript {name} disabled"),
    ("already_enabled", "&eสคริปต์ {name} เปิดอยู่แล้ว", "&eScript {name} is already enabled"),
    ("already_disabled", "&eสคริปต์ {name} ปิดอยู่แล้ว", "&eScript {name} is already disabled"),
    ("rename_failed", "&cเปลี่ยนชื่อไฟล์ {name} ไม่ได้", "&cCould not rename file {name}"),
    ("list_head", "&eสคริปต์ทั้งหมด {count} ไฟล์", "&e{count} script files"),
    ("list_on", "&a{name} &7{commands} คำสั่ง {triggers} event {functions} ฟังก์ชัน{errors}", "&a{name} &7{commands} commands, {triggers} events, {functions} functions{errors}"),
    ("list_errors", " &cผิด {errors} จุด", " &c{errors} errors"),
    ("list_off", "&8{name} &7(ปิดอยู่)", "&8{name} &7(disabled)"),
    ("list_none", "&7ยังไม่มีสคริปต์ วางไฟล์ .sk ไว้ที่ plugins/data/scripts/scripts", "&7No scripts yet. Put .sk files in plugins/data/scripts/scripts"),
    ("info", "&eสคริปต์ {scripts} ไฟล์ &7| &fคำสั่ง {commands} &7| &fevent {triggers} &7| &fตัวแปร {variables} &7| &fรอ wait {waits}", "&e{scripts} scripts &7| &f{commands} commands &7| &f{triggers} events &7| &f{variables} variables &7| &f{waits} waiting"),
    ("info_errors", "&eข้อผิดพลาดล่าสุดตอนรัน:", "&eLatest runtime errors:"),
    ("version", "&fScripts {version}", "&fScripts {version}"),
    ("new_command", "&7คำสั่งใหม่ /{name} ใช้ได้ทันทีสำหรับผู้เล่น และพิมพ์จากคอนโซลได้", "&7New command /{name} works now for players and from the console"),
    ("gone_command", "&cคำสั่งนี้ถูกถอดออกจากสคริปต์แล้ว", "&cThis command is no longer defined by a script"),
    ("line", "บรรทัด {line}: {text}", "line {line}: {text}"),
    ("warning", "&6คำเตือน &7{file} {text}", "&6warning &7{file} {text}"),
    ("error", "&c{file} {text}", "&c{file} {text}"),
    // loader errors
    ("e_effect", "ไม่เข้าใจคำสั่งนี้: {d}", "can't understand this effect: {d}"),
    ("e_condition", "ไม่เข้าใจเงื่อนไขนี้: {d}", "can't understand this condition: {d}"),
    ("e_expression", "ไม่เข้าใจค่านี้: {d}", "can't understand this expression: {d}"),
    ("e_section", "ไม่เข้าใจหัวข้อนี้: {d}", "can't understand this section: {d}"),
    ("e_structure", "ไม่เข้าใจบรรทัดนี้ (ต้องเป็น command, function, on <event>, options หรือ variables): {d}", "can't understand this line (expected command, function, on <event>, options or variables): {d}"),
    ("e_event", "ไม่เข้าใจ event นี้: {d}", "can't understand this event: {d}"),
    ("e_indent", "ย่อหน้าผิด (ใช้ tab หรือ space อย่างเดียว และย่อหน้าหลังบรรทัดที่ลงท้ายด้วย :): {d}", "indentation is wrong (use only tabs or only spaces, and indent after a line ending with ':'): {d}"),
    ("e_entry", "ไม่เข้าใจรายการนี้: {d}", "can't understand this entry: {d}"),
    ("e_command", "ประกาศคำสั่งผิด (ชื่อ อาร์กิวเมนต์ หรือไม่มี trigger): {d}", "this command definition is wrong (name, arguments or no trigger): {d}"),
    ("e_function", "ประกาศฟังก์ชันผิด: {d}", "this function definition is wrong: {d}"),
    ("e_loop", "ไม่เข้าใจสิ่งที่จะวน: {d}", "can't understand what to loop: {d}"),
    ("e_flow", "ใช้ตรงนี้ไม่ได้: {d}", "this can't be used here: {d}"),
    ("e_option", "ไม่มี option ชื่อ {d} ใน options:", "option {d} is not defined in options:"),
    ("e_duplicate", "คำสั่ง {d} มีสคริปต์อื่นประกาศไว้แล้ว ใช้ตัวที่โหลดทีหลัง", "command {d} is already defined by another script; the one loaded last wins"),
    ("e_unsupported", "ใช้บน Pumpkin ไม่ได้: {d} ({reason})", "not available on Pumpkin: {d} ({reason})"),
    ("r_aliases", "ไม่รองรับ aliases ให้ใช้ id ของ Minecraft", "aliases are not supported, use Minecraft ids"),
    ("r_gui", "เปิดได้เฉพาะเมนูแบบหีบ: open chest inventory with 3 rows named \"ชื่อ\" to player", "only chest menus can be opened: open chest inventory with 3 rows named \"title\" to player"),
    ("r_enchant", "ไม่รองรับ disenchant", "disenchant is not supported"),
    ("r_shoot", "ยิงได้เฉพาะ arrow, snowball, egg, ender pearl, splash potion, fireball, small fireball, trident, wind charge", "only arrow, snowball, egg, ender pearl, splash potion, fireball, small fireball, trident and wind charge can be shot"),
    ("r_event", "Pumpkin 0.2.2 ไม่ส่ง event นี้ให้ปลั๊กอิน", "Pumpkin 0.2.2 does not send this event to plugins"),
    ("r_pickup", "Pumpkin 0.2.2 ไม่บอกว่าเก็บไอเทมอะไร ใช้ on pickup: เฉย ๆ", "Pumpkin 0.2.2 does not say which item was picked up, use plain on pickup:"),
    ("r_chat_format", "เปลี่ยนรูปแบบแชตไม่ได้ ให้แก้ message แทน", "the chat format cannot be changed, change the message instead"),
    ("r_loop", "วนได้เฉพาะ blocks in radius n around ตำแหน่ง, entities [in radius n around ตำแหน่ง] และ items in inventory", "only blocks in radius n around a place, entities [in radius n around a place] and items in an inventory can be looped"),
    ("r_vector", "ไม่รองรับ vector ให้ใช้ push", "vectors are not supported, use push"),
    ("r_entity", "ไม่รู้จักชนิดสิ่งมีชีวิตนี้", "unknown entity type"),
    ("r_join_message", "เปลี่ยนข้อความเข้าออกของเกมไม่ได้ ข้อความของคุณจะถูกประกาศเพิ่มอีกบรรทัด", "the game's join/quit line cannot be replaced; your text is broadcast as an extra line"),
    ("r_cooldown_storage", "ไม่รองรับ cooldown storage คูลดาวน์เก็บในหน่วยความจำ", "cooldown storage is ignored; cooldowns are kept in memory"),
];

pub fn tr(th: bool, key: &str) -> &'static str {
    TEXTS.iter().find(|(k, _, _)| *k == key).map_or("", |(_, t, e)| if th { t } else { e })
}

pub fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// `line 12: can't understand this effect: …`
pub fn format_error(th: bool, e: &ScriptError) -> String {
    let reason = e.reason.map(|r| tr(th, &format!("r_{r}"))).unwrap_or_default();
    let text = fill(tr(th, &format!("e_{}", e.code)), &[("d", e.detail.clone()), ("reason", reason.to_owned())]);
    fill(tr(th, "line"), &[("line", e.line.to_string()), ("text", text)])
}

/// A script file name an admin typed, or `None` when it could be a path.
pub fn script_name(input: &str) -> Option<String> {
    let name = input.trim().trim_start_matches('-');
    let name = name.strip_suffix(".sk").unwrap_or(name);
    let ok = !name.is_empty() && name.len() <= 64 && !name.starts_with('.') && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ')) && !name.contains("..");
    ok.then(|| name.to_lowercase())
}

/// The key of a script from what an admin typed or from a path under `scripts/`:
/// lower case, `/` between folders (three at most), no leading `-`, always `.sk`.
pub fn script_path(input: &str) -> Option<String> {
    let input = input.trim().replace('\\', "/");
    let parts: Vec<&str> = input.split('/').collect();
    if parts.len() > 4 {
        return None;
    }
    let names: Option<Vec<String>> = parts.iter().map(|p| script_name(p)).collect();
    Some(format!("{}.sk", names?.join("/")))
}

/// A log file name a script gave: letters, digits, `_`, `-`, one folder at most, always `.log`.
pub fn log_file(input: &str) -> Option<String> {
    let name = input.trim().trim_end_matches(".log").replace('\\', "/");
    let parts: Vec<&str> = name.split('/').collect();
    let ok = !name.is_empty() && name.len() <= 64 && parts.len() <= 2 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')));
    ok.then(|| format!("logs/{name}.log"))
}

pub const EXAMPLES: &[(&str, &str)] = &[
    (
        "-welcome.sk",
        r#"# Scripts example: welcome messages and a join counter.
# Remove the "-" in front of the file name (or type /sk enable welcome) to turn it on.

options:
	server: &bMy Server

on first join:
	add 1 to {joins::total}
	broadcast "&e%player% &fjoined for the first time! (player number %{joins::total}%)"
	give 1 bread to player

on join:
	add 1 to {joins::%player's uuid%}
	send "&fWelcome to {@server}&f, &a%player%&f! You have joined %{joins::%player's uuid%}% times." to player
	send title "&bWelcome" with subtitle "&f%player%" to player for 3 seconds

on quit:
	set {lastseen::%player's uuid%} to now
"#,
    ),
    (
        "-commands.sk",
        r#"# Scripts example: commands with arguments, permission and cooldown.

command /heal [<player>]:
	description: Heal yourself or another player
	permission: scripts.heal
	permission message: &cYou may not heal.
	cooldown: 30 seconds
	cooldown message: &cWait %remaining time% before healing again.
	trigger:
		if arg-1 is set:
			heal arg-1
			feed arg-1
			send "&aYou healed %arg-1%." to player
			send "&a%player% healed you." to arg-1
		else:
			heal player
			feed player
			send "&aYou are healed."

command /setspawn:
	permission: scripts.setspawn
	executable by: players
	trigger:
		set {spawn} to location of player
		send "&aSpawn set."

command /spawn:
	cooldown: 10 seconds
	executable by: players
	trigger:
		if {spawn} is not set:
			send "&cThere is no spawn yet."
			stop
		send "&7Teleporting in 3 seconds..."
		wait 3 seconds
		teleport player to {spawn}
		send "&aWelcome to spawn."
"#,
    ),
    (
        "-rewards.sk",
        r#"# Scripts example: block rewards, a chat filter and a timed broadcast.

function reward(p: player, amount: number = 1):
	add {_amount} to {coins::%{_p}'s uuid%}
	send action bar "&6+%{_amount}% coins &7(total %{coins::%{_p}'s uuid%}%)" to {_p}

on break of diamond ore or deepslate diamond ore:
	reward(player, 5)

on break of stone:
	chance of 5%
	reward(player)

on chat:
	if message contains "badword":
		cancel event
		send "&cPlease keep the chat friendly."

command /coins [<offline player>]:
	trigger:
		if arg-1 is set:
			send "&e%arg-1% has %{coins::%arg-1's uuid%} ? 0% coins."
		else:
			send "&eYou have %{coins::%player's uuid%} ? 0% coins."

every 10 minutes:
	broadcast "&bTip: &fmine diamonds to earn coins, check them with /coins"
"#,
    ),
];
