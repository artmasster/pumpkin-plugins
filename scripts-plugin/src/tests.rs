//! Engine tests against a mock server, with scripts of the kind people copy from Skript forums.
use std::collections::{HashMap, HashSet};

use crate::ast::{Check, EventKind, Fx, Prop, SendKind};
use crate::cfg;
use crate::engine::{Ctx, Engine, Host, Limits, Messages, VarStore};
use crate::parse::Names;
use crate::value::{self, Item, Loc, PlayerRef, Value};

struct MockPlayer {
    me: PlayerRef,
    health: f64,
    food: f64,
    level: f64,
    gamemode: String,
    loc: Loc,
    perms: HashSet<String>,
    op: bool,
    items: Vec<Item>,
    sneaking: bool,
}

#[derive(Default)]
struct Mock {
    now: f64,
    rand: f64,
    players: Vec<MockPlayer>,
    out: Vec<String>,
    blocks: HashMap<(i32, i32, i32), String>,
    time: f64,
    /// Balances by player name; `None` = no economy plugin.
    bank: Option<HashMap<String, f64>>,
}

struct Reg;
impl Names for Reg {
    fn item(&self, id: &str) -> bool {
        matches!(
            id,
            "stone" | "diamond" | "diamond_sword" | "oak_log" | "bread" | "diamond_ore" | "deepslate_diamond_ore" | "apple" | "dirt" | "cobblestone" | "air" | "torch" | "iron_ingot" | "emerald" | "tnt" | "bedrock" | "oak_planks" | "golden_apple" | "stick" | "grass_block"
        )
    }
    fn entity(&self, id: &str) -> bool {
        matches!(id, "zombie" | "cow" | "pig" | "snow_golem")
    }
}

impl Mock {
    fn new() -> Mock {
        let mut m = Mock { now: 1_700_000_000.0, rand: 0.5, ..Mock::default() };
        m.join("Alice", true);
        m.join("Bob", false);
        m
    }

    fn join(&mut self, name: &str, op: bool) {
        self.players.push(MockPlayer {
            me: PlayerRef { uuid: format!("uuid-{}", name.to_lowercase()), name: name.to_owned() },
            health: 10.0,
            food: 12.0,
            level: 3.0,
            gamemode: "survival".into(),
            loc: Loc { x: 1.5, y: 64.0, z: -3.5, world: "world".into(), yaw: 0.0, pitch: 0.0 },
            perms: HashSet::new(),
            op,
            items: Vec::new(),
            sneaking: false,
        });
    }

    fn p(&self, name: &str) -> Value {
        Value::Player(self.players.iter().find(|p| p.me.name == name).map(|p| p.me.clone()).unwrap())
    }

    fn who(&mut self, v: &Value) -> Option<&mut MockPlayer> {
        match v {
            Value::Player(r) => self.players.iter_mut().find(|p| p.me.uuid == r.uuid),
            _ => None,
        }
    }

    fn sent(&self, needle: &str) -> usize {
        self.out.iter().filter(|l| l.contains(needle)).count()
    }

    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }
}

impl Host for Mock {
    fn now(&mut self) -> f64 {
        self.now
    }
    fn random(&mut self) -> f64 {
        self.rand
    }
    fn players(&mut self) -> Vec<PlayerRef> {
        self.players.iter().map(|p| p.me.clone()).collect()
    }
    fn find_player(&mut self, name: &str) -> Option<PlayerRef> {
        self.players.iter().find(|p| p.me.name.eq_ignore_ascii_case(name)).map(|p| p.me.clone())
    }
    fn offline_player(&mut self, name: &str) -> PlayerRef {
        self.find_player(name).unwrap_or(PlayerRef { uuid: format!("offline-{}", name.to_lowercase()), name: name.to_owned() })
    }
    fn item(&mut self, text: &str) -> Option<Item> {
        Reg.item(&value::norm_id(text)).then(|| Item::new(text, 1))
    }
    fn get(&mut self, target: &Value, prop: Prop) -> Value {
        if let Value::Block(l) = target {
            return Value::Item(Item::new(self.blocks.get(&l.block()).map_or("air", String::as_str), 1));
        }
        if let (Value::World(_), Prop::Time) = (target, prop) {
            return Value::Num(self.time);
        }
        if let (Value::Player(r), Prop::Balance) = (target, prop) {
            return self.bank.as_ref().and_then(|b| b.get(&r.name)).map_or(Value::None, |n| Value::Num(*n));
        }
        let Some(p) = self.who(target) else { return Value::None };
        match prop {
            Prop::Health => Value::Num(p.health),
            Prop::MaxHealth => Value::Num(20.0),
            Prop::Food => Value::Num(p.food),
            Prop::Level => Value::Num(p.level),
            Prop::Gamemode => Value::Text(p.gamemode.clone()),
            Prop::Location => Value::Loc(p.loc.clone()),
            Prop::World => Value::World(p.loc.world.clone()),
            Prop::Tool => p.items.first().cloned().map_or(Value::None, Value::Item),
            Prop::DisplayName => Value::Text(p.me.name.clone()),
            _ => Value::None,
        }
    }
    fn set(&mut self, target: &Value, prop: Prop, value: &Value) -> Result<(), String> {
        if let Value::Block(l) = target {
            let id = match value {
                Value::Item(i) => i.id.clone(),
                other => value::norm_id(&value::to_text(other)),
            };
            self.blocks.insert(l.block(), id);
            return Ok(());
        }
        if let (Value::Player(r), Prop::Balance) = (target, prop) {
            let bank = self.bank.as_mut().ok_or("no economy")?;
            bank.insert(r.name.clone(), value.num().ok_or("bad amount")?);
            return Ok(());
        }
        let Some(p) = self.who(target) else { return Err("no target".into()) };
        match prop {
            Prop::Health => p.health = value.num().unwrap_or(20.0).clamp(0.0, 20.0),
            Prop::Food => p.food = value.num().unwrap_or(20.0),
            Prop::Level => p.level = value.num().unwrap_or(0.0),
            Prop::Gamemode => p.gamemode = value::to_text(value),
            _ => return Err(format!("cannot set {prop:?}")),
        }
        Ok(())
    }
    fn check(&mut self, check: Check, args: &[Value]) -> bool {
        let extra = args.get(1).cloned().unwrap_or(Value::None);
        let Some(p) = self.who(&args[0]) else { return false };
        match check {
            Check::HasPerm => p.op || p.perms.contains(&value::to_text(&extra)),
            Check::IsOp => p.op,
            Check::Online | Check::Alive => true,
            Check::Sneaking => p.sneaking,
            Check::HasItem => match extra {
                Value::Item(want) => p.items.iter().filter(|i| i.id == want.id).map(|i| i.count).sum::<u32>() >= want.count,
                _ => false,
            },
            _ => false,
        }
    }
    fn fx(&mut self, fx: Fx, args: &[Value], flags: u32) -> Result<(), String> {
        match fx {
            Fx::Money => {
                let bank = self.bank.as_mut().ok_or("no economy")?;
                let (Value::Player(r), Some(amount)) = (&args[0], args[1].num()) else { return Err("bad money request".into()) };
                let have = bank.entry(r.name.clone()).or_insert(0.0);
                if flags == 1 && *have < amount {
                    return Err(format!("{} does not have enough money", r.name));
                }
                *have += if flags == 1 { -amount } else { amount };
            }
            Fx::Give => {
                let items = args[0].clone().flat();
                for who in args[1].clone().flat() {
                    if let Some(p) = self.who(&who) {
                        for i in &items {
                            if let Value::Item(i) = i {
                                p.items.push(i.clone());
                            }
                        }
                    }
                }
            }
            Fx::Take => {
                if let (Some(Value::Item(want)), Some(p)) = (args[0].clone().flat().first().cloned(), self.who(&args[1])) {
                    let mut left = want.count;
                    for i in &mut p.items {
                        if i.id == want.id {
                            let n = left.min(i.count);
                            i.count -= n;
                            left -= n;
                        }
                    }
                    p.items.retain(|i| i.count > 0);
                }
            }
            Fx::Heal => {
                let amount = args.get(1).and_then(Value::num);
                if let Some(p) = self.who(&args[0]) {
                    p.health = match (amount, flags) {
                        (Some(a), 1) => p.health - a,
                        (Some(a), _) => (p.health + a).min(20.0),
                        (None, _) => 20.0,
                    };
                }
            }
            Fx::Feed => {
                if let Some(p) = self.who(&args[0]) {
                    p.food = 20.0;
                }
            }
            Fx::Teleport => {
                let to = match &args[1] {
                    Value::Loc(l) => l.clone(),
                    other => match self.get(other, Prop::Location) {
                        Value::Loc(l) => l,
                        _ => return Err("no place".into()),
                    },
                };
                if let Some(p) = self.who(&args[0]) {
                    p.loc = to;
                }
            }
            _ => {}
        }
        let shown: Vec<String> = args.iter().map(value::to_text).collect();
        self.out.push(format!("fx {fx:?} {} #{flags}", shown.join(" | ")));
        Ok(())
    }
    fn send(&mut self, to: &Value, text: &str, kind: SendKind) {
        let kind = if kind == SendKind::ActionBar { "bar" } else { "send" };
        self.out.push(format!("{kind} {}: {text}", value::to_text(to)));
    }
    fn broadcast(&mut self, text: &str) {
        self.out.push(format!("broadcast: {text}"));
    }
    fn log(&mut self, line: &str) {
        self.out.push(format!("log: {line}"));
    }
    fn entities(&mut self, around: Option<(&Loc, f64)>) -> Vec<Value> {
        let all = [("zombie", 0.0), ("cow", 4.0), ("pig", 30.0)];
        let near = |d: f64| around.is_none_or(|(_, r)| d <= r);
        all.iter().enumerate().filter(|(_, (_, d))| near(*d)).map(|(i, (k, _))| Value::Entity(value::EntityRef { id: i as i32 + 100, kind: (*k).to_owned(), world: "world".into() })).collect()
    }
}

fn load(src: &str) -> (Engine, Mock) {
    let mut eng = Engine::new(Limits::default(), Messages::default());
    let errors: Vec<String> = eng.load("test.sk", src, &Reg).errors.iter().map(|e| cfg::format_error(false, e)).collect();
    assert!(errors.is_empty(), "script errors: {errors:#?}");
    (eng, Mock::new())
}

fn errors(src: &str) -> Vec<String> {
    let mut eng = Engine::default();
    eng.load("test.sk", src, &Reg).errors.iter().map(|e| cfg::format_error(false, e)).collect()
}

fn ctx(m: &Mock, name: &str) -> Ctx {
    Ctx::default().with("player", m.p(name)).with("world", Value::World("world".into()))
}

/// Runs lines inside a command as Alice and returns what the server was told.
fn run(body: &str) -> Vec<String> {
    let body: String = body.lines().map(|l| format!("\t\t{l}\n")).collect();
    let (mut eng, mut m) = load(&format!("command /t:\n\ttrigger:\n{body}"));
    let alice = m.p("Alice");
    assert!(eng.run_command(&mut m, "t", alice, ""));
    m.take()
}

// ---- values ------------------------------------------------------------------------------------

#[test]
fn timespans() {
    assert_eq!(value::parse_span("5 seconds"), Some(5.0));
    assert_eq!(value::parse_span("1 minute and 30 seconds"), Some(90.0));
    assert_eq!(value::parse_span("a tick"), Some(0.05));
    assert_eq!(value::parse_span("2 hours, 3 minutes and 4 seconds"), Some(7384.0));
    assert_eq!(value::parse_span("1.5 seconds"), Some(1.5));
    assert_eq!(value::parse_span("5"), None);
    assert_eq!(value::parse_span("5 bananas"), None);
    assert_eq!(value::fmt_span(90.0), "1 minute and 30 seconds");
    assert_eq!(value::fmt_span(1.0), "1 second");
    assert_eq!(value::fmt_span(3661.0), "1 hour, 1 minute and 1 second");
    assert_eq!(value::fmt_span(0.1), "2 ticks");
    assert_eq!(value::fmt_span(86_400.0 * 2.0), "2 days");
    assert_eq!(crate::engine::short_span("30s"), Some(30.0));
    assert_eq!(crate::engine::short_span("5m"), Some(300.0));
    assert_eq!(crate::engine::short_span("x"), None);
}

#[test]
fn numbers_dates_and_colours() {
    assert_eq!(value::fmt_num(3.0), "3");
    assert_eq!(value::fmt_num(2.5), "2.5");
    assert_eq!(value::fmt_num(1.0 / 3.0), "0.33");
    assert_eq!(value::parse_number("-4.5"), Some(-4.5));
    assert_eq!(value::parse_number("1e5"), None);
    assert_eq!(value::parse_number("nan"), None);
    assert_eq!(value::fmt_date(0.0, value::DATE_FORMAT), "1970-01-01 00:00:00");
    assert_eq!(value::fmt_date(1_700_000_000.0, "dd/MM/yyyy HH:mm"), "14/11/2023 22:13");
    assert_eq!(value::colorize("<red>hi <bold>there §a!"), "&chi &lthere &a!");
    assert_eq!(value::colorize("a < b and <unknown>"), "a < b and <unknown>");
    assert_eq!(value::uncolor("&aHi &lthere & you"), "Hi there & you");
    assert_eq!(value::parse_time_of_day("6:00"), Some(0));
    assert_eq!(value::parse_time_of_day("18:00"), Some(12_000));
    assert_eq!(value::parse_time_of_day("12:30"), Some(6500));
    assert_eq!(value::parse_time_of_day("noon"), Some(6000));
    assert_eq!(value::parse_time_of_day("6 pm"), Some(12_000));
    assert_eq!(value::parse_time_of_day("5"), None);
    assert_eq!(crate::engine::replace_ci("Hello HELLO ñandú İi", "hello", "x"), "x x ñandú İi");
    assert_eq!(crate::engine::replace_ci("สวัสดี Bad คำ", "bad", "***"), "สวัสดี *** คำ");
    assert_eq!(value::world_name("minecraft:the_nether"), "world_nether");
    assert_eq!(value::world_dimension("world"), "minecraft:overworld");
}

#[test]
fn variable_store_lists_and_json() {
    let mut v = VarStore::default();
    for (k, n) in [("list::10", 10.0), ("list::2", 2.0), ("list::1", 1.0), ("list::b", 0.0), ("list::a::deep", 9.0), ("other", 5.0)] {
        assert!(v.set(k, Value::Num(n), 100));
    }
    let keys: Vec<String> = v.list("list").into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, ["1", "2", "10", "b"]);
    v.delete("list");
    assert_eq!(v.len(), 1);
    assert!(v.set("p", Value::Player(PlayerRef { uuid: "u".into(), name: "N".into() }), 2));
    assert!(!v.set("third", Value::Num(1.0), 2));
    assert!(v.set("other", Value::Num(6.0), 2));
    let back = VarStore::from_json(v.to_json()).unwrap();
    assert_eq!(back.get("other"), Value::Num(6.0));
    assert_eq!(value::to_text(&back.get("p")), "N");
    assert!(v.set("other", Value::None, 2));
    assert_eq!(v.len(), 1);
}

// ---- language ----------------------------------------------------------------------------------

#[test]
fn arithmetic_and_text() {
    let out = run(r#"set {_x} to 2 + 3 * 4
send "%{_x}%"
send "%(2 + 3) * 4%"
send "%2 ^ 3 ^ 2%"
send "%10 / 4%"
send "%{_x} - 4 - 3%"
send "%-{_x} + 1%"
send "%{_unset} + 5%"
send "%7 / 0%"
send "100%% sure, say ""hi"""
send "%length of ""hello""% %""hello"" in upper case% %first 2 characters of ""hello""% %subtext of ""hello"" from 2 to 4%"
send "%join (""a"", ""b"" and ""c"") with ""-""% | %size of split ""a,b,c"" at "",""%"
send "%round(2.567, 2)% %floor(2.9)% %ceil(2.1)% %abs(-3)% %sqrt(16)% %min(4, 2, 9)% %max(4, 2, 9)% %mod(7, 3)%"
send "%{_none} ? ""fallback""%""#);
    assert_eq!(
        out,
        [
            "send Alice: 14",
            "send Alice: 20",
            "send Alice: 512",
            "send Alice: 2.5",
            "send Alice: 7",
            "send Alice: -13",
            "send Alice: 5",
            "send Alice: 0",
            "send Alice: 100% sure, say \"hi\"",
            "send Alice: 5 HELLO he ell",
            "send Alice: a-b-c | 3",
            "send Alice: 2.57 2 3 3 4 2 9 1",
            "send Alice: fallback",
        ]
    );
}

#[test]
fn player_properties_and_changes() {
    let out = run(r#"send "%player% %player's name% %name of player% %player's uuid% %player's health% %player's world%"
add 4 to player's health
remove 2 from player's food level
set player's level to player's level * 2
set player's gamemode to creative
send "%health of player% %player's food level% %player's level% %player's gamemode%"
send "%x-coordinate of player% %player's y-coordinate% %z-coordinate of location of player%"
send "%location of player%"
send "%distance between player and location(1.5, 60, -3.5, world ""world"")%"
heal player
send "%player's health%"
damage player by 5
send "%player's health%""#);
    assert_eq!(out[0], "send Alice: Alice Alice Alice uuid-alice 10 world");
    assert_eq!(out[1], "send Alice: 14 10 6 creative");
    assert_eq!(out[2], "send Alice: 1.5 64 -3.5");
    assert_eq!(out[3], "send Alice: x: 1.5, y: 64, z: -3.5 in world");
    assert_eq!(out[4], "send Alice: 4");
    assert_eq!(out[6], "send Alice: 20");
    assert_eq!(out[8], "send Alice: 15");
}

#[test]
fn conditions() {
    let out = run(r#"set {_n} to 5
if {_n} is 5:
	send "eq"
if {_n} is not 5:
	send "wrong"
else if {_n} is greater than 4:
	send "gt"
else:
	send "wrong"
if {_n} is between 1 and 10:
	send "between"
if {_n} >= 5:
	send "ge"
if {_n} is less than or equal to 4:
	send "wrong"
if {_x} is not set:
	send "unset"
if {_n} is set:
	send "set"
if "Hello World" contains "world":
	send "contains"
if "Hello" starts with "he":
	send "starts"
if "Hello" doesn't end with "x":
	send "not ends"
if "abc123" matches "[a-z]+\d+":
	send "matches"
if {_n} is 1, 5 or 9:
	send "any"
if {_n} is not 1 or 2:
	send "neither"
if player is op:
	send "op"
if player has permission "some.node":
	send "perm"
if player is online:
	send "online"
if player isn't sneaking:
	send "standing"
if player is in world "world":
	send "world"
if player's gamemode is survival:
	send "survival"
if {_n} is 5 and player is op:
	send "and"
if {_n} is 4 or player is op:
	send "or"
if player doesn't have 1 diamond:
	send "no diamond"
give 3 diamonds to player
if player has 2 diamonds:
	send "has diamonds"
if player's inventory contains 5 diamonds:
	send "wrong"
if player's tool is diamond:
	send "holding"
chance of 60%:
	send "lucky"
chance of 40%:
	send "wrong""#);
    let got: Vec<&str> = out.iter().filter_map(|l| l.strip_prefix("send Alice: ")).collect();
    assert_eq!(
        got,
        [
            "eq", "gt", "between", "ge", "unset", "set", "contains", "starts", "not ends", "matches", "any", "neither", "op", "perm", "online", "standing", "world", "survival", "and", "or",
            "no diamond", "has diamonds", "holding", "lucky"
        ]
    );
}

#[test]
fn loops_and_flow() {
    let out = run(r#"set {_l::*} to "a", "b" and "c"
add "d" to {_l::*}
remove "b" from {_l::*}
loop {_l::*}:
	send "%loop-index%=%loop-value%"
send "size %size of {_l::*}%"
loop 3 times:
	if loop-number is 2:
		continue
	send "n%loop-number%"
loop integers from 5 to 9:
	if loop-number is 7:
		exit loop
	send "i%loop-number%"
set {_i} to 0
while {_i} < 3:
	add 1 to {_i}
	send "w%{_i}%"
loop 2 times:
	loop 2 times:
		send "%loop-number-1%.%loop-number-2%"
loop all players:
	send "p %loop-player%"
loop 3 times:
	loop-number is 2
	send "only %loop-number%"
send "after"
{_i} is 99
send "never""#);
    let got: Vec<&str> = out.iter().filter_map(|l| l.strip_prefix("send Alice: ")).collect();
    assert_eq!(
        got,
        ["1=a", "3=c", "4=d", "size 3", "n1", "n3", "i5", "i6", "w1", "w2", "w3", "1.1", "1.2", "2.1", "2.2", "p Alice", "p Bob", "only 2", "after"]
    );
}

#[test]
fn global_lists_keyed_by_player() {
    let (mut eng, mut m) = load(
        r#"on join:
	add 1 to {joins::%player's uuid%}
	add player's name to {seen::*}
	set {last::%player%} to now
	send "joined %{joins::%player's uuid%}% times, %size of {seen::*}% seen"
"#,
    );
    for name in ["Alice", "Bob", "Alice"] {
        {
 let c0 = ctx(&m, name);
 eng.fire(&mut m, EventKind::Join, c0)
};
    }
    assert_eq!(m.out, ["send Alice: joined 1 times, 1 seen", "send Bob: joined 1 times, 2 seen", "send Alice: joined 2 times, 3 seen"]);
    assert_eq!(eng.vars.get("joins::uuid-alice"), Value::Num(2.0));
    assert_eq!(eng.vars.get("last::alice"), Value::Date(1_700_000_000.0));
    assert!(eng.vars.dirty);
}

#[test]
fn player_text_is_data_not_code() {
    // a name or chat text with braces and percent signs must stay a plain value
    let (mut eng, mut m) = load(
        r#"on chat:
	set {said::%player%} to message
	set {key::%message%} to 1
	send "%message%"
"#,
    );
    let evil = "%{coins::*}% {x} \" on quit: {@a}";
    {
 let c0 = ctx(&m, "Bob").with("message", Value::text(evil));
 eng.fire(&mut m, EventKind::Chat, c0)
};
    assert_eq!(m.out, [format!("send Bob: {evil}")]);
    assert_eq!(eng.vars.get("said::bob"), Value::text(evil));
    assert_eq!(eng.vars.len(), 2);
}

// ---- community-style fixtures ------------------------------------------------------------------

const WELCOME: &str = r#"
options:
	prefix: &8[&aServer&8]&r
	join-item: bread

variables:
	{motd} = "Have fun!"

on first join:
	broadcast "{@prefix} &eWelcome &6%player% &eto the server for the first time!"
	give 5 {@join-item} to player
	set {firstjoin::%player's uuid%} to now

on join:
	send "{@prefix} &fWelcome back, &a%player%&f! %{motd}%" to player
	send title "&aWelcome" with subtitle "&7%player%" to player for 3 seconds
	if player is op:
		send "{@prefix} &cYou are an operator." to player

on quit:
	broadcast "{@prefix} &7%player% left."
"#;

#[test]
fn fixture_welcome() {
    let (mut eng, mut m) = load(WELCOME);
    {
 let c0 = ctx(&m, "Bob");
 eng.fire(&mut m, EventKind::FirstJoin, c0)
};
    {
 let c0 = ctx(&m, "Bob");
 eng.fire(&mut m, EventKind::Join, c0)
};
    {
 let c0 = ctx(&m, "Alice");
 eng.fire(&mut m, EventKind::Join, c0)
};
    {
 let c0 = ctx(&m, "Bob");
 eng.fire(&mut m, EventKind::Quit, c0)
};
    assert_eq!(
        m.out,
        [
            "broadcast: &8[&aServer&8]&r &eWelcome &6Bob &eto the server for the first time!",
            "fx Give 5 bread | Bob #0",
            "send Bob: &8[&aServer&8]&r &fWelcome back, &aBob&f! Have fun!",
            "fx Title &aWelcome | &7Bob | Bob | 3 seconds | <none> | <none> #0",
            "send Alice: &8[&aServer&8]&r &fWelcome back, &aAlice&f! Have fun!",
            "fx Title &aWelcome | &7Alice | Alice | 3 seconds | <none> | <none> #0",
            "send Alice: &8[&aServer&8]&r &cYou are an operator.",
            "broadcast: &8[&aServer&8]&r &7Bob left.",
        ]
    );
    assert_eq!(m.players[1].items[0], Item::new("bread", 5));
}

const COMMANDS: &str = r#"
command /heal [<player>]:
	description: Heals a player
	usage: /heal [player]
	permission: server.heal
	permission message: &cNo permission!
	aliases: /h, hp
	cooldown: 30 seconds
	cooldown message: &cWait %remaining time%!
	cooldown bypass: server.heal.bypass
	trigger:
		if arg-1 is set:
			heal arg-1
			feed arg-1
			send "&aHealed %arg-1%" to player
			send "&aYou were healed by %player%" to arg-1
		else:
			heal player
			send "&aHealed"

command /setspawn:
	permission: server.admin
	executable by: players
	trigger:
		set {spawn} to location of player
		send "&aSpawn set at %{spawn}%"

command /spawn:
	executable by: players
	trigger:
		if {spawn} isn't set:
			send "&cNo spawn"
			stop
		teleport player to {spawn}
		send "&aTeleported"

command /give <player> <item> [<integer = 1>]:
	trigger:
		give arg-3 of arg-2 to arg-1
		send "gave %arg-3% %arg-2% to %arg-1%"

command /say <text>:
	executable by: console
	trigger:
		broadcast "[Server] %arg-1%"

command /greet <name:text> [<times:integer = 2>] [<who:player = %player%>]:
	trigger:
		loop {_times} times:
			send "hi %{_name}% from %{_who}%" to {_who}
		send "%arguments%|%arg 1%|%argument 2%|%arg-text%|%integer-argument%"
"#;

#[test]
fn fixture_commands_permission_cooldown_arguments() {
    let (mut eng, mut m) = load(COMMANDS);
    let (alice, bob) = (m.p("Alice"), m.p("Bob"));
    // not permitted
    assert!(eng.run_command(&mut m, "heal", bob.clone(), ""));
    assert_eq!(m.take(), ["send Bob: &cNo permission!"]);
    // permitted, heals self, then the cooldown answers
    m.players[1].perms.insert("server.heal".into());
    eng.run_command(&mut m, "heal", bob.clone(), "");
    assert_eq!(m.players[1].health, 20.0);
    m.now += 10.0;
    eng.run_command(&mut m, "h", bob.clone(), "");
    assert_eq!(m.take()[2], "send Bob: &cWait 20 seconds!");
    m.now += 21.0;
    eng.run_command(&mut m, "hp", bob.clone(), "alice");
    assert_eq!(m.take()[2..], ["send Bob: &aHealed Alice", "send Alice: &aYou were healed by Bob"]);
    // operators bypass (they have every node)
    eng.run_command(&mut m, "heal", alice.clone(), "");
    eng.run_command(&mut m, "heal", alice.clone(), "");
    assert_eq!(m.sent("&aHealed"), 2);
    m.take();
    // usage and wrong arguments
    eng.run_command(&mut m, "heal", alice.clone(), "Nobody");
    eng.run_command(&mut m, "heal", alice.clone(), "Bob extra");
    eng.run_command(&mut m, "give", alice.clone(), "Bob");
    eng.run_command(&mut m, "give", alice.clone(), "Bob unobtainium");
    eng.run_command(&mut m, "give", alice.clone(), "Bob diamond lots");
    assert_eq!(
        m.take(),
        [
            "send Alice: &cPlayer not found: Nobody",
            "send Alice: /heal [player]",
            "send Alice: &cUsage: /give <player> <item> [<integer>]",
            "send Alice: &cUsage: /give <player> <item> [<integer>]",
            "send Alice: &cUsage: /give <player> <item> [<integer>]",
        ]
    );
    // defaults and typed arguments
    eng.run_command(&mut m, "give", alice.clone(), "Bob diamond");
    eng.run_command(&mut m, "give", alice.clone(), "Bob oak_log 12");
    assert_eq!(m.take(), ["fx Give diamond | Bob #0", "send Alice: gave 1 diamond to Bob", "fx Give 12 oak log | Bob #0", "send Alice: gave 12 oak log to Bob"]);
    // spawn: players only, set then teleport
    eng.run_command(&mut m, "spawn", Value::Console, "");
    eng.run_command(&mut m, "spawn", bob.clone(), "");
    eng.run_command(&mut m, "setspawn", bob.clone(), "");
    m.players[0].loc.x = 100.0;
    eng.run_command(&mut m, "setspawn", alice.clone(), "");
    eng.run_command(&mut m, "spawn", bob.clone(), "");
    assert_eq!(m.players[1].loc.x, 100.0);
    let out = m.take();
    assert_eq!(out[0], "send console: &cOnly players can use this command.");
    assert_eq!(out[1], "send Bob: &cNo spawn");
    assert_eq!(out[2], "send Bob: &cYou don't have permission to use this command.");
    assert_eq!(out[3], "send Alice: &aSpawn set at x: 100, y: 64, z: -3.5 in world");
    // console only, greedy text
    eng.run_command(&mut m, "say", alice.clone(), "hi");
    eng.run_command(&mut m, "say", Value::Console, "hello   big world");
    assert_eq!(m.take(), ["send Alice: &cOnly the console can use this command.", "broadcast: [Server] hello big world"]);
    // named arguments with defaults
    eng.run_command(&mut m, "greet", bob.clone(), "you");
    assert_eq!(m.take(), ["send Bob: hi you from Bob", "send Bob: hi you from Bob", "send Bob: you 2 Bob|you|2|you|2"]);
    eng.run_command(&mut m, "greet", bob, "x 1 Alice");
    assert_eq!(m.take()[0], "send Alice: hi x from Alice");
    assert!(!eng.run_command(&mut m, "nosuch", alice, ""));
}

const ECONOMY: &str = r#"
options:
	start: 100

on join:
	if {money::%player's uuid%} is not set:
		set {money::%player's uuid%} to {@start}

command /bal [<offline player>]:
	aliases: balance, money
	trigger:
		if arg-1 is set:
			send "&e%arg-1%: &a$%{money::%arg-1's uuid%} ? 0%"
		else:
			send "&eBalance: &a$%{money::%player's uuid%}%"

command /pay <player> <number>:
	trigger:
		if arg-2 <= 0:
			send "&cAmount must be positive."
			stop
		if arg-1 is player:
			send "&cYou can't pay yourself."
			stop
		if {money::%player's uuid%} < arg-2:
			send "&cNot enough money."
			stop
		remove arg-2 from {money::%player's uuid%}
		add arg-2 to {money::%arg-1's uuid%}
		send "&aPaid $%arg-2% to %arg-1%."
		send "&a%player% paid you $%arg-2%." to arg-1

command /baltop:
	trigger:
		set {_best} to -1
		loop {money::*}:
			if loop-value > {_best}:
				set {_best} to loop-value
				set {_who} to loop-index
		send "top: %{_who}% with %{_best}%"
"#;

#[test]
fn fixture_economy_with_variables() {
    let (mut eng, mut m) = load(ECONOMY);
    let (alice, bob) = (m.p("Alice"), m.p("Bob"));
    {
 let c0 = ctx(&m, "Alice");
 eng.fire(&mut m, EventKind::Join, c0)
};
    {
 let c0 = ctx(&m, "Bob");
 eng.fire(&mut m, EventKind::Join, c0)
};
    eng.run_command(&mut m, "pay", alice.clone(), "Bob 30.5");
    eng.run_command(&mut m, "pay", alice.clone(), "Bob 500");
    eng.run_command(&mut m, "pay", alice.clone(), "Alice 5");
    eng.run_command(&mut m, "pay", alice.clone(), "Bob -5");
    eng.run_command(&mut m, "bal", alice.clone(), "");
    eng.run_command(&mut m, "money", alice.clone(), "bob");
    eng.run_command(&mut m, "bal", alice.clone(), "Stranger");
    eng.run_command(&mut m, "baltop", bob, "");
    assert_eq!(
        m.out,
        [
            "send Alice: &aPaid $30.5 to Bob.",
            "send Bob: &aAlice paid you $30.5.",
            "send Alice: &cNot enough money.",
            "send Alice: &cYou can't pay yourself.",
            "send Alice: &cAmount must be positive.",
            "send Alice: &eBalance: &a$69.5",
            "send Alice: &eBob: &a$130.5",
            "send Alice: &eStranger: &a$0",
            "send Bob: top: uuid-bob with 130.5",
        ]
    );
    // joining again keeps the balance
    {
 let c0 = ctx(&m, "Alice");
 eng.fire(&mut m, EventKind::Join, c0)
};
    assert_eq!(eng.vars.get("money::uuid-alice"), Value::Num(69.5));
}

const CHAT: &str = r#"
options:
	words: "darn", "heck" or "frick"

on chat:
	if message contains {@words}:
		cancel event
		send "&cWatch your language!" to player
		add 1 to {warnings::%player's uuid%}
		if {warnings::%player's uuid%} >= 2:
			kick player due to "Too many warnings"
		stop
	if player has permission "chat.color":
		stop
	replace all "&" with "" in message
	set message to "%message% :)"

on command "/plugins":
	player is not op
	cancel event
	send "&cNope."
"#;

#[test]
fn fixture_chat_filter_cancels_and_rewrites() {
    let (mut eng, mut m) = load(CHAT);
    let chat = |eng: &mut Engine, m: &mut Mock, who: &str, text: &str| {
 let c0 = ctx(m, who).with("message", Value::text(text));
 eng.fire(m, EventKind::Chat, c0)
};
    let c = chat(&mut eng, &mut m, "Bob", "what the HECK");
    assert!(c.cancelled);
    let c = chat(&mut eng, &mut m, "Bob", "&chello");
    assert!(!c.cancelled);
    assert_eq!(c.get("message"), Value::text("chello :)"));
    let c = chat(&mut eng, &mut m, "Alice", "&chello");
    assert_eq!(c.get("message"), Value::text("&chello"));
    chat(&mut eng, &mut m, "Bob", "frick");
    assert_eq!(m.take(), ["send Bob: &cWatch your language!", "send Bob: &cWatch your language!", "fx Kick Bob | Too many warnings #0"]);
    // on command with a filter
    let cmd = |eng: &mut Engine, m: &mut Mock, who: &str, label: &str| {
 let c0 = ctx(m, who).with("command", Value::text(label)).with("filter", Value::text(label));
 eng.fire(m, EventKind::Command, c0)
};
    assert!(cmd(&mut eng, &mut m, "Bob", "plugins").cancelled);
    assert!(!cmd(&mut eng, &mut m, "Alice", "plugins").cancelled);
    assert!(!cmd(&mut eng, &mut m, "Bob", "help").cancelled);
}

const BLOCKS: &str = r#"
on break of diamond ore or deepslate diamond ore:
	add 10 to {coins::%player's uuid%}
	send action bar "&b+10 coins" to player

on break:
	if event-block is bedrock or tnt:
		cancel event
		send "&cYou can't break %event-block%!"
	else if player's gamemode is creative:
		stop
	else:
		add 1 to {mined::%player's uuid%}

on place of tnt:
	player doesn't have permission "build.tnt"
	cancel event
	send "&cTNT is disabled at %x-coordinate of event-block%, %y-coordinate of event-location%."

on place:
	if type of event-block is torch:
		set block below event-block to stone

on right click on stone with stick:
	send "magic at %event-block%"
	set event-block to dirt

on left click:
	send "left"
"#;

fn block_ctx(m: &Mock, who: &str, id: &str, at: (i32, i32, i32)) -> Ctx {
    let loc = Loc { x: f64::from(at.0), y: f64::from(at.1), z: f64::from(at.2), world: "world".into(), yaw: 0.0, pitch: 0.0 };
    ctx(m, who).with("block", Value::Block(loc.clone())).with("location", Value::Loc(loc)).with("block type", Value::Item(Item::new(id, 1)))
}

#[test]
fn fixture_block_events() {
    let (mut eng, mut m) = load(BLOCKS);
    let c = {
 let c0 = block_ctx(&m, "Bob", "diamond_ore", (1, 2, 3));
 eng.fire(&mut m, EventKind::Break, c0)
};
    assert!(!c.cancelled);
    assert_eq!(eng.vars.get("coins::uuid-bob"), Value::Num(10.0));
    assert_eq!(eng.vars.get("mined::uuid-bob"), Value::Num(1.0));
    let c = {
 let c0 = block_ctx(&m, "Bob", "minecraft:bedrock", (1, 2, 3));
 eng.fire(&mut m, EventKind::Break, c0)
};
    assert!(c.cancelled);
    assert_eq!(m.take(), ["bar Bob: &b+10 coins", "send Bob: &cYou can't break bedrock!"]);
    // place: TNT refused for Bob, allowed for an operator
    assert!({
 let c0 = block_ctx(&m, "Bob", "tnt", (7, 70, 9));
 eng.fire(&mut m, EventKind::Place, c0)
}.cancelled);
    assert!(!{
 let c0 = block_ctx(&m, "Alice", "tnt", (7, 70, 9));
 eng.fire(&mut m, EventKind::Place, c0)
}.cancelled);
    assert_eq!(m.take(), ["send Bob: &cTNT is disabled at 7, 70."]);
    {
 let c0 = block_ctx(&m, "Bob", "torch", (0, 65, 0));
 eng.fire(&mut m, EventKind::Place, c0)
};
    assert_eq!(m.blocks.get(&(0, 64, 0)).map(String::as_str), Some("stone"));
    // click filters: side, block and held item
    m.players[1].items.push(Item::new("stick", 1));
    let click = |m: &Mock, side: &str, tool: &str| block_ctx(m, "Bob", "stone", (4, 5, 6)).with("filter", Value::text(side)).with("tool", Value::Item(Item::new(tool, 1)));
    {
 let c0 = click(&m, "right", "stick");
 eng.fire(&mut m, EventKind::Click, c0)
};
    {
 let c0 = click(&m, "right", "apple");
 eng.fire(&mut m, EventKind::Click, c0)
};
    {
 let c0 = click(&m, "left", "stick");
 eng.fire(&mut m, EventKind::Click, c0)
};
    assert_eq!(m.take(), ["send Bob: magic at stone", "send Bob: left"]);
    assert_eq!(m.blocks.get(&(4, 5, 6)).map(String::as_str), Some("dirt"));
}

const COUNTDOWN: &str = r#"
function announce(text: text, times: number = 1) :: number:
	loop {_times} times:
		broadcast "&e%{_text}%"
	return {_times} * 2

function countdown(p: player, n: number):
	loop {_n} times:
		send "&e%{_n} - loop-number + 1%..." to {_p}
		wait 1 second
	send "&aGo!" to {_p}

command /start [<integer = 3>]:
	trigger:
		set {_r} to announce("Starting", 2)
		send "returned %{_r}%"
		countdown(player, arg-1)
		send "countdown finished for %player%"

command /delay:
	trigger:
		send "now"
		wait 2 seconds
		send "later %player%"
		wait 10 ticks
		send "done"

every 5 seconds:
	add 1 to {ticks}

on unload:
	set {unloaded} to true
"#;

#[test]
fn fixture_wait_countdown_function_and_timer() {
    let (mut eng, mut m) = load(COUNTDOWN);
    let alice = m.p("Alice");
    eng.run_command(&mut m, "start", alice.clone(), "");
    assert_eq!(m.take(), ["broadcast: &eStarting", "broadcast: &eStarting", "send Alice: returned 4", "send Alice: &e3..."]);
    assert_eq!(eng.waiting(), 1);
    let mut seen = Vec::new();
    for _ in 0..70 {
        eng.tick(&mut m);
        for line in m.take() {
            seen.push((eng.tick, line));
        }
    }
    assert_eq!(
        seen,
        [
            (20, "send Alice: &e2...".to_owned()),
            (40, "send Alice: &e1...".to_owned()),
            (60, "send Alice: &aGo!".to_owned()),
            (60, "send Alice: countdown finished for Alice".to_owned()),
        ]
    );
    assert_eq!(eng.waiting(), 0);
    // `every 5 seconds` ran at tick 100 only once so far? 70 ticks: not yet
    assert_eq!(eng.vars.get("ticks"), Value::None);
    for _ in 0..140 {
        eng.tick(&mut m);
    }
    assert_eq!(eng.vars.get("ticks"), Value::Num(2.0));
    // waits survive in order and a reload drops them
    eng.run_command(&mut m, "delay", alice.clone(), "");
    for _ in 0..45 {
        eng.tick(&mut m);
    }
    assert_eq!(m.take(), ["send Alice: now", "send Alice: later Alice"]);
    eng.run_command(&mut m, "delay", alice, "");
    assert_eq!(eng.waiting(), 2);
    assert!(eng.unload(&mut m, "test.sk"));
    assert_eq!(eng.waiting(), 0);
    assert_eq!(eng.vars.get("unloaded"), Value::Bool(true));
    for _ in 0..100 {
        eng.tick(&mut m);
    }
    assert_eq!(m.take(), ["send Alice: now"]);
}

#[test]
fn endless_loops_are_cut_off() {
    let (mut eng, mut m) = load(
        r#"command /hang:
	trigger:
		while true is true:
			add 1 to {_i}
		send "never"

command /spin:
	trigger:
		while true is true:
			add 1 to {spins}
			wait 1 tick

function forever(n: number) :: number:
	return forever({_n} + 1)

command /recurse:
	trigger:
		send "%forever(1)%"
"#,
    );
    eng.limits.run_ops = 5000;
    eng.limits.tick_ops = 12_000;
    let alice = m.p("Alice");
    eng.run_command(&mut m, "hang", alice.clone(), "");
    let out = m.take();
    assert!(out.len() == 1 && out[0].starts_with("log: test.sk line ") && out[0].ends_with("stopped after 5000 instructions without a wait (endless loop?)"), "{out:?}");
    // a loop with a wait may run forever
    eng.run_command(&mut m, "spin", alice.clone(), "");
    for _ in 0..50 {
        eng.tick(&mut m);
    }
    assert_eq!(eng.vars.get("spins"), Value::Num(51.0));
    assert!(m.take().is_empty());
    // the tick budget stops a third heavy trigger in the same tick
    eng.run_command(&mut m, "hang", alice.clone(), "");
    eng.run_command(&mut m, "hang", alice.clone(), "");
    eng.run_command(&mut m, "hang", alice.clone(), "");
    eng.run_command(&mut m, "hang", alice.clone(), "");
    let out = m.take();
    assert_eq!(out.len(), 4);
    assert!(out[2].contains("tick's budget"), "{out:?}");
    assert!(out[3].contains("tick's budget"), "{out:?}");
    eng.tick(&mut m);
    m.take();
    eng.run_command(&mut m, "recurse", alice, "");
    let out = m.take();
    assert!(out[0].contains("too deeply"), "{out:?}");
    assert_eq!(out.last().map(String::as_str), Some("send Alice: <none>"));
}

#[test]
fn limits_on_variables_and_waits() {
    let (mut eng, mut m) = load(
        r#"command /fill:
	trigger:
		loop 50 times:
			set {v::%loop-number%} to loop-number
command /sleep:
	trigger:
		wait 1 minute
"#,
    );
    eng.limits.max_variables = 20;
    eng.limits.max_waits = 3;
    let alice = m.p("Alice");
    eng.run_command(&mut m, "fill", alice.clone(), "");
    assert_eq!(eng.vars.len(), 20);
    assert!(m.take()[0].contains("variable limit"));
    for _ in 0..5 {
        eng.run_command(&mut m, "sleep", alice.clone(), "");
    }
    assert_eq!(eng.waiting(), 3);
    assert!(eng.runtime_errors.iter().any(|l| l.contains("too many waiting")));
}

#[test]
fn timers_at_a_time_of_day() {
    let (mut eng, mut m) = load("at 18:00:\n\tbroadcast \"evening in %event-world%\"\n\nevery 2 seconds in \"world\":\n\tadd 1 to {n}\n");
    m.time = 11_000.0;
    for i in 0..200 {
        m.time = 11_000.0 + f64::from(i) * 10.0;
        eng.tick(&mut m);
    }
    assert_eq!(m.sent("evening in world"), 1);
    assert_eq!(eng.vars.get("n"), Value::Num(5.0));
}

// ---- loader ------------------------------------------------------------------------------------

#[test]
fn errors_name_the_line_and_the_rest_still_loads() {
    let src = r#"on join:
	send "hello" to player
	frobnicate the player
	set {x} to player's balance

on jion:
	send "typo"

command /ok:
	trigger:
		send "works"

on chat:
	if message contains:
		send "x"

on inventory open:
	cancel event

aliases:
	blah = stone

command /bad <unicorn>:
	trigger:
		send "x"

on quit:
	drop 1 diamond at player
	open workbench to player
	send "%nonsense expression here%"
	send "{@missing}"
	loop all blocks:
		send "x"
	wait until player is op
	else:
		send "x"
"#;
    let errs = errors(src);
    assert_eq!(
        errs,
        [
            "line 3: can't understand this effect: frobnicate the player",
            "line 6: can't understand this event: jion",
            "line 14: can't understand this condition: message contains",
            "line 17: not available on Pumpkin: inventory open (Pumpkin 0.2.2 does not send this event to plugins)",
            "line 20: not available on Pumpkin: aliases (aliases are not supported, use Minecraft ids)",
            "line 23: this command definition is wrong (name, arguments or no trigger): /bad <unicorn>",
            "line 29: not available on Pumpkin: open workbench to player (only chest menus can be opened: open chest inventory with 3 rows named \"title\" to player)",
            "line 30: can't understand this effect: send \"%nonsense expression here%\"",
            "line 31: option missing is not defined in options:",
            "line 32: not available on Pumpkin: all blocks (only blocks in radius n around a place, entities [in radius n around a place] and items in an inventory can be looped)",
            "line 35: can't understand this section: else",
        ]
    );
    // the healthy command of the same file works
    let mut eng = Engine::default();
    let loaded = eng.load("test.sk", src, &Reg);
    assert_eq!((loaded.commands.len(), loaded.triggers.len()), (1, 0));
    let mut m = Mock::new();
    let alice = m.p("Alice");
    assert!(eng.run_command(&mut m, "ok", alice, ""));
    assert_eq!(m.out, ["send Alice: works"]);
    // Thai wording
    let th = cfg::format_error(true, &eng.scripts["test.sk"].errors[0]);
    assert_eq!(th, "บรรทัด 3: ไม่เข้าใจคำสั่งนี้: frobnicate the player");
}

#[test]
fn syntax_variants_parse() {
    let lines = [
        "message \"hi\"",
        "send \"a\" and \"b\" to all players",
        "send \"x\" to console",
        "send action bar \"x\" to player",
        "send title \"a\" to player",
        "broadcast \"x\"",
        "teleport player to location at 0, 64, 0 in world \"world\"",
        "teleport player to location(0, 64, 0)",
        "teleport victim to attacker",
        "set {_loc} to location of player",
        "set player's display name to \"&c%player%\"",
        "set player's tab list name to \"x\"",
        "set player's flight mode to true",
        "set player's tool to diamond sword named \"&bExcalibur\" with lore \"a\" and \"b\"",
        "set time to 6:00",
        "set time in world \"world\" to night",
        "set weather to rain",
        "set block at location of player to stone",
        "set player's food level to 20",
        "reset player's display name",
        "clear player's inventory",
        "delete {a::*}",
        "clear {a}",
        "give player 3 diamonds",
        "give 1 diamond sword and 2 apples to player",
        "add 5 golden apples to player's inventory",
        "remove 2 diamonds from player",
        "remove all stone from player's inventory",
        "kill player",
        "kick player",
        "kick player because of \"bye\"",
        "ban player due to \"x\"",
        "unban \"Notch\" parsed as offline player",
        "op player",
        "deop player",
        "execute console command \"say hi\"",
        "console command \"/say hi\"",
        "make player execute command \"/spawn\"",
        "execute player command \"spawn\"",
        "execute command \"say %player%\"",
        "play sound \"entity.player.levelup\" with volume 1 and pitch 2 to player",
        "play sound \"block.note_block.pling\" at player",
        "play sound \"ENTITY_EXPERIENCE_ORB_PICKUP\"",
        "strike lightning at player",
        "strike lightning effect at location of player",
        "create explosion of force 3 at player",
        "create a safe explosion with force 2 at event-location",
        "apply speed 2 to player for 10 seconds",
        "apply potion of regeneration of tier 2 to player for 1 minute",
        "apply jump boost to all players",
        "remove speed from player",
        "spawn zombie at player",
        "spawn 3 cows at location of player",
        "push player upwards at speed 1",
        "push player forwards with force 2",
        "ignite player for 5 seconds",
        "extinguish player",
        "log \"%player% did it\"",
        "log \"x\" to \"audit\"",
        "wait 1 tick",
        "wait {_n} seconds",
        "set {_d} to difference between now and {last::%player%}",
        "set {_d} to time since {last::%player%}",
        "set {_t} to now formatted as \"HH:mm\"",
        "set {_r} to random integer between 1 and 6",
        "set {_r} to a random number from 0 to 1",
        "set {_r} to random element of {a::*}",
        "set {_n} to number of all players",
        "set {_n} to amount of {a::*}",
        "set {_n} to \"42\" parsed as number",
        "set {_p} to arg-1 parsed as player",
        "set {_s} to \"a\" in lowercase",
        "set {_s} to uppercase \"a\"",
        "set {_s} to last 2 characters of \"abc\"",
        "set {_b} to block below player",
        "set {_t} to type of block at player",
        "set {_w} to world of player",
        "set {_i} to player's ip",
        "set {_i} to ping of player",
        "set {_i} to player's max health",
        "set {_n} to {_a} * ({_b} + 2) / 3",
        "set {_x} to 5 minutes",
        "set {_x} to 1 minute and 30 seconds",
        "increase {_n} by 2",
        "reduce {_n} by 1",
        "replace all \"a\" with \"b\" in {_s}",
        "cancel event",
        "cancel command cooldown",
        "player is flying",
        "player is alive",
        "victim is not dead",
        "player's health is less than 5",
        "{_a} is greater than or equal to {_b}",
        "{_list::*} contains \"x\"",
        "player is holding a diamond sword",
        "player isn't holding stone",
        "event is cancelled",
        "{_x} is empty",
        "player has 3 stone in their inventory",
        "player is banned",
        "victim is a player",
        "open chest inventory with 3 rows named \"&8Shop\" to player",
        "open virtual chest inventory with size 6 named \"Big\" to player",
        "open chest to player",
        "set slot 4 of player's current inventory to diamond named \"&bBuy\"",
        "set slot 0 of player's inventory to stone",
        "set slot 2 of current inventory of player to air",
        "format slot 1 of player with emerald named \"Go\" to close then run \"spawn\"",
        "format slot 2 of player with stone to be unstealable",
        "format slot 3 of player with tnt to close",
        "close player's inventory",
        "close inventory of player",
        "give diamond sword of sharpness 5 and unbreaking 3 to player",
        "enchant player's tool with sharpness 3",
        "drop 3 diamonds at location of player",
        "equip player with diamond sword",
        "make player shoot an arrow",
        "shoot snowball from player",
        "wait until player is sneaking",
        "wait while player is flying",
        "attacker is a zombie",
        "victim is a cow",
        "send \"&#ff8800orange <#00ff00>green\"",
        "send \"line one%nl%line two\"",
        "send \"ภาษาไทย İ ẞ %player%\" to player",
        "set {ตัวแปร::%player%} to \"ค่า İ\"",
    ];
    let body: String = lines.iter().map(|l| format!("\t\t{l}\n")).collect();
    let errs = errors(&format!("command /t [<text>]:\n\ttrigger:\n{body}"));
    assert!(errs.is_empty(), "{errs:#?}");
    // events
    for ev in [
        "join", "first join", "quit", "disconnect", "chat", "command", "command \"/tp\"", "death", "death of player", "respawn", "break", "break of stone", "mine of diamond ore",
        "block place", "place of tnt or bedrock", "right click", "rightclick on stone", "left click with stick", "right click on dirt holding a diamond sword", "click", "damage",
        "drop", "drop of diamond", "pickup", "pick up", "consume", "eat of apple", "sneak toggle", "toggle sneak", "sprint toggle", "world change", "gamemode change",
        "gamemode change to creative", "level change", "bed enter", "bed leave", "teleport", "move", "walk on stone", "step on grass block", "any movement", "load", "script load",
        "server start", "unload", "script unload", "weather change", "join with priority high", "inventory click", "inventory close", "tool change", "damage of zombie",
        "damage of player", "damage of entity", "death of zombie or cow", "death of any entity", "death of a player",
    ] {
        let errs = errors(&format!("on {ev}:\n\tset {{_x}} to 1\n"));
        assert!(errs.is_empty(), "on {ev}: {errs:?}");
    }
    for head in ["every 5 seconds", "every minute", "every 1 minute in \"world\"", "every 10 ticks in world \"world_nether\"", "at 18:00", "at 7:30 in \"world\""] {
        let head = head.replace("every minute", "every 1 minute");
        let errs = errors(&format!("{head}:\n\tset {{_x}} to 1\n"));
        assert!(errs.is_empty(), "{head}: {errs:?}");
    }
}

const SHOP: &str = r#"
command /buy:
	trigger:
		if player can't afford 30:
			send "&cYou need $30, you have $%player's balance%."
			stop
		remove 30 from player's balance
		give 1 diamond to player
		send "&aBought. Left: $%balance of player%"

command /pay <player> <number>:
	trigger:
		if player has arg-2 money:
			remove arg-2 from player's balance
			add arg-2 to arg-1's balance
			send "paid %arg-2%, you %player's money%, they %arg-1's money%"
		else:
			send "too poor"

command /grant <offline player> <number>:
	trigger:
		set arg-1's balance to arg-2
		send "%arg-1% now has %arg-1's balance%"

command /fine:
	trigger:
		remove 1000 from player's balance
		send "after fine: %player's balance%"
"#;

#[test]
fn money_goes_through_the_host() {
    let (mut eng, mut m) = load(SHOP);
    m.bank = Some(HashMap::from([("Alice".to_owned(), 50.0), ("Bob".to_owned(), 5.0)]));
    let (alice, bob) = (m.p("Alice"), m.p("Bob"));
    eng.run_command(&mut m, "buy", alice.clone(), "");
    eng.run_command(&mut m, "buy", alice.clone(), "");
    eng.run_command(&mut m, "pay", alice.clone(), "Bob 15.5");
    eng.run_command(&mut m, "pay", bob.clone(), "Alice 100");
    eng.run_command(&mut m, "grant", alice.clone(), "Zed 7");
    assert_eq!(
        m.take(),
        [
            "fx Money Alice | 30 #1",
            "fx Give diamond | Alice #0",
            "send Alice: &aBought. Left: $20",
            "send Alice: &cYou need $30, you have $20.",
            "fx Money Alice | 15.5 #1",
            "fx Money Bob | 15.5 #0",
            "send Alice: paid 15.5, you 4.5, they 20.5",
            "send Bob: too poor",
            "send Alice: Zed now has 7",
        ]
    );
    // a withdrawal that does not fit changes nothing and is reported once, however often it happens
    eng.run_command(&mut m, "fine", alice.clone(), "");
    eng.run_command(&mut m, "fine", alice.clone(), "");
    assert_eq!(m.take(), ["log: test.sk line 27: Alice does not have enough money", "send Alice: after fine: 4.5", "send Alice: after fine: 4.5"]);
    // no economy plugin: the balance is not set, nobody can afford anything, changes are reported once
    m.bank = None;
    eng.load("test.sk", SHOP, &Reg);
    eng.run_command(&mut m, "buy", alice.clone(), "");
    eng.run_command(&mut m, "fine", alice.clone(), "");
    eng.run_command(&mut m, "fine", alice, "");
    assert_eq!(
        m.take(),
        ["send Alice: &cYou need $30, you have $<none>.", "log: test.sk line 27: no economy", "send Alice: after fine: <none>", "send Alice: after fine: <none>"]
    );
}

#[test]
fn menus_waits_and_radius_loops() {
    let (mut eng, mut m) = load(
        r#"command /shop:
	trigger:
		open chest inventory with 2 rows named "&8Shop" to player
		set slot 0 of player's current inventory to diamond sword of sharpness 5 named "&bBlade"
		format slot 8 of player with tnt to close then run "say bye"

on inventory click:
	if name of event-inventory is "&8Shop":
		cancel event
		if event-slot is 0:
			send "bought %event-item% with %click type% click"
			close player's inventory

on inventory close:
	send "closed %event-inventory%"

command /scan:
	trigger:
		loop blocks in radius 1 around player:
			add 1 to {_n}
		loop entities in radius 5 around player:
			send "near %loop-entity%"
		loop all entities:
			add 1 to {_e}
		send "%{_n}% blocks, %{_e}% entities"

command /hold:
	trigger:
		send "waiting"
		wait until player is sneaking
		send "sneaked"
"#,
    );
    let alice = m.p("Alice");
    eng.run_command(&mut m, "shop", alice.clone(), "");
    assert_eq!(m.take(), ["fx OpenMenu 2 | &8Shop | Alice #0", "fx SetSlot 0 | Alice | diamond sword | <none> #1", "fx SetSlot 8 | Alice | tnt | say bye #49"]);
    let click = |m: &Mock, slot: f64| ctx(m, "Alice").with("slot", Value::Num(slot)).with("inventory", Value::text("&8Shop")).with("item", Value::Item(Item::new("diamond_sword", 1))).with("click type", Value::text("left"));
    let c0 = click(&m, 0.0);
    assert!(eng.fire(&mut m, EventKind::InvClick, c0).cancelled);
    let c1 = click(&m, 5.0).with("inventory", Value::text("Other"));
    assert!(!eng.fire(&mut m, EventKind::InvClick, c1).cancelled);
    let c2 = ctx(&m, "Alice").with("inventory", Value::text("&8Shop"));
    eng.fire(&mut m, EventKind::InvClose, c2);
    assert_eq!(m.take(), ["send Alice: bought diamond sword with left click", "fx CloseMenu Alice #0", "send Alice: closed &8Shop"]);
    eng.run_command(&mut m, "scan", alice.clone(), "");
    assert_eq!(m.take(), ["send Alice: near zombie", "send Alice: near cow", "send Alice: 7 blocks, 3 entities"]);
    // wait until: looked at every tick, costs nothing while it waits
    eng.run_command(&mut m, "hold", alice, "");
    for _ in 0..50 {
        eng.tick(&mut m);
    }
    assert_eq!((m.take(), eng.waiting()), (vec!["send Alice: waiting".to_owned()], 1));
    m.players[0].sneaking = true;
    eng.tick(&mut m);
    assert_eq!((m.take(), eng.waiting()), (vec!["send Alice: sneaked".to_owned()], 0));
}

#[test]
fn damage_and_death_know_the_attacker() {
    let (mut eng, mut m) = load(
        r#"on damage:
	send "hurt by %attacker%" to victim
	if attacker is a zombie:
		cancel event

on damage of zombie or cow:
	attacker is a player
	send "you hit a %victim% for %damage%" to attacker

on death of entity:
	broadcast "%victim% died, killer %attacker%"

on death:
	broadcast "player %victim% died"
"#,
    );
    let zombie = Value::Entity(value::EntityRef { id: 7, kind: "zombie".into(), world: "world".into() });
    let hurt = |m: &Mock, victim: Value, kind: &str, who: &str, attacker: Value| {
        Ctx::default().with("player", if who == "player" { victim.clone() } else { Value::None }).with("victim", victim).with("victim type", Value::text(kind)).with("filter", Value::text(who)).with("attacker", attacker).with("damage", Value::Num(3.0)).with("world", Value::World(m.players[0].loc.world.clone()))
    };
    // a zombie hits Alice: the plain trigger runs and cancels; the entity triggers do not
    let c = hurt(&m, m.p("Alice"), "player", "player", zombie.clone());
    assert!(eng.fire(&mut m, EventKind::Damage, c).cancelled);
    // Bob hits a zombie: only `of zombie` runs; a pig is nobody's business
    let c = hurt(&m, zombie.clone(), "zombie", "entity", m.p("Bob"));
    assert!(!eng.fire(&mut m, EventKind::Damage, c).cancelled);
    let pig = Value::Entity(value::EntityRef { id: 8, kind: "pig".into(), world: "world".into() });
    let c = hurt(&m, pig.clone(), "pig", "entity", m.p("Bob"));
    eng.fire(&mut m, EventKind::Damage, c);
    let c = hurt(&m, pig, "pig", "entity", m.p("Bob"));
    eng.fire(&mut m, EventKind::Death, c);
    let c = hurt(&m, m.p("Alice"), "player", "player", Value::None);
    eng.fire(&mut m, EventKind::Death, c);
    assert_eq!(
        m.take(),
        ["send Alice: hurt by zombie", "send Bob: you hit a zombie for 3", "broadcast: pig died, killer Bob", "broadcast: Alice died, killer <none>", "broadcast: player Alice died"]
    );
    assert!(eng.wants_entities(EventKind::Damage) && eng.wants_entities(EventKind::Death));
    assert!(value::loose_eq(&zombie, &Value::text("a zombie")) && !value::loose_eq(&zombie, &Value::text("cow")));
}

#[test]
fn enchanted_items_and_script_paths() {
    let out = run("set {_i} to diamond sword of sharpness 5 and unbreaking 3\ngive {_i} to player\nenchant {_i} with sharpness 2 and mending\ngive {_i} to player");
    assert_eq!(out.len(), 2);
    let out = run("set {_i} to stone of unbreaking 3\nenchant {_i} with sharpness 2 and unbreaking 1\nsend \"%enchantments of {_i}%\"");
    assert_eq!(out, ["send Alice: sharpness 2 and unbreaking 1"]);
    assert_eq!(crate::parse::enchant_list("Sharpness 5, unbreaking and curse of vanishing").as_deref(), Some("sharpness:5,unbreaking:1,vanishing_curse:1"));
    assert_eq!(crate::parse::enchant_list("speed 2"), None);
    assert_eq!(cfg::script_path("games/-Spleef.sk").as_deref(), Some("games/spleef.sk"));
    assert_eq!(cfg::script_path("welcome").as_deref(), Some("welcome.sk"));
    assert_eq!(cfg::script_path("../x"), None);
    assert_eq!(cfg::script_path("a/b/c/d/e"), None);
    assert_eq!(cfg::script_path("/abs"), None);
}

#[test]
fn join_message_is_a_warning_not_an_error() {
    let mut eng = Engine::default();
    let loaded = eng.load("w.sk", "on join:\n\tset join message to \"&a+ %player%\"\n", &Reg);
    assert!(loaded.errors.is_empty());
    assert_eq!(cfg::format_error(false, &loaded.warnings[0]), "line 2: not available on Pumpkin: join message (the game's join/quit line cannot be replaced; your text is broadcast as an extra line)");
    let mut m = Mock::new();
    let c = {
 let c0 = ctx(&m, "Bob");
 eng.fire(&mut m, EventKind::Join, c0)
};
    assert_eq!(c.get("join message"), Value::text("&a+ Bob"));
}

#[test]
fn scripts_replace_and_share_functions() {
    let mut eng = Engine::default();
    let mut m = Mock::new();
    eng.extra_functions.insert("double".into());
    assert!(eng.load("a.sk", "command /a:\n\ttrigger:\n\t\tsend \"%double(4)%\"\n", &Reg).errors.is_empty());
    assert!(eng.load("b.sk", "function double(n: number) :: number:\n\treturn {_n} * 2\n", &Reg).errors.is_empty());
    let alice = m.p("Alice");
    eng.run_command(&mut m, "a", alice.clone(), "");
    // editing a.sk replaces its command and removes the old one
    assert!(eng.load("a.sk", "command /c:\n\ttrigger:\n\t\tsend \"new\"\n", &Reg).errors.is_empty());
    assert!(!eng.run_command(&mut m, "a", alice.clone(), ""));
    eng.run_command(&mut m, "c", alice, "");
    assert_eq!(m.out, ["send Alice: 8", "send Alice: new"]);
    assert_eq!(eng.command_labels(), ["c"]);
}

#[test]
fn names_typed_by_admins_and_scripts() {
    assert_eq!(cfg::script_name("-Welcome.sk").as_deref(), Some("welcome"));
    assert_eq!(cfg::script_name("my script").as_deref(), Some("my script"));
    assert_eq!(cfg::script_name("../config"), None);
    assert_eq!(cfg::script_name("a/b"), None);
    assert_eq!(cfg::script_name(""), None);
    assert_eq!(cfg::log_file("audit").as_deref(), Some("logs/audit.log"));
    assert_eq!(cfg::log_file("shop/sales.log").as_deref(), Some("logs/shop/sales.log"));
    assert_eq!(cfg::log_file("../../etc/passwd"), None);
    assert_eq!(cfg::log_file("/abs"), None);
    assert_eq!(cfg::log_file("a/b/c"), None);
}

#[test]
fn shipped_examples_load_clean() {
    for (name, src) in cfg::EXAMPLES {
        let mut eng = Engine::default();
        let loaded = eng.load(name, src, &Reg);
        let errs: Vec<String> = loaded.errors.iter().map(|e| cfg::format_error(false, e)).collect();
        assert!(errs.is_empty(), "{name}: {errs:#?}");
        assert!(loaded.commands.len() + loaded.triggers.len() > 0);
    }
    let c = cfg::Config { language: "xx".into(), save_interval_seconds: 0, max_instructions_per_run: 5, ..cfg::Config::default() }.cleaned();
    assert_eq!((c.language.as_str(), c.save_interval_seconds, c.max_instructions_per_run), ("th", 10, 1000));
}
