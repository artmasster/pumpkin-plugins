//! Script text → triggers, commands and functions.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::ast::*;
use crate::pattern::{Caps, Input, Pattern};
use crate::value::{self, Item, Value, norm_id, parse_number, parse_span};

/// Names only the server knows.
pub trait Names {
    fn item(&self, id: &str) -> bool;
    fn entity(&self, id: &str) -> bool;
}

/// A problem in a script file. `code` picks the sentence, `reason` says why a known feature is not available.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptError {
    pub line: u32,
    pub code: &'static str,
    pub reason: Option<&'static str>,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Unit {
    pub commands: Vec<CommandDef>,
    pub triggers: Vec<Trigger>,
    pub functions: Vec<FunctionDef>,
    pub defaults: Vec<(VarName, Expr)>,
    pub errors: Vec<ScriptError>,
    pub warnings: Vec<ScriptError>,
}

#[derive(Debug)]
struct Node {
    line: u32,
    text: String,
    children: Vec<Node>,
}

// ---- lines -----------------------------------------------------------------------------------

fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut quote = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => quote = !quote,
            b'#' if !quote => {
                if b.get(i + 1) == Some(&b'#') {
                    i += 1;
                } else if i > 0 && b[i - 1] == b'<' {
                    // `<#ff0000>` colour tag
                } else {
                    return &line[..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Single spaces outside quotes, so patterns never see runs of blanks.
fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut quote, mut space) = (false, false);
    for c in s.trim().chars() {
        if c == '"' {
            quote = !quote;
        }
        if !quote && (c == ' ' || c == '\t') {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn tree(src: &str, errors: &mut Vec<ScriptError>) -> Vec<Node> {
    // (indent, node) stack; the root has indent -1
    let mut stack: Vec<(i32, Node)> = vec![(-1, Node { line: 0, text: String::new(), children: Vec::new() })];
    let (mut tabs, mut spaces) = (false, false);
    for (i, raw) in src.lines().enumerate() {
        let line = strip_comment(raw.trim_end_matches('\r'));
        if line.trim().is_empty() {
            continue;
        }
        let lead = &line[..line.len() - line.trim_start().len()];
        let indent: i32 = lead.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
        tabs |= lead.contains('\t');
        spaces |= lead.contains(' ');
        if tabs && spaces && lead.contains('\t') && lead.contains(' ') {
            errors.push(ScriptError { line: i as u32 + 1, code: "indent", reason: None, detail: line.trim().to_owned() });
        }
        while stack.len() > 1 && stack[stack.len() - 1].0 >= indent {
            let (_, done) = stack.pop().unwrap_or_else(|| unreachable!());
            if let Some(parent) = stack.last_mut() {
                parent.1.children.push(done);
            }
        }
        stack.push((indent, Node { line: i as u32 + 1, text: squeeze(line), children: Vec::new() }));
    }
    while stack.len() > 1 {
        let (_, done) = stack.pop().unwrap_or_else(|| unreachable!());
        if let Some(parent) = stack.last_mut() {
            parent.1.children.push(done);
        }
    }
    stack.pop().map(|(_, n)| n.children).unwrap_or_default()
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') { &s[1..s.len() - 1] } else { s }
}

/// Split at top-level occurrences of the separators.
fn split_top<'a>(s: &'a str, seps: &[&str]) -> Vec<&'a str> {
    let input = Input::new(s);
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for sep in seps {
        for at in input.find_top(sep) {
            cuts.push((at, sep.len()));
        }
    }
    cuts.sort_unstable();
    let mut out = Vec::new();
    let mut from = 0;
    for (at, len) in cuts {
        if at < from {
            continue;
        }
        out.push(s[from..at].trim());
        from = at + len;
    }
    out.push(s[from..].trim());
    out
}

// ---- tables ----------------------------------------------------------------------------------

const PROPS: &[(&str, Prop)] = &[
    ("display name", Prop::DisplayName), ("displayname", Prop::DisplayName), ("nickname", Prop::DisplayName), ("nick name", Prop::DisplayName),
    ("tab list name", Prop::TabName), ("tablist name", Prop::TabName), ("player list name", Prop::TabName),
    ("name", Prop::Name), ("uuid", Prop::Uuid), ("unique id", Prop::Uuid),
    ("location", Prop::Location), ("position", Prop::Location), ("world", Prop::World),
    ("max health", Prop::MaxHealth), ("maximum health", Prop::MaxHealth), ("health", Prop::Health),
    ("food level", Prop::Food), ("hunger level", Prop::Food), ("food bar", Prop::Food), ("food", Prop::Food), ("hunger", Prop::Food),
    ("saturation", Prop::Saturation),
    ("xp level", Prop::Level), ("experience level", Prop::Level), ("exp level", Prop::Level), ("level", Prop::Level),
    ("gamemode", Prop::Gamemode), ("game mode", Prop::Gamemode),
    ("held item", Prop::Tool), ("item in hand", Prop::Tool), ("item in main hand", Prop::Tool), ("tool", Prop::Tool), ("weapon", Prop::Tool),
    ("ip address", Prop::Ip), ("ip-address", Prop::Ip), ("ip", Prop::Ip), ("ping", Prop::Ping), ("latency", Prop::Ping),
    ("x-coordinate", Prop::X), ("x coordinate", Prop::X), ("x-coord", Prop::X), ("x coord", Prop::X), ("x-position", Prop::X), ("x position", Prop::X), ("x-pos", Prop::X), ("x-loc", Prop::X),
    ("y-coordinate", Prop::Y), ("y coordinate", Prop::Y), ("y-coord", Prop::Y), ("y coord", Prop::Y), ("y-position", Prop::Y), ("y position", Prop::Y), ("y-pos", Prop::Y), ("y-loc", Prop::Y),
    ("z-coordinate", Prop::Z), ("z coordinate", Prop::Z), ("z-coord", Prop::Z), ("z coord", Prop::Z), ("z-position", Prop::Z), ("z position", Prop::Z), ("z-pos", Prop::Z), ("z-loc", Prop::Z),
    ("yaw", Prop::Yaw), ("pitch", Prop::Pitch), ("type", Prop::Type), ("inventory", Prop::Inventory),
    ("flight mode", Prop::Flight), ("fly mode", Prop::Flight), ("flying mode", Prop::Flight), ("flight state", Prop::Flight), ("flying state", Prop::Flight), ("flight", Prop::Flight),
    ("walk speed", Prop::WalkSpeed), ("walking speed", Prop::WalkSpeed), ("walkspeed", Prop::WalkSpeed),
    ("fly speed", Prop::FlySpeed), ("flying speed", Prop::FlySpeed), ("flyspeed", Prop::FlySpeed),
    ("time", Prop::Time), ("weather", Prop::Weather), ("length", Prop::Length),
    ("item amount", Prop::Amount), ("stack size", Prop::Amount),
    ("helmet", Prop::Helmet), ("chestplate", Prop::Chestplate), ("chest plate", Prop::Chestplate), ("leggings", Prop::Leggings), ("boots", Prop::Boots),
    ("fire ticks", Prop::FireTicks), ("burning time", Prop::FireTicks),
    ("enchantments", Prop::Enchants), ("enchants", Prop::Enchants),
    ("balance", Prop::Balance), ("money", Prop::Balance), ("account", Prop::Balance),
];

/// Skript names of status effects → Minecraft ids.
pub const POTIONS: &[(&str, &str)] = &[
    ("speed", "speed"), ("swiftness", "speed"), ("slowness", "slowness"), ("slow", "slowness"), ("haste", "haste"), ("mining fatigue", "mining_fatigue"),
    ("fatigue", "mining_fatigue"), ("strength", "strength"), ("instant health", "instant_health"), ("healing", "instant_health"),
    ("instant damage", "instant_damage"), ("harming", "instant_damage"), ("jump boost", "jump_boost"), ("jump", "jump_boost"), ("leaping", "jump_boost"),
    ("nausea", "nausea"), ("confusion", "nausea"), ("regeneration", "regeneration"), ("regen", "regeneration"), ("resistance", "resistance"),
    ("damage resistance", "resistance"), ("fire resistance", "fire_resistance"), ("water breathing", "water_breathing"), ("invisibility", "invisibility"),
    ("blindness", "blindness"), ("night vision", "night_vision"), ("hunger", "hunger"), ("weakness", "weakness"), ("poison", "poison"), ("wither", "wither"),
    ("health boost", "health_boost"), ("absorption", "absorption"), ("saturation", "saturation"), ("glowing", "glowing"), ("levitation", "levitation"),
    ("luck", "luck"), ("bad luck", "unluck"), ("unluck", "unluck"), ("slow falling", "slow_falling"), ("conduit power", "conduit_power"),
    ("dolphins grace", "dolphins_grace"), ("bad omen", "bad_omen"), ("hero of the village", "hero_of_the_village"), ("darkness", "darkness"),
];

pub fn potion_id(name: &str) -> Option<&'static str> {
    let n = name.trim().to_ascii_lowercase().replace(['_', '\''], " ");
    let n = n.strip_suffix(" potion").unwrap_or(&n).trim().to_owned();
    POTIONS.iter().find(|(a, _)| *a == n).map(|(_, id)| *id)
}

/// Skript names of enchantments → Minecraft ids.
pub const ENCHANTS: &[(&str, &str)] = &[
    ("aqua affinity", "aqua_affinity"), ("bane of arthropods", "bane_of_arthropods"), ("curse of binding", "binding_curse"), ("binding curse", "binding_curse"),
    ("blast protection", "blast_protection"), ("breach", "breach"), ("channeling", "channeling"), ("density", "density"), ("depth strider", "depth_strider"),
    ("efficiency", "efficiency"), ("feather falling", "feather_falling"), ("fire aspect", "fire_aspect"), ("fire protection", "fire_protection"), ("flame", "flame"),
    ("fortune", "fortune"), ("frost walker", "frost_walker"), ("impaling", "impaling"), ("infinity", "infinity"), ("knockback", "knockback"), ("looting", "looting"),
    ("loyalty", "loyalty"), ("luck of the sea", "luck_of_the_sea"), ("lunge", "lunge"), ("lure", "lure"), ("mending", "mending"), ("multishot", "multishot"),
    ("piercing", "piercing"), ("power", "power"), ("projectile protection", "projectile_protection"), ("protection", "protection"), ("punch", "punch"),
    ("quick charge", "quick_charge"), ("respiration", "respiration"), ("riptide", "riptide"), ("sharpness", "sharpness"), ("silk touch", "silk_touch"), ("smite", "smite"),
    ("soul speed", "soul_speed"), ("sweeping edge", "sweeping_edge"), ("sweeping", "sweeping_edge"), ("swift sneak", "swift_sneak"), ("thorns", "thorns"),
    ("unbreaking", "unbreaking"), ("curse of vanishing", "vanishing_curse"), ("vanishing curse", "vanishing_curse"), ("wind burst", "wind_burst"),
];

/// `sharpness 5 and unbreaking 3` → `sharpness:5,unbreaking:3`
pub fn enchant_list(text: &str) -> Option<String> {
    let mut out = Vec::new();
    for part in split_top(text, &[", and ", ",", " and "]) {
        let lower = part.trim().to_ascii_lowercase().replace('_', " ");
        let (name, level) = match lower.rsplit_once(' ') {
            Some((n, l)) if l.parse::<u32>().is_ok() => (n.to_owned(), l.parse::<u32>().ok()?),
            _ => (lower.clone(), 1),
        };
        let id = ENCHANTS.iter().find(|(a, _)| *a == name).map(|(_, id)| *id)?;
        out.push(format!("{id}:{}", level.clamp(1, 255)));
    }
    (!out.is_empty()).then(|| out.join(","))
}

const ITEM_ALIASES: &[(&str, &str)] = &[
    ("grass", "grass_block"), ("log", "oak_log"), ("wood", "oak_planks"), ("plank", "oak_planks"), ("wooden_plank", "oak_planks"), ("steak", "cooked_beef"),
    ("cobble", "cobblestone"), ("lapis", "lapis_lazuli"), ("wool", "white_wool"), ("bed", "red_bed"), ("sign", "oak_sign"), ("door", "oak_door"),
    ("boat", "oak_boat"), ("sapling", "oak_sapling"), ("leaves", "oak_leaves"), ("workbench", "crafting_table"), ("redstone_dust", "redstone"),
    ("bottle_of_enchanting", "experience_bottle"), ("xp_bottle", "experience_bottle"), ("notch_apple", "enchanted_golden_apple"), ("eye_of_ender", "ender_eye"),
    ("bottle", "glass_bottle"), ("firework", "firework_rocket"), ("snow_ball", "snowball"), ("slimeball", "slime_ball"), ("melon", "melon_slice"),
    ("chest_plate", "chestplate"), ("fence", "oak_fence"), ("carpet", "white_carpet"), ("glass_pane", "glass_pane"), ("trapdoor", "oak_trapdoor"),
    ("button", "stone_button"), ("pressure_plate", "stone_pressure_plate"), ("gold_sword", "golden_sword"), ("gold_pickaxe", "golden_pickaxe"),
    ("gold_axe", "golden_axe"), ("gold_shovel", "golden_shovel"), ("gold_hoe", "golden_hoe"), ("gold_helmet", "golden_helmet"),
    ("gold_chestplate", "golden_chestplate"), ("gold_leggings", "golden_leggings"), ("gold_boots", "golden_boots"), ("wood_sword", "wooden_sword"),
    ("wood_pickaxe", "wooden_pickaxe"), ("wood_axe", "wooden_axe"), ("wood_shovel", "wooden_shovel"), ("wood_hoe", "wooden_hoe"), ("spade", "iron_shovel"),
];

const WORDS: &[&str] = &[
    "survival", "creative", "adventure", "spectator", "clear", "sun", "sunny", "rain", "rainy", "raining", "thunder", "thunderstorm", "storm", "thundering",
];

const IS: &str = "(is|are|2:isn't|2:is not|2:aren't|2:are not)";
const DONT: &str = "(2:doesn't|2:does not|2:do not|2:don't)";

#[derive(Clone, Copy, PartialEq, Debug)]
enum C {
    IsSet,
    Exists,
    HasPerm,
    Is(Check),
    Offline,
    Dead,
    InWorld,
    HasItem,
    Afford,
    Chance,
    Contains,
    Starts,
    Ends,
    Matches,
    Between,
    Empty,
    Cancelled,
    Holding,
    Cmp(CmpOp),
}

fn cond_patterns() -> &'static Vec<(Pattern, C)> {
    static P: OnceLock<Vec<(Pattern, C)>> = OnceLock::new();
    P.get_or_init(|| {
        let mut v: Vec<(String, C)> = vec![
            (format!("%a% {IS} set"), C::IsSet),
            ("%a% exist[s]".into(), C::Exists),
            (format!("%a% {DONT} exist"), C::Exists),
            ("%a% (has|have) [the] permission[s] %b%".into(), C::HasPerm),
            (format!("%a% {DONT} have [the] permission[s] %b%"), C::HasPerm),
            (format!("%a% {IS} [a[n]] (op|ops|operator|operators)"), C::Is(Check::IsOp)),
            (format!("%a% {IS} online"), C::Is(Check::Online)),
            (format!("%a% {IS} offline"), C::Offline),
            (format!("%a% {IS} sneaking"), C::Is(Check::Sneaking)),
            (format!("%a% {IS} flying"), C::Is(Check::Flying)),
            (format!("%a% {IS} sprinting"), C::Is(Check::Sprinting)),
            (format!("%a% {IS} alive"), C::Is(Check::Alive)),
            (format!("%a% {IS} dead"), C::Dead),
            (format!("%a% {IS} (burning|on fire|ignited)"), C::Is(Check::Burning)),
            (format!("%a% {IS} on [the] ground"), C::Is(Check::OnGround)),
            (format!("%a% {IS} swimming"), C::Is(Check::Swimming)),
            (format!("%a% {IS} banned"), C::Is(Check::Banned)),
            (format!("%a% {IS} in [the] world %b%"), C::InWorld),
            (format!("%a% {IS} a player"), C::Is(Check::IsPlayer)),
            (format!("[the] event {IS} cancel[l]ed"), C::Cancelled),
            (format!("%a% {IS} between %b% and %c%"), C::Between),
            (format!("%a% {IS} empty"), C::Empty),
            (format!("%a% {IS} holding %b%"), C::Holding),
            ("%a% (has|have) [at least] %b% (money|balance)".into(), C::Afford),
            (format!("%a% {DONT} have [at least] %b% (money|balance)"), C::Afford),
            ("%a% can afford %b%".into(), C::Afford),
            ("%a% (2:can't|2:cannot|2:can not) afford %b%".into(), C::Afford),
            ("%a% (has|have) %b% in [(his|her|their|its)] inventory".into(), C::HasItem),
            (format!("%a% {DONT} have %b% in [(his|her|their|its)] inventory"), C::HasItem),
            ("chance of %a%[\\%]".into(), C::Chance),
            ("%a% contain[s] %b%".into(), C::Contains),
            (format!("%a% {DONT} contain %b%"), C::Contains),
            ("%a% starts with %b%".into(), C::Starts),
            (format!("%a% {DONT} start with %b%"), C::Starts),
            ("%a% ends with %b%".into(), C::Ends),
            (format!("%a% {DONT} end with %b%"), C::Ends),
            ("%a% matches %b%".into(), C::Matches),
            (format!("%a% {DONT} match %b%"), C::Matches),
            ("%a% (is|are) (greater|more|higher|bigger|larger) than or equal to %b%".into(), C::Cmp(CmpOp::Ge)),
            ("%a% (is|are) (less|smaller|lower|fewer) than or equal to %b%".into(), C::Cmp(CmpOp::Le)),
            ("%a% (is|are) not (greater|more|higher|bigger|larger) than %b%".into(), C::Cmp(CmpOp::Le)),
            ("%a% (is|are) not (less|smaller|lower|fewer) than %b%".into(), C::Cmp(CmpOp::Ge)),
            ("%a% (is|are) (greater|more|higher|bigger|larger) than %b%".into(), C::Cmp(CmpOp::Gt)),
            ("%a% (is|are) (less|smaller|lower|fewer) than %b%".into(), C::Cmp(CmpOp::Lt)),
            ("%a% (is|are) above %b%".into(), C::Cmp(CmpOp::Gt)),
            ("%a% (is|are) below %b%".into(), C::Cmp(CmpOp::Lt)),
            ("%a% (is|are) at least %b%".into(), C::Cmp(CmpOp::Ge)),
            ("%a% (is|are) at most %b%".into(), C::Cmp(CmpOp::Le)),
            ("%a% >= %b%".into(), C::Cmp(CmpOp::Ge)),
            ("%a% <= %b%".into(), C::Cmp(CmpOp::Le)),
            ("%a% != %b%".into(), C::Cmp(CmpOp::Ne)),
            ("%a% > %b%".into(), C::Cmp(CmpOp::Gt)),
            ("%a% < %b%".into(), C::Cmp(CmpOp::Lt)),
            ("%a% (==|=) %b%".into(), C::Cmp(CmpOp::Eq)),
            ("%a% (isn't|is not|aren't|are not) [equal to] %b%".into(), C::Cmp(CmpOp::Ne)),
            ("%a% (is|are) [equal to] %b%".into(), C::Cmp(CmpOp::Eq)),
            ("%a% (has|have) %b%".into(), C::HasItem),
            (format!("%a% {DONT} have %b%"), C::HasItem),
        ];
        v.drain(..).map(|(p, c)| (Pattern::new(&p), c)).collect()
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum X {
    Op(ExprOp),
    LocationAt,
    Rand(bool),
    Parsed,
    SpanOf,
    BlockRel(i32),
    Case(ExprOp),
    Replace,
    ReplaceRev,
}

fn expr_patterns() -> &'static Vec<(Pattern, X)> {
    static P: OnceLock<Vec<(Pattern, X)>> = OnceLock::new();
    P.get_or_init(|| {
        let v: Vec<(&str, X)> = vec![
            ("[the] location [at] %a%, %b%, %c% [in] [[the] world] %d%", X::LocationAt),
            ("[the] location [at] %a%, %b%, %c%", X::LocationAt),
            ("[the] block at %a%", X::Op(ExprOp::BlockAt)),
            ("[the] block (below|under|beneath) %a%", X::BlockRel(-1)),
            ("[the] block above %a%", X::BlockRel(1)),
            ("[the] distance between %a% and %b%", X::Op(ExprOp::Distance)),
            ("[the] difference between %a% and %b%", X::Op(ExprOp::Difference)),
            ("[the] time since %a%", X::Op(ExprOp::TimeSince)),
            ("%a% formatted [human-readable] [(with|as) %b%]", X::Op(ExprOp::Formatted)),
            ("[a] random integer (from|between) %a% (to|and) %b%", X::Rand(true)),
            ("[a] random number (from|between) %a% (to|and) %b%", X::Rand(false)),
            ("[a] random element [out] of %a%", X::Op(ExprOp::RandElem)),
            ("[the] first element [out] of %a%", X::Op(ExprOp::FirstElem)),
            ("[the] last element [out] of %a%", X::Op(ExprOp::LastElem)),
            ("[the] (size|amount|number) of %a%", X::Op(ExprOp::Size)),
            ("all [[of] the] players in [[the] world] %a%", X::Op(ExprOp::PlayersIn)),
            ("%a% parsed as [a[n]] %b%", X::Parsed),
            ("%a% in (upper|1:lower)[ ]case", X::Case(ExprOp::Upper)),
            ("(upper|1:lower)[ ]case %a%", X::Case(ExprOp::Upper)),
            ("capitali(s|z)ed %a%", X::Op(ExprOp::Capital)),
            ("[the] (subtext|substring) of %a% (from|between) [(index|character|char)] %b% (to|and) [(index|character|char)] %c%", X::Op(ExprOp::Subtext)),
            ("[the] first %b% (characters|chars) of %a%", X::Op(ExprOp::FirstChars)),
            ("[the] last %b% (characters|chars) of %a%", X::Op(ExprOp::LastChars)),
            ("(join|concat) %a% (with|using|by) [[the] delimiter] %b%", X::Op(ExprOp::Join)),
            ("(join|concat) %a%", X::Op(ExprOp::Join)),
            ("split %a% (at|using|by) [[the] delimiter] %b%", X::Op(ExprOp::Split)),
            ("%a% split (at|using|by) [[the] delimiter] %b%", X::Op(ExprOp::Split)),
            ("replace [(all|every)] %a% with %b% in %c%", X::Replace),
            ("replace [(all|every)] %a% in %c% with %b%", X::ReplaceRev),
            ("[the] index of %b% in %a%", X::Op(ExprOp::IndexOf)),
            ("%a% of %b%", X::Op(ExprOp::ItemOf)),
            ("%a% named %b%", X::Op(ExprOp::Named)),
            ("%a% with lore %b%", X::Op(ExprOp::WithLore)),
            ("[the] target[ed] block [of %a%]", X::Op(ExprOp::TargetBlock)),
            ("%a%'s target[ed] block", X::Op(ExprOp::TargetBlock)),
            ("(uncolo[u]red|stripped) %a%", X::Op(ExprOp::Uncolored)),
            ("(colo[u]red|formatted) %a%", X::Op(ExprOp::Lower)),
            ("[the] world [named] %a%", X::Op(ExprOp::WorldOf)),
            ("%a% (1:tick|2:second|4:minute|8:hour|16:day|32:week)[s]", X::SpanOf),
        ];
        v.into_iter().map(|(p, x)| (Pattern::new(p), x)).collect()
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum E {
    ActionBar,
    Title,
    Broadcast,
    Send,
    Teleport,
    ConsoleCmd,
    PlayerCmd,
    PlayerCmdRev,
    Cancel,
    Uncancel,
    CancelCooldown,
    Give,
    GiveRev,
    Fx1(Fx),
    Kick,
    Ban,
    Sound,
    Lightning,
    Explosion,
    Potion,
    Spawn,
    Push,
    Ignite,
    Log,
    Replace,
    ReplaceRev,
    Change(Mode),
    ChangeRev(Mode),
    Damage,
    Drop,
    OpenMenu,
    CloseMenu,
    SetSlot,
    FormatSlot,
    Enchant,
    Equip,
    Shoot,
    ShootRev,
    Unsupported(&'static str),
}

fn effect_patterns() -> &'static Vec<(Pattern, E)> {
    static P: OnceLock<Vec<(Pattern, E)>> = OnceLock::new();
    const WHY: &str = "(due to|because of|on account of|by reason of|with reason|for)";
    P.get_or_init(|| {
        let v: Vec<(String, E)> = vec![
            ("send [the|an] action[ ]bar [with text] %a% [to %b%]".into(), E::ActionBar),
            ("send [the] title %a% [with subtitle %b%] [to %c%] [for %d%] [with fade[( |-)]in %e%] [[and] [with] fade[( |-)]out %f%]".into(), E::Title),
            ("broadcast %a%".into(), E::Broadcast),
            ("(send|message) %a% [to %b%]".into(), E::Send),
            ("teleport %a% to %b%".into(), E::Teleport),
            ("[execute] [the] console command %a%".into(), E::ConsoleCmd),
            ("(make|let) [the] console execute [[the] command] %a%".into(), E::ConsoleCmd),
            ("(make|let|force) %b% [to] execute [[the] command] %a%".into(), E::PlayerCmd),
            ("[execute] [the] command %a% (by|from|as) %b%".into(), E::PlayerCmdRev),
            ("execute [the] %b% command %a%".into(), E::PlayerCmd),
            ("[execute] [the] command %a%".into(), E::ConsoleCmd),
            ("cancel [the] event".into(), E::Cancel),
            ("uncancel [the] event".into(), E::Uncancel),
            ("cancel [the] [current] [command] cooldown".into(), E::CancelCooldown),
            ("give %a% to %b%".into(), E::Give),
            ("kill %a%".into(), E::Fx1(Fx::Kill)),
            ("heal %a% [by %b%]".into(), E::Fx1(Fx::Heal)),
            ("feed %a% [by %b%]".into(), E::Fx1(Fx::Feed)),
            ("damage %a% by %b%".into(), E::Damage),
            (format!("kick %a% [{WHY} %b%]"), E::Kick),
            (format!("ban %a% [{WHY} %b%]"), E::Ban),
            ("unban %a%".into(), E::Fx1(Fx::Unban)),
            ("op %a%".into(), E::Fx1(Fx::Op)),
            ("de[-]op %a%".into(), E::Fx1(Fx::Deop)),
            ("play [the] sound %a% [with volume %b%] [[and] [with] pitch %c%] [1:(to|for) %d%|2:at %e%]".into(), E::Sound),
            ("strike lightning [1:effect] at %a%".into(), E::Lightning),
            ("create [a[n]] [1:safe] explosion (of|with) (force|strength|power) %a% at %b% [2:with fire]".into(), E::Explosion),
            ("apply [potion of] %a% [[of] tier %b%] to %c% [for %d%]".into(), E::Potion),
            ("spawn %a% (at|on|above) %b%".into(), E::Spawn),
            ("push %a% (1:up[wards]|2:down[wards]|3:north[wards]|4:south[wards]|5:east[wards]|6:west[wards]|7:forward[s]|8:backward[s]) [(at|with) (speed|velocity|force) %b%]".into(), E::Push),
            ("(ignite|set fire to) %a% [for %b%]".into(), E::Ignite),
            ("extinguish %a%".into(), E::Fx1(Fx::Extinguish)),
            ("log %a% [(to|in) [[the] file] %b%]".into(), E::Log),
            ("replace [(all|every)] %a% with %b% in %c%".into(), E::Replace),
            ("replace [(all|every)] %a% in %c% with %b%".into(), E::ReplaceRev),
            ("drop %a% at %b%".into(), E::Drop),
            ("open [a] [virtual] (chest|inventory) [inventory] with size %a% [named %b%] to %c%".into(), E::OpenMenu),
            ("open [a] [virtual] (chest|inventory) [inventory] [with %a% row[s]] [named %b%] to %c%".into(), E::OpenMenu),
            ("close [the] inventory (of|for) %a%".into(), E::CloseMenu),
            ("close %a%'s inventory".into(), E::CloseMenu),
            ("set slot %a% of %b%'s [1:current|1:open] inventory to %c%".into(), E::SetSlot),
            ("set slot %a% of [the] [1:current|1:open] inventory of %b% to %c%".into(), E::SetSlot),
            ("format slot %a% of %b% with %c% to (48:close then run %d%|32:run %d%|16:close|be unstealable)".into(), E::FormatSlot),
            ("enchant %a% with %b%".into(), E::Enchant),
            ("equip %a% with %b%".into(), E::Equip),
            ("make %a% shoot [a[n]] %b%".into(), E::Shoot),
            ("shoot [a[n]] %b% from %a%".into(), E::ShootRev),
            ("(open|close) %a%".into(), E::Unsupported("gui")),
            ("disenchant %a%".into(), E::Unsupported("enchant")),
            ("set %a% to %b%".into(), E::Change(Mode::Set)),
            ("remove (all|every) %a% from %b%".into(), E::Change(Mode::RemoveAll)),
            ("(remove|subtract) %a% from %b%".into(), E::Change(Mode::Remove)),
            ("(reduce|decrease) %b% by %a%".into(), E::ChangeRev(Mode::Remove)),
            ("add %a% to %b%".into(), E::Change(Mode::Add)),
            ("increase %b% by %a%".into(), E::ChangeRev(Mode::Add)),
            ("(delete|clear) %a%".into(), E::Change(Mode::Delete)),
            ("reset %a%".into(), E::Change(Mode::Reset)),
            ("give %b% %a%".into(), E::GiveRev),
        ];
        v.into_iter().map(|(p, e)| (Pattern::new(&p), e)).collect()
    })
}

fn event_patterns() -> &'static Vec<(Pattern, EventKind, u32)> {
    static P: OnceLock<Vec<(Pattern, EventKind, u32)>> = OnceLock::new();
    P.get_or_init(|| {
        use EventKind::*;
        // flag 1: %a% is an item list; 2: %a% is a text; 4: timespan; 8: time of day
        let v: Vec<(&str, EventKind, u32)> = vec![
            ("first (join|login)", FirstJoin, 0),
            ("[player] (join|joining|login|logging in|log in|connect)", Join, 0),
            ("[player] (quit|quitting|disconnect|leave|leaving|log out|logout|logging out)", Quit, 0),
            ("chat", Chat, 0),
            ("command [%a%]", Command, 2),
            ("death [of %a%]", Death, 16),
            ("[player] respawn[ing]", Respawn, 0),
            ("[block] (break[ing]|mine|mining) [[of] %a%]", Break, 1),
            ("[block] (place|placing|build[ing]) [[of] %a%]", Place, 1),
            ("(1:right|2:left)[( |-)][mouse[( |-)]]click[ing] [on %a%] [(with|using|holding) %b%]", Click, 1),
            ("[mouse[( |-)]]click[ing] [on %a%] [(with|using|holding) %b%]", Click, 1),
            ("damage [of %a%]", Damage, 16),
            ("inventory click", InvClick, 0),
            ("inventory close", InvClose, 0),
            ("(tool|held item|hotbar slot|item held) change", ToolChange, 0),
            ("[player] drop[ping] [[of] %a%]", Drop, 1),
            ("[player] (pick[ ]up|picking up) [[of] %a%]", Pickup, 1),
            ("[player] (consume|consuming|eat|eating|drink|drinking) [[of] %a%]", Consume, 1),
            ("[player] (toggle sneak|toggling sneak|sneak toggle|sneak toggling|sneak[ing])", Sneak, 0),
            ("[player] (toggle sprint|toggling sprint|sprint toggle|sprint toggling|sprint[ing])", Sprint, 0),
            ("[player] (toggle flight|toggling flight|flight toggle|flight toggling|toggle fly|fly toggle)", Flight, 0),
            ("[player] world chang(e|ing)", WorldChange, 0),
            ("[player] chang(e|ing) [of] world", WorldChange, 0),
            ("game[ ]mode change [to %a%]", GamemodeChange, 2),
            ("[player] level (change|up|down)", LevelChange, 0),
            ("[player] bed enter[ing]", BedEnter, 0),
            ("[player] enter[ing] [a] bed", BedEnter, 0),
            ("[player] bed leav(e|ing)", BedLeave, 0),
            ("[player] leav(e|ing) [a] bed", BedLeave, 0),
            ("[player] teleport[ing]", Teleport, 0),
            ("[player] (move|moving|walk|walking|step|stepping) [on %a%]", Move, 1),
            ("any move[ment]", Move, 0),
            ("weather change [to %a%]", WeatherChange, 2),
            ("[server|skript|script] (start|load|enable)", Load, 0),
            ("[server|skript|script] (stop|unload|disable)", Unload, 0),
            ("every %a% [in [[the] world] %b%]", Every, 4),
            ("at %a% [in [[the] world] %b%]", At, 8),
        ];
        v.into_iter().map(|(p, k, f)| (Pattern::new(p), k, f)).collect()
    })
}

const UNSUPPORTED_EVENTS: &[&str] = &[
    "inventory", "craft", "fish", "explo", "spawn", "portal", "sign", "furnace", "smelt", "enchant", "projectile", "shoot", "vehicle", "region", "kick",
    "burn", "grow", "piston", "redstone", "tame", "target", "bucket", "tool change", "held", "swap", "resource pack", "exp", "experience", "heal", "hunger",
    "food", "chunk", "death of", "damage of", "ignit", "combust", "entity", "physics", "book", "armor", "armour", "jump", "connect", "ping", "tab",
];

// ---- parser ----------------------------------------------------------------------------------

struct LoopCtx {
    /// A `loop` (has a runtime frame) or a `while`.
    counted: bool,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

pub struct Parser<'a> {
    names: &'a dyn Names,
    script: String,
    functions: HashSet<String>,
    memo: HashMap<String, Option<Expr>>,
    /// Why the innermost failure happened, when it was a known but unavailable feature.
    note: Option<&'static str>,
    in_command: bool,
    in_function: bool,
    loops: Vec<LoopCtx>,
    ops: Vec<Op>,
    lines: Vec<u32>,
    line: u32,
    errors: Vec<ScriptError>,
    warnings: Vec<ScriptError>,
    depth: u32,
}

/// Names of the functions a file defines (so other files can call them whatever the load order).
pub fn function_names(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let l = l.strip_prefix("local ").unwrap_or(l);
            let rest = l.strip_prefix("function ")?;
            Some(rest.split('(').next()?.trim().to_ascii_lowercase())
        })
        .collect()
}

pub fn parse_script(name: &str, src: &str, names: &dyn Names, known_functions: &HashSet<String>) -> Unit {
    let mut unit = Unit::default();
    let mut nodes = tree(src, &mut unit.errors);
    let mut functions = known_functions.clone();
    functions.extend(function_names(src));

    // options first: they are plain text substitution
    let mut options: HashMap<String, String> = HashMap::new();
    for n in &nodes {
        if n.text.eq_ignore_ascii_case("options:") {
            for c in &n.children {
                match c.text.split_once(':') {
                    Some((k, v)) => {
                        options.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
                    }
                    None => unit.errors.push(err(c.line, "entry", None, &c.text)),
                }
            }
        }
    }
    fn substitute(n: &mut Node, options: &HashMap<String, String>, errors: &mut Vec<ScriptError>) {
        let mut guard = 0;
        while let Some(start) = n.text.find("{@") {
            let Some(len) = n.text[start..].find('}') else { break };
            let key = n.text[start + 2..start + len].trim().to_ascii_lowercase();
            guard += 1;
            match options.get(&key) {
                Some(v) if guard < 50 => n.text.replace_range(start..=start + len, v),
                _ => {
                    errors.push(err(n.line, "option", None, &key));
                    n.text.replace_range(start..=start + len, "<none>");
                }
            }
        }
        for c in &mut n.children {
            substitute(c, options, errors);
        }
    }
    for n in &mut nodes {
        if !n.text.eq_ignore_ascii_case("options:") {
            substitute(n, &options, &mut unit.errors);
        }
    }

    let mut p = Parser {
        names,
        script: name.to_owned(),
        functions,
        memo: HashMap::new(),
        note: None,
        in_command: false,
        in_function: false,
        loops: Vec::new(),
        ops: Vec::new(),
        lines: Vec::new(),
        line: 0,
        errors: Vec::new(),
        warnings: Vec::new(),
        depth: 0,
    };
    for n in &nodes {
        p.structure(n, &mut unit);
    }
    unit.errors.append(&mut p.errors);
    unit.warnings.append(&mut p.warnings);
    unit.errors.sort_by_key(|e| e.line);
    unit
}

fn err(line: u32, code: &'static str, reason: Option<&'static str>, detail: &str) -> ScriptError {
    let mut detail: String = detail.chars().take(120).collect();
    if detail.ends_with(':') {
        detail.pop();
    }
    ScriptError { line, code, reason, detail }
}

impl Parser<'_> {
    fn fail(&mut self, line: u32, code: &'static str, detail: &str) {
        let e = match self.note.take() {
            Some(reason) => err(line, "unsupported", Some(reason), detail),
            None => err(line, code, None, detail),
        };
        self.errors.push(e);
    }

    // ---- top level ---------------------------------------------------------------------------

    fn structure(&mut self, n: &Node, unit: &mut Unit) {
        let Some(head) = n.text.strip_suffix(':') else {
            self.fail(n.line, "structure", &n.text);
            return;
        };
        let head = head.trim();
        let lower = head.to_ascii_lowercase();
        self.note = None;
        if lower == "options" {
        } else if lower == "aliases" {
            self.note = Some("aliases");
            self.fail(n.line, "structure", head);
        } else if lower == "variables" {
            for c in &n.children {
                self.memo.clear();
                let parsed = c.text.split_once('=').and_then(|(k, v)| match self.expr(k.trim()) {
                    Some(Expr::Var(name)) if !name.local && !name.list => self.expr(v.trim()).map(|v| (name, v)),
                    _ => None,
                });
                match parsed {
                    Some(d) => unit.defaults.push(d),
                    None => self.fail(c.line, "entry", &c.text),
                }
            }
        } else if let Some(rest) = lower.strip_prefix("command ") {
            let rest = &head[head.len() - rest.len()..];
            if let Some(cmd) = self.command(n, rest) {
                unit.commands.push(cmd);
            }
        } else if lower.starts_with("function ") || lower.starts_with("local function ") {
            if let Some(f) = self.function(n, head) {
                unit.functions.push(f);
            }
        } else {
            let ev = lower.strip_prefix("on ").unwrap_or(&lower);
            let ev = &head[head.len() - ev.len()..];
            if let Some(t) = self.event(n, ev) {
                unit.triggers.push(t);
            }
        }
    }

    fn body(&mut self, nodes: &[Node]) -> Option<Arc<Code>> {
        let before = self.errors.len();
        self.ops.clear();
        self.lines.clear();
        self.loops.clear();
        let mut exits = Vec::new();
        self.block(nodes, &mut exits);
        let end = self.ops.len();
        self.emit(Op::Stop);
        for at in exits {
            self.patch(at, end);
        }
        (self.errors.len() == before).then(|| Arc::new(Code { script: self.script.clone(), ops: std::mem::take(&mut self.ops), lines: std::mem::take(&mut self.lines) }))
    }

    fn items(&mut self, raw: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for part in split_top(raw, &[",", " or ", " and "]) {
            let part = part.strip_prefix("any ").unwrap_or(part);
            out.push(self.item(part)?.id);
        }
        Some(out)
    }

    fn event(&mut self, n: &Node, head: &str) -> Option<Trigger> {
        // `with priority high` has no meaning here
        let head = match head.to_ascii_lowercase().find(" with priority ") {
            Some(i) => &head[..i],
            None => head,
        };
        let input = Input::new(head);
        let mut found: Option<(EventKind, Vec<String>, Vec<String>, Option<String>, f64, Option<String>)> = None;
        for (pat, kind, flag) in event_patterns() {
            let mut hit = None;
            pat.matches(&input, &mut |c: &Caps| {
                hit = Some((c.get(0).map(str::to_owned), c.get(1).map(str::to_owned), c.mark));
                true
            });
            let Some((a, b, mark)) = hit else { continue };
            let (mut items, mut holding, mut filter, mut number, mut world) = (Vec::new(), Vec::new(), None, 0.0, None);
            let ok = match flag {
                1 => {
                    let first = a.as_deref().map_or(Some(Vec::new()), |a| self.items(a));
                    let second = b.as_deref().map_or(Some(Vec::new()), |b| self.items(b));
                    if *kind == EventKind::Click {
                        filter = match mark {
                            1 => Some("right".to_owned()),
                            2 => Some("left".to_owned()),
                            _ => None,
                        };
                    }
                    match (first, second) {
                        (Some(f), Some(s)) => {
                            items = f;
                            holding = s;
                            true
                        }
                        _ => false,
                    }
                }
                2 => {
                    filter = a.map(|a| unquote(&a).trim_start_matches('/').to_ascii_lowercase());
                    true
                }
                // who is hurt or dies: nothing = players (as before), `entity` = anything, or entity types
                16 => match a.as_deref().map(|a| a.trim().to_ascii_lowercase()) {
                    None => {
                        filter = Some("player".to_owned());
                        true
                    }
                    Some(a) if matches!(a.as_str(), "player" | "a player" | "players") => {
                        filter = Some("player".to_owned());
                        true
                    }
                    Some(a) if matches!(a.as_str(), "entity" | "an entity" | "any entity" | "entities" | "living entity" | "a living entity" | "mob" | "a mob" | "anything") => true,
                    Some(a) => {
                        let mut ok = true;
                        for part in split_top(&a, &[",", " or ", " and "]) {
                            let part = part.strip_prefix("an ").or_else(|| part.strip_prefix("a ")).unwrap_or(part);
                            let id = norm_id(part);
                            match [id.clone(), id.trim_end_matches('s').to_owned(), id.replace("men", "man")].into_iter().find(|i| self.names.entity(i)) {
                                Some(id) => items.push(id),
                                None => ok = false,
                            }
                        }
                        ok
                    }
                },
                4 => {
                    number = a.as_deref().and_then(parse_span).unwrap_or(0.0);
                    world = b.map(|b| value::world_name(unquote(&b)));
                    number >= 0.05
                }
                8 => {
                    let t = a.as_deref().and_then(|a| value::parse_time_of_day(unquote(a)));
                    world = b.map(|b| value::world_name(unquote(&b)));
                    number = t.unwrap_or(0) as f64;
                    t.is_some()
                }
                _ => true,
            };
            if ok {
                found = Some((*kind, items, holding, filter, number, world));
                break;
            }
        }
        let Some((kind, items, holding, filter, number, world)) = found else {
            let lower = head.to_ascii_lowercase();
            if UNSUPPORTED_EVENTS.iter().any(|w| lower.contains(w)) {
                self.note = Some("event");
            }
            self.fail(n.line, "event", head);
            return None;
        };
        if kind == EventKind::Pickup && !items.is_empty() {
            self.note = Some("pickup");
            self.fail(n.line, "event", head);
            return None;
        }
        self.in_command = false;
        self.in_function = false;
        let code = self.body(&n.children)?;
        Some(Trigger { script: self.script.clone(), line: n.line, kind, items, holding, filter, number, world, code })
    }

    fn arg_type(name: &str) -> Option<ArgType> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "text" | "string" | "texts" | "strings" => ArgType::Text,
            "player" | "players" => ArgType::Player,
            "offline player" | "offlineplayer" | "offline players" | "offlineplayers" => ArgType::OfflinePlayer,
            "number" | "numbers" | "num" | "double" | "float" => ArgType::Number,
            "integer" | "integers" | "int" | "long" => ArgType::Integer,
            "boolean" | "bool" => ArgType::Boolean,
            "item" | "items" | "itemtype" | "item type" | "itemtypes" | "material" => ArgType::Item,
            "world" => ArgType::World,
            "timespan" | "time span" | "duration" => ArgType::Timespan,
            _ => return None,
        })
    }

    fn arg_def(&mut self, inner: &str, optional: bool) -> Option<ArgDef> {
        let (decl, default) = match inner.split_once('=') {
            Some((d, v)) => (d.trim(), Some(v.trim())),
            None => (inner.trim(), None),
        };
        let (name, ty) = match decl.split_once(':') {
            Some((n, t)) => (Some(n.trim().to_ascii_lowercase()), t),
            None => (None, decl),
        };
        let ty = Self::arg_type(ty)?;
        let default = match default {
            Some(d) => {
                let d = d.strip_prefix('%').and_then(|d| d.strip_suffix('%')).unwrap_or(d);
                Some(self.expr(d).or_else(|| Some(Expr::Lit(Value::text(unquote(d)))))?)
            }
            None => None,
        };
        Some(ArgDef { name, ty, optional: optional || default.is_some(), default })
    }

    fn command(&mut self, n: &Node, head: &str) -> Option<CommandDef> {
        let head = head.trim();
        let (label, spec) = head.split_once(' ').unwrap_or((head, ""));
        let name = label.trim_start_matches('/').to_ascii_lowercase();
        if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')) {
            self.fail(n.line, "command", head);
            return None;
        }
        // argument pattern: words, <type>, <name:type = default>, [optional parts]
        let mut parts = Vec::new();
        let chars: Vec<char> = spec.chars().collect();
        let (mut i, mut optional, mut word) = (0, 0u32, String::new());
        let mut bad = false;
        while i < chars.len() {
            match chars[i] {
                '[' => optional += 1,
                ']' => optional = optional.saturating_sub(1),
                '<' => {
                    let Some(len) = chars[i..].iter().position(|c| *c == '>') else {
                        bad = true;
                        break;
                    };
                    let inner: String = chars[i + 1..i + len].iter().collect();
                    match self.arg_def(&inner, optional > 0) {
                        Some(def) => parts.push(ArgPart::Arg(def)),
                        None => bad = true,
                    }
                    i += len;
                }
                c if c.is_whitespace() => {
                    if !word.is_empty() && optional == 0 {
                        parts.push(ArgPart::Word(std::mem::take(&mut word).to_ascii_lowercase()));
                    }
                    word.clear();
                }
                c => word.push(c),
            }
            i += 1;
        }
        if !word.is_empty() && optional == 0 {
            parts.push(ArgPart::Word(word.to_ascii_lowercase()));
        }
        if bad {
            self.fail(n.line, "command", head);
            return None;
        }
        let mut cmd = CommandDef {
            script: self.script.clone(),
            line: n.line,
            name,
            aliases: Vec::new(),
            parts,
            description: String::new(),
            usage: None,
            permission: None,
            permission_message: None,
            executable_by: ExecBy::Both,
            cooldown: 0.0,
            cooldown_message: None,
            cooldown_bypass: None,
            code: Arc::new(Code { script: self.script.clone(), ops: vec![Op::Stop], lines: vec![n.line] }),
        };
        let before = self.errors.len();
        let mut has_trigger = false;
        for c in &n.children {
            self.memo.clear();
            self.note = None;
            let (key, val) = c.text.split_once(':').map_or((c.text.as_str(), ""), |(k, v)| (k.trim(), v.trim()));
            match key.to_ascii_lowercase().as_str() {
                "trigger" => {
                    has_trigger = true;
                    self.in_command = true;
                    self.in_function = false;
                    if let Some(code) = self.body(&c.children) {
                        cmd.code = code;
                    }
                    self.in_command = false;
                }
                "description" => cmd.description = unquote(val).chars().take(200).collect(),
                "usage" => match self.text(unquote(val)) {
                    Some(e) => cmd.usage = Some(e),
                    None => self.fail(c.line, "expression", &c.text),
                },
                "permission" => cmd.permission = Some(unquote(val).to_owned()),
                "permission message" => match self.text(unquote(val)) {
                    Some(e) => cmd.permission_message = Some(e),
                    None => self.fail(c.line, "expression", &c.text),
                },
                "aliases" => {
                    cmd.aliases = val.split(',').map(|a| unquote(a).trim().trim_start_matches('/').to_ascii_lowercase()).filter(|a| !a.is_empty()).collect();
                }
                "executable by" => {
                    let v = val.to_ascii_lowercase();
                    cmd.executable_by = match (v.contains("player"), v.contains("console") || v.contains("server")) {
                        (true, false) => ExecBy::Players,
                        (false, true) => ExecBy::Console,
                        _ => ExecBy::Both,
                    };
                }
                "cooldown" => match parse_span(val) {
                    Some(s) => cmd.cooldown = s,
                    None => self.fail(c.line, "entry", &c.text),
                },
                "cooldown message" => match self.text(unquote(val)) {
                    Some(e) => cmd.cooldown_message = Some(e),
                    None => self.fail(c.line, "expression", &c.text),
                },
                "cooldown bypass" => cmd.cooldown_bypass = Some(unquote(val).to_owned()),
                "cooldown storage" => self.warnings.push(err(c.line, "unsupported", Some("cooldown_storage"), &c.text)),
                _ => self.fail(c.line, "entry", &c.text),
            }
        }
        if !has_trigger {
            self.fail(n.line, "command", head);
        }
        (self.errors.len() == before).then_some(cmd)
    }

    fn function(&mut self, n: &Node, head: &str) -> Option<FunctionDef> {
        let lower = head.to_ascii_lowercase();
        let rest = lower.strip_prefix("local ").unwrap_or(&lower);
        let rest = rest.strip_prefix("function ").unwrap_or(rest);
        let rest = &head[head.len() - rest.len()..];
        let parsed = (|| {
            let open = rest.find('(')?;
            let close = rest.rfind(')')?;
            let name = rest[..open].trim().to_ascii_lowercase();
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            let mut params = Vec::new();
            let inner = rest.get(open + 1..close)?;
            if !inner.trim().is_empty() {
                for p in split_top(inner, &[","]) {
                    let (pname, decl) = p.split_once(':')?;
                    let default = match decl.split_once('=') {
                        Some((_, d)) => Some(self.expr(d.trim())?),
                        None => None,
                    };
                    params.push((pname.trim().to_ascii_lowercase(), default));
                }
            }
            Some((name, params))
        })();
        let Some((name, params)) = parsed else {
            self.fail(n.line, "function", head);
            return None;
        };
        self.in_command = false;
        self.in_function = true;
        let code = self.body(&n.children);
        self.in_function = false;
        Some(FunctionDef { script: self.script.clone(), name, params, code: code? })
    }

    // ---- statements --------------------------------------------------------------------------

    fn emit(&mut self, op: Op) -> usize {
        self.ops.push(op);
        self.lines.push(self.line);
        self.ops.len() - 1
    }

    fn patch(&mut self, at: usize, to: usize) {
        match &mut self.ops[at] {
            Op::JumpIfNot(_, t) | Op::Jump(t) | Op::LoopInit(_, t) | Op::LoopNext(t) => *t = to,
            _ => {}
        }
    }

    /// Compiles a block; `exits` collects jumps that leave it (a failed inline condition).
    fn block(&mut self, nodes: &[Node], exits: &mut Vec<usize>) {
        let mut i = 0;
        while i < nodes.len() {
            let n = &nodes[i];
            i += 1;
            self.line = n.line;
            self.memo.clear();
            self.note = None;
            let Some(head) = n.text.strip_suffix(':') else {
                if !n.children.is_empty() {
                    self.fail(n.line, "indent", &n.text);
                    continue;
                }
                self.statement(n, exits);
                continue;
            };
            let head = head.trim();
            let lower = head.to_ascii_lowercase();
            if lower == "else" || lower.starts_with("else if ") {
                self.fail(n.line, "section", head);
            } else if let Some(rest) = lower.strip_prefix("loop ") {
                self.loop_section(n, &head[head.len() - rest.len()..]);
            } else if let Some(rest) = lower.strip_prefix("while ") {
                let start = self.ops.len();
                let Some(cond) = self.cond(&head[head.len() - rest.len()..]) else {
                    self.fail(n.line, "condition", head);
                    continue;
                };
                let test = self.emit(Op::JumpIfNot(cond, 0));
                self.loops.push(LoopCtx { counted: false, breaks: vec![test], continues: Vec::new() });
                let mut inner = Vec::new();
                self.block(&n.children, &mut inner);
                self.line = n.line;
                let back = self.emit(Op::Jump(start));
                let ctx = self.loops.pop().unwrap_or_else(|| unreachable!());
                let end = self.ops.len();
                ctx.breaks.iter().for_each(|b| self.patch(*b, end));
                ctx.continues.iter().chain(&inner).for_each(|c| self.patch(*c, back));
            } else {
                // if / else if / else chain; a bare `condition:` is an `if`
                let mut ends = Vec::new();
                let mut arm = Some((n, lower.strip_prefix("if ").map_or(head, |r| &head[head.len() - r.len()..])));
                while let Some((node, cond_text)) = arm.take() {
                    self.line = node.line;
                    self.memo.clear();
                    self.note = None;
                    let test = if cond_text.is_empty() {
                        None
                    } else {
                        match self.cond(cond_text) {
                            Some(c) => Some(self.emit(Op::JumpIfNot(c, 0))),
                            None => {
                                self.fail(node.line, if node.text.to_ascii_lowercase().starts_with("if ") || node.text.to_ascii_lowercase().starts_with("else") { "condition" } else { "section" }, cond_text);
                                None
                            }
                        }
                    };
                    let mut inner = Vec::new();
                    self.block(&node.children, &mut inner);
                    // what follows this arm?
                    let next = nodes.get(i).and_then(|nx| {
                        let l = nx.text.to_ascii_lowercase();
                        let h = l.strip_suffix(':')?.trim().to_owned();
                        if h == "else" {
                            Some((nx, ""))
                        } else if h.starts_with("else if ") {
                            let t = nx.text.strip_suffix(':')?.trim();
                            Some((nx, &t[8..]))
                        } else {
                            None
                        }
                    });
                    self.line = node.line;
                    let after_body = self.ops.len();
                    if next.is_some() && test.is_some() {
                        ends.push(self.emit(Op::Jump(0)));
                    }
                    inner.iter().for_each(|j| self.patch(*j, after_body));
                    if let Some(t) = test {
                        let to = self.ops.len();
                        self.patch(t, to);
                    }
                    if let Some(nx) = next {
                        if test.is_some() || !cond_text.is_empty() {
                            i += 1;
                            arm = Some(nx);
                        }
                    }
                }
                let end = self.ops.len();
                ends.iter().for_each(|e| self.patch(*e, end));
            }
        }
    }

    fn loop_section(&mut self, n: &Node, what: &str) {
        let lower = what.to_ascii_lowercase();
        let src = if let Some(times) = lower.strip_suffix(" times").or_else(|| lower.strip_suffix(" time")) {
            self.expr(&what[..times.len()]).map(LoopSrc::Times)
        } else if matches!(lower.as_str(), "all players" | "players" | "all online players" | "online players" | "all the players" | "all of the players") {
            Some(LoopSrc::Items(Expr::Op(ExprOp::AllPlayers, Vec::new(), String::new())))
        } else if let Some(range) = ["integers from ", "integers between ", "numbers from ", "numbers between ", "all integers from ", "all integers between "]
            .iter()
            .find_map(|p| lower.strip_prefix(p))
        {
            let range = &what[what.len() - range.len()..];
            let parts = split_top(range, &[" to ", " and "]);
            match parts.as_slice() {
                [a, b] => match (self.expr(a), self.expr(b)) {
                    (Some(a), Some(b)) => Some(LoopSrc::Range(a, b)),
                    _ => None,
                },
                _ => None,
            }
        } else if let Some(rest) = ["all blocks in radius ", "blocks in radius "].iter().find_map(|p| lower.strip_prefix(p)) {
            // `loop blocks in radius 3 around player`
            let rest = &what[what.len() - rest.len()..];
            match split_top(rest, &[" around ", " of "]).as_slice() {
                [radius, place] => match (self.expr(radius), self.expr(place)) {
                    (Some(r), Some(p)) => Some(LoopSrc::Items(Expr::Op(ExprOp::BlocksAround, vec![r, p], String::new()))),
                    _ => None,
                },
                _ => None,
            }
        } else if matches!(lower.as_str(), "all entities" | "entities" | "all the entities") {
            Some(LoopSrc::Items(Expr::Op(ExprOp::Entities, Vec::new(), String::new())))
        } else if let Some(rest) = ["all entities in radius ", "entities in radius "].iter().find_map(|p| lower.strip_prefix(p)) {
            let rest = &what[what.len() - rest.len()..];
            match split_top(rest, &[" around ", " of "]).as_slice() {
                [radius, place] => match (self.expr(radius), self.expr(place)) {
                    (Some(r), Some(p)) => Some(LoopSrc::Items(Expr::Op(ExprOp::Entities, vec![r, p], String::new()))),
                    _ => None,
                },
                _ => None,
            }
        } else if let Some(rest) = ["all items in ", "items in "].iter().find_map(|p| lower.strip_prefix(p)) {
            self.expr(&what[what.len() - rest.len()..]).map(LoopSrc::Items)
        } else if lower.contains("blocks") || lower.contains("entities") {
            self.note = Some("loop");
            None
        } else {
            self.expr(what).map(LoopSrc::Items)
        };
        let Some(src) = src else {
            self.fail(n.line, "loop", what);
            return;
        };
        let init = self.emit(Op::LoopInit(src, 0));
        let start = self.ops.len();
        self.loops.push(LoopCtx { counted: true, breaks: vec![init], continues: Vec::new() });
        let mut inner = Vec::new();
        self.block(&n.children, &mut inner);
        self.line = n.line;
        let next = self.emit(Op::LoopNext(start));
        let ctx = self.loops.pop().unwrap_or_else(|| unreachable!());
        let end = self.ops.len();
        ctx.breaks.iter().for_each(|b| self.patch(*b, end));
        ctx.continues.iter().chain(&inner).for_each(|c| self.patch(*c, next));
    }

    fn statement(&mut self, n: &Node, exits: &mut Vec<usize>) {
        let text = n.text.as_str();
        let lower = text.to_ascii_lowercase();
        let words: Vec<&str> = lower.split(' ').collect();
        // flow
        match words.as_slice() {
            ["stop"] | ["exit"] | ["stop" | "exit", "trigger"] => {
                self.emit(Op::Stop);
                return;
            }
            ["continue"] | ["continue", "loop"] | ["continue", "this", "loop"] => {
                if self.loops.is_empty() {
                    self.fail(n.line, "flow", text);
                } else {
                    let j = self.emit(Op::Jump(0));
                    if let Some(l) = self.loops.last_mut() {
                        l.continues.push(j);
                    }
                }
                return;
            }
            ["stop" | "exit", rest @ ..] if rest.last().is_some_and(|w| *w == "loop" || *w == "loops") => {
                let count = match rest {
                    ["loop"] | ["a" | "the" | "this" | "1", "loop"] => 1,
                    ["all", "loops"] => self.loops.len(),
                    [num, "loops"] => num.parse().unwrap_or(0),
                    _ => 0,
                };
                if count == 0 || count > self.loops.len() {
                    self.fail(n.line, "flow", text);
                    return;
                }
                let from = self.loops.len() - count;
                let counted = self.loops[from..].iter().filter(|l| l.counted).count();
                if counted > 0 {
                    self.emit(Op::PopLoops(counted));
                }
                let j = self.emit(Op::Jump(0));
                // the init/test jump already targets the end of that loop; reuse its patch list
                self.loops[from].breaks.push(j);
                return;
            }
            ["return"] => {
                self.emit(if self.in_function { Op::Return(None) } else { Op::Stop });
                return;
            }
            ["return", ..] => {
                match (self.in_function, self.expr(&text[7..])) {
                    (true, Some(e)) => {
                        self.emit(Op::Return(Some(e)));
                    }
                    _ => self.fail(n.line, "flow", text),
                }
                return;
            }
            ["wait" | "halt", ..] => {
                let rest = text[4..].trim();
                let rest = rest.strip_prefix("for ").unwrap_or(rest);
                let rest_lower = rest.to_ascii_lowercase();
                if let Some(cond) = rest_lower.strip_prefix("until ").or_else(|| rest_lower.strip_prefix("while ")) {
                    // checked again every tick until it is (no longer) true
                    match self.cond(&rest[rest.len() - cond.len()..]) {
                        Some(c) => {
                            self.emit(Op::WaitFor(c, rest_lower.starts_with("while ")));
                        }
                        None => self.fail(n.line, "condition", text),
                    }
                    return;
                }
                match self.expr(rest) {
                    Some(e) if self.note.is_none() => {
                        self.emit(Op::Wait(e));
                    }
                    _ => self.fail(n.line, "effect", text),
                }
                return;
            }
            _ => {}
        }
        if let Some(effect) = self.effect(text) {
            self.emit(Op::Effect(effect));
            return;
        }
        if self.note.is_some() {
            self.fail(n.line, "effect", text);
            return;
        }
        if let Some(Expr::Call(name, args)) = self.call(text) {
            self.emit(Op::Effect(Effect::CallFn(name, args)));
            return;
        }
        match self.cond(text) {
            Some(c) => {
                let j = self.emit(Op::JumpIfNot(c, 0));
                exits.push(j);
            }
            None => self.fail(n.line, "effect", text),
        }
    }

    // ---- effects -----------------------------------------------------------------------------

    fn effect(&mut self, text: &str) -> Option<Effect> {
        let input = Input::new(text);
        for (pat, kind) in effect_patterns() {
            let mut out = None;
            let kind = *kind;
            pat.matches(&input, &mut |c: &Caps| {
                out = self.build_effect(kind, c);
                out.is_some()
            });
            if out.is_some() {
                self.note = None;
                return out;
            }
            if self.note.is_some() {
                return None;
            }
        }
        None
    }

    fn opt(&mut self, c: &Caps, i: usize) -> Option<Option<Expr>> {
        match c.get(i) {
            Some(s) => self.expr(s).map(Some),
            None => Some(None),
        }
    }

    fn build_effect(&mut self, kind: E, c: &Caps) -> Option<Effect> {
        let a = c.get(0);
        Some(match kind {
            E::Unsupported(reason) => {
                self.note = Some(reason);
                return None;
            }
            E::ActionBar => Effect::Send(self.expr(a?)?, self.opt(c, 1)?, SendKind::ActionBar),
            E::Send => Effect::Send(self.expr(a?)?, self.opt(c, 1)?, SendKind::Chat),
            E::Broadcast => Effect::Broadcast(self.expr(a?)?),
            E::Title => {
                let args = vec![Some(self.expr(a?)?), self.opt(c, 1)?, self.opt(c, 2)?, self.opt(c, 3)?, self.opt(c, 4)?, self.opt(c, 5)?];
                Effect::Host(Fx::Title, args, 0)
            }
            E::Teleport => Effect::Host(Fx::Teleport, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?)], 0),
            E::ConsoleCmd => Effect::Host(Fx::ConsoleCmd, vec![Some(self.expr(a?)?)], 0),
            E::PlayerCmd => Effect::Host(Fx::PlayerCmd, vec![Some(self.expr(c.get(1)?)?), Some(self.expr(a?)?)], 0),
            E::PlayerCmdRev => Effect::Host(Fx::PlayerCmd, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?)], 0),
            E::Cancel => Effect::Cancel(true),
            E::Uncancel => Effect::Cancel(false),
            E::CancelCooldown => Effect::CancelCooldown,
            E::Give | E::GiveRev => {
                let (items, who) = if kind == E::Give { (a?, c.get(1)?) } else { (c.get(1)?, a?) };
                let items = self.expr(items)?;
                if !self.is_items(&items) {
                    return None;
                }
                Effect::Host(Fx::Give, vec![Some(items), Some(self.expr(who)?)], 0)
            }
            E::Fx1(fx) => Effect::Host(fx, vec![Some(self.expr(a?)?), self.opt(c, 1)?], 0),
            E::Damage => Effect::Host(Fx::Heal, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?)], 1),
            E::Kick => Effect::Host(Fx::Kick, vec![Some(self.expr(a?)?), self.opt(c, 1)?], 0),
            E::Ban => Effect::Host(Fx::Ban, vec![Some(self.expr(a?)?), self.opt(c, 1)?], 0),
            E::Sound => {
                let name = unquote(a?);
                let sound = if a?.starts_with('"') && !name.contains('%') { Expr::Lit(Value::text(name)) } else { self.expr(a?)? };
                Effect::Host(Fx::Sound, vec![Some(sound), self.opt(c, 1)?, self.opt(c, 2)?, self.opt(c, 3)?, self.opt(c, 4)?], c.mark)
            }
            E::Lightning => Effect::Host(Fx::Lightning, vec![Some(self.expr(a?)?)], c.mark),
            E::Explosion => Effect::Host(Fx::Explosion, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?)], c.mark),
            E::Potion => {
                // `speed`, `speed 2`, `potion of speed`
                let raw = a?.trim();
                let (name, tier) = match raw.rsplit_once(' ') {
                    Some((n, t)) if parse_number(t).is_some() && potion_id(n).is_some() => (n, Some(Expr::Lit(Value::Num(parse_number(t)?)))),
                    _ => (raw, None),
                };
                let potion = match potion_id(name) {
                    Some(id) => Expr::Lit(Value::text(id)),
                    None => self.expr(name)?,
                };
                let tier = match self.opt(c, 1)? {
                    Some(t) => Some(t),
                    None => tier,
                };
                Effect::Host(Fx::Potion, vec![Some(potion), tier, Some(self.expr(c.get(2)?)?), self.opt(c, 3)?], 0)
            }
            E::Spawn => {
                let raw = a?.trim();
                let (count, name) = match raw.split_once(' ') {
                    Some((n, rest)) if parse_number(n).is_some() => (parse_number(n)?, rest),
                    Some(("a" | "an", rest)) => (1.0, rest),
                    _ => (1.0, raw),
                };
                let name = name.strip_prefix("of ").unwrap_or(name);
                let id = norm_id(name);
                let id = [id.clone(), id.trim_end_matches('s').to_owned(), id.trim_end_matches("es").to_owned(), id.replace("men", "man")]
                    .into_iter()
                    .find(|i| self.names.entity(i));
                let Some(id) = id else {
                    self.note = Some("entity");
                    return None;
                };
                Effect::Host(Fx::Spawn, vec![Some(Expr::Lit(Value::text(id))), Some(self.expr(c.get(1)?)?), Some(Expr::Lit(Value::Num(count)))], 0)
            }
            E::Push => Effect::Host(Fx::Push, vec![Some(self.expr(a?)?), self.opt(c, 1)?], c.mark),
            E::Ignite => Effect::Host(Fx::Ignite, vec![Some(self.expr(a?)?), self.opt(c, 1)?], 0),
            E::Log => Effect::Host(Fx::Log, vec![Some(self.expr(a?)?), self.opt(c, 1)?], 0),
            E::Replace | E::ReplaceRev => {
                let (w, h) = if kind == E::Replace { (1, 2) } else { (2, 1) };
                let (needle, with, hay) = (self.expr(a?)?, self.expr(c.get(w)?)?, self.expr(c.get(h)?)?);
                if !matches!(hay, Expr::Var(_)) && !matches!(&hay, Expr::Ev(k) if k == "message") {
                    return None;
                }
                Effect::ReplaceIn(hay, needle, with)
            }
            E::Drop => {
                let items = self.expr(a?)?;
                if !self.is_items(&items) {
                    return None;
                }
                Effect::Host(Fx::Drop, vec![Some(items), Some(self.expr(c.get(1)?)?)], 0)
            }
            E::OpenMenu => Effect::Host(Fx::OpenMenu, vec![self.opt(c, 0)?, self.opt(c, 1)?, Some(self.expr(c.get(2)?)?)], 0),
            E::CloseMenu => Effect::Host(Fx::CloseMenu, vec![Some(self.expr(a?)?)], 0),
            E::SetSlot => {
                let item = if c.get(2)?.eq_ignore_ascii_case("air") { Expr::Lit(Value::None) } else { self.expr(c.get(2)?)? };
                Effect::Host(Fx::SetSlot, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?), Some(item), None], c.mark & 1)
            }
            E::FormatSlot => {
                let command = match c.get(3).or(c.get(4)) {
                    Some(raw) => Some(self.expr(raw)?),
                    None => None,
                };
                Effect::Host(Fx::SetSlot, vec![Some(self.expr(a?)?), Some(self.expr(c.get(1)?)?), Some(self.expr(c.get(2)?)?), command], 1 | c.mark)
            }
            E::Enchant => {
                // `enchant player's tool with sharpness 3` is `set player's tool to (that tool, enchanted)`
                let target = self.expr(a?)?;
                let list = enchant_list(c.get(1)?)?;
                if !matches!(target, Expr::Var(_) | Expr::Prop(_, Prop::Tool | Prop::Helmet | Prop::Chestplate | Prop::Leggings | Prop::Boots)) {
                    return None;
                }
                Effect::Change(target.clone(), Mode::Set, Some(Expr::Op(ExprOp::Enchanted, vec![target], list)))
            }
            E::Equip => {
                let items = self.expr(c.get(1)?)?;
                if !self.is_items(&items) {
                    return None;
                }
                Effect::Host(Fx::Equip, vec![Some(self.expr(a?)?), Some(items)], 0)
            }
            E::Shoot | E::ShootRev => {
                let (who, what) = if kind == E::Shoot { (a?, c.get(1)?) } else { (c.get(1)?, a?) };
                let id = norm_id(what);
                if !matches!(id.as_str(), "arrow" | "snowball" | "egg" | "ender_pearl" | "splash_potion" | "fireball" | "small_fireball" | "trident" | "wind_charge") {
                    self.note = Some("shoot");
                    return None;
                }
                Effect::Host(Fx::Shoot, vec![Some(self.expr(who)?), Some(Expr::Lit(Value::text(id)))], 0)
            }
            E::Change(mode) => self.change(mode, c, false)?,
            E::ChangeRev(mode) => self.change(mode, c, true)?,
        })
    }

    fn is_items(&self, e: &Expr) -> bool {
        match e {
            Expr::Lit(Value::Item(_)) | Expr::Var(_) | Expr::Arg(..) | Expr::Loop(..) | Expr::Ev(_) | Expr::Call(..) => true,
            Expr::Op(op, ..) => matches!(op, ExprOp::ItemOf | ExprOp::Named | ExprOp::WithLore | ExprOp::RandElem | ExprOp::ParsedAs),
            Expr::Prop(_, p) => matches!(p, Prop::Tool | Prop::Type | Prop::Helmet | Prop::Chestplate | Prop::Leggings | Prop::Boots),
            Expr::List(l, _) => l.iter().all(|e| self.is_items(e)),
            _ => false,
        }
    }

    fn change(&mut self, mode: Mode, c: &Caps, target_first: bool) -> Option<Effect> {
        let (value_raw, target_raw) = match mode {
            Mode::Delete | Mode::Reset => (None, c.get(0)?),
            // `set %target% to %value%`, `increase %target% by %value%`
            Mode::Set => (Some(c.get(1)?), c.get(0)?),
            _ if target_first => (Some(c.get(1)?), c.get(0)?),
            _ => (Some(c.get(0)?), c.get(1)?),
        };
        // `remove speed from player`
        if let (Mode::Remove | Mode::RemoveAll, Some(v)) = (mode, value_raw) {
            if let Some(id) = potion_id(v) {
                let who = self.expr(target_raw)?;
                if !matches!(who, Expr::Var(_)) {
                    return Some(Effect::Host(Fx::Unpotion, vec![Some(Expr::Lit(Value::text(id))), Some(who)], 0));
                }
            }
        }
        let target = self.expr(target_raw)?;
        let value = match (value_raw, &target) {
            (Some(v), Expr::Prop(_, Prop::Time)) => match value::parse_time_of_day(unquote(v)) {
                Some(t) => Some(Expr::Lit(Value::Num(t as f64))),
                None => Some(self.expr(v)?),
            },
            (Some(v), _) => Some(self.expr(v)?),
            (None, _) => None,
        };
        match &target {
            Expr::Var(_) | Expr::Prop(..) => {}
            Expr::Op(ExprOp::BlockAt | ExprOp::BlockRel | ExprOp::TargetBlock, ..) if mode == Mode::Set => {}
            Expr::Ev(k) if mode == Mode::Set && matches!(k.as_str(), "message" | "damage" | "death message" | "block") => {}
            Expr::Ev(k) if mode == Mode::Set && matches!(k.as_str(), "join message" | "quit message") => {
                self.warnings.push(err(self.line, "unsupported", Some("join_message"), target_raw));
            }
            // `remove 3 diamonds from player`, `add stone to player`
            _ if matches!(mode, Mode::Add | Mode::Remove | Mode::RemoveAll) && value.as_ref().is_some_and(|v| self.is_items(v)) => {
                let fx = if mode == Mode::Add { Fx::Give } else { Fx::Take };
                return Some(Effect::Host(fx, vec![value, Some(target)], u32::from(mode == Mode::RemoveAll)));
            }
            _ => return None,
        }
        Some(Effect::Change(target, mode, value))
    }

    // ---- conditions --------------------------------------------------------------------------

    pub fn cond(&mut self, text: &str) -> Option<Cond> {
        self.depth += 1;
        let out = if self.depth > 40 { None } else { self.cond_inner(text) };
        self.depth -= 1;
        out
    }

    fn cond_inner(&mut self, text: &str) -> Option<Cond> {
        let text = text.trim();
        let input = Input::new(text);
        for (pat, kind) in cond_patterns() {
            let mut out = None;
            let kind = *kind;
            pat.matches(&input, &mut |c: &Caps| {
                out = self.build_cond(kind, c);
                out.is_some()
            });
            if out.is_some() {
                return out;
            }
        }
        // `a is 1 and b is 2`
        for (sep, all) in [(" or ", false), (" and ", true)] {
            let parts = split_top(text, &[sep]);
            if parts.len() > 1 {
                let conds: Option<Vec<Cond>> = parts.iter().map(|p| self.cond(p)).collect();
                if let Some(conds) = conds {
                    return Some(if all { Cond::All(conds) } else { Cond::Any(conds) });
                }
            }
        }
        None
    }

    fn build_cond(&mut self, kind: C, c: &Caps) -> Option<Cond> {
        let neg = c.mark & 2 != 0;
        let a = match c.get(0) {
            Some(a) => Some(self.expr(a)?),
            None => None,
        };
        let b = match c.get(1) {
            Some(b) => Some(self.expr(b)?),
            None => None,
        };
        let check = |k: Check, args: Vec<Option<Expr>>, neg: bool| Some(Cond::Check(k, args.into_iter().collect::<Option<Vec<_>>>()?, neg));
        match kind {
            C::IsSet | C::Exists => check(Check::IsSet, vec![a], neg),
            C::HasPerm => check(Check::HasPerm, vec![a, b], neg),
            C::Is(k) => check(k, vec![a], neg),
            C::Offline => check(Check::Online, vec![a], !neg),
            C::Dead => check(Check::Alive, vec![a], !neg),
            C::InWorld => check(Check::InWorld, vec![a, b], neg),
            C::HasItem => {
                if !self.is_items(b.as_ref()?) {
                    return None;
                }
                check(Check::HasItem, vec![a, b], neg)
            }
            C::Afford => check(Check::Afford, vec![a, b], neg),
            C::Chance => check(Check::Chance, vec![a], false),
            // `player's inventory contains 3 diamonds`
            C::Contains => match a {
                Some(Expr::Prop(owner, Prop::Inventory)) => check(Check::HasItem, vec![Some(*owner), b], neg),
                a => check(Check::Contains, vec![a, b], neg),
            },
            C::Starts => check(Check::StartsWith, vec![a, b], neg),
            C::Ends => check(Check::EndsWith, vec![a, b], neg),
            C::Matches => check(Check::Matches, vec![a, b], neg),
            C::Between => {
                let third = self.expr(c.get(2)?)?;
                check(Check::Between, vec![a, b, Some(third)], neg)
            }
            C::Empty => check(Check::Empty, vec![a], neg),
            C::Cancelled => check(Check::Cancelled, vec![], neg),
            C::Holding => {
                let cmp = Cond::Cmp(Expr::Prop(Box::new(a?), Prop::Tool), CmpOp::Eq, b?);
                Some(if neg { negate(cmp) } else { cmp })
            }
            C::Cmp(op) => Some(Cond::Cmp(a?, op, b?)),
        }
    }

    // ---- expressions -------------------------------------------------------------------------

    pub fn expr(&mut self, text: &str) -> Option<Expr> {
        let text = text.trim();
        if text.is_empty() || text.len() > 2000 {
            return None;
        }
        if let Some(hit) = self.memo.get(text) {
            return hit.clone();
        }
        self.depth += 1;
        let out = if self.depth > 40 { None } else { self.expr_inner(text) };
        self.depth -= 1;
        self.memo.insert(text.to_owned(), out.clone());
        out
    }

    /// The text of a quoted string (quotes already removed) with its `%expressions%`.
    pub fn text(&mut self, content: &str) -> Option<Expr> {
        let content = content.replace("\"\"", "\"");
        let mut parts = Vec::new();
        let mut lit = String::new();
        let chars: Vec<char> = content.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '%' {
                lit.push(chars[i]);
                i += 1;
                continue;
            }
            // closing `%` outside braces
            let (mut j, mut depth, mut end) = (i + 1, 0i32, None);
            while j < chars.len() {
                match chars[j] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    '%' if depth <= 0 => {
                        end = Some(j);
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            match end {
                Some(end) if end == i + 1 => {
                    lit.push('%');
                    i = end + 1;
                }
                Some(end) => {
                    let inner: String = chars[i + 1..end].iter().collect();
                    let e = self.expr(&inner)?;
                    if !lit.is_empty() {
                        parts.push(TextPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(TextPart::Expr(e));
                    i = end + 1;
                }
                None => {
                    lit.push('%');
                    i += 1;
                }
            }
        }
        if parts.is_empty() {
            return Some(Expr::Lit(Value::Text(lit)));
        }
        if !lit.is_empty() {
            parts.push(TextPart::Lit(lit));
        }
        Some(Expr::Text(parts))
    }

    fn var(&mut self, inner: &str) -> Option<Expr> {
        let inner = inner.trim();
        let (local, name) = match inner.strip_prefix('_') {
            Some(n) => (true, n),
            None => (false, inner),
        };
        let (list, name) = match name.strip_suffix("::*") {
            Some(n) => (true, n),
            None => (false, name),
        };
        if name.is_empty() || name.contains('*') {
            return None;
        }
        let parts = match self.text(name)? {
            Expr::Lit(Value::Text(t)) => vec![TextPart::Lit(t)],
            Expr::Text(parts) => parts,
            _ => return None,
        };
        Some(Expr::Var(VarName { parts, local, list }))
    }

    fn expr_inner(&mut self, text: &str) -> Option<Expr> {
        let input = Input::new(text);
        let bytes = text.as_bytes();
        // whole thing wrapped: (…), "…", {…}
        if bytes[0] == b'(' && text.ends_with(')') && (1..text.len() - 1).all(|i| !input.is_top(i)) {
            return self.expr(&text[1..text.len() - 1]);
        }
        if bytes[0] == b'"' && text.len() >= 2 && text.ends_with('"') && (1..text.len()).all(|i| !input.is_top(i)) {
            return self.text(&text[1..text.len() - 1]);
        }
        if bytes[0] == b'{' && text.ends_with('}') && (1..text.len()).all(|i| !input.is_top(i)) {
            return self.var(&text[1..text.len() - 1]);
        }
        if let Some(n) = parse_number(text) {
            return Some(Expr::Lit(Value::Num(n)));
        }
        if let Some(n) = text.strip_suffix('%').and_then(parse_number) {
            return Some(Expr::Lit(Value::Num(n)));
        }
        let lower = text.to_ascii_lowercase();
        if let Some(e) = self.simple(&lower) {
            return Some(e);
        }
        if let Some(s) = parse_span(text) {
            return Some(Expr::Lit(Value::Span(s)));
        }
        if let Some(call) = self.call(text) {
            return Some(call);
        }
        let either = split_top(text, &[" ? ", " otherwise "]);
        if let [first, second] = either.as_slice() {
            if let (Some(a), Some(b)) = (self.expr(first), self.expr(second)) {
                return Some(Expr::Op(ExprOp::Default, vec![a, b], String::new()));
            }
        }
        if let Some(e) = self.arithmetic(text, &input) {
            return Some(e);
        }
        if let Some(e) = self.property(text, &input) {
            return Some(e);
        }
        if self.note.is_some() {
            return None;
        }
        for (pat, kind) in expr_patterns() {
            let mut out = None;
            let kind = *kind;
            pat.matches(&input, &mut |c: &Caps| {
                out = self.build_expr(kind, c);
                out.is_some()
            });
            if out.is_some() {
                return out;
            }
        }
        // lists: a, b and c
        let has_or = !input.find_top(" or ").is_empty();
        let parts = split_top(text, &[", and ", ", or ", ",", " and ", " or "]);
        if parts.len() > 1 {
            let items: Option<Vec<Expr>> = parts.iter().map(|p| self.expr(p)).collect();
            if let Some(items) = items {
                return Some(Expr::List(items, !has_or));
            }
        }
        if WORDS.contains(&lower.as_str()) {
            return Some(Expr::Lit(Value::Text(lower)));
        }
        if lower.starts_with("vector") || lower.contains(" vector ") {
            self.note = Some("vector");
            return None;
        }
        if let Some(item) = self.item(text) {
            return Some(Expr::Lit(Value::Item(item)));
        }
        // an entity type: `attacker is a zombie`
        let name = lower.strip_prefix("an ").or_else(|| lower.strip_prefix("a ")).unwrap_or(&lower);
        let id = norm_id(name);
        [id.clone(), id.trim_end_matches('s').to_owned()].into_iter().find(|i| i.len() <= 40 && self.names.entity(i)).map(|id| Expr::Lit(Value::Text(id)))
    }

    /// `diamond sword`, `3 stone`, `an apple`, `oak logs`.
    fn item(&mut self, text: &str) -> Option<Item> {
        let lower = text.trim().to_ascii_lowercase();
        let (count, name) = match lower.split_once(' ') {
            Some((n, rest)) if n.parse::<u32>().is_ok() => (n.parse::<u32>().ok()?, rest),
            Some(("a" | "an", rest)) => (1, rest),
            _ => (1, lower.as_str()),
        };
        let name = name.strip_prefix("of ").unwrap_or(name);
        if name.is_empty() || name.len() > 60 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | ':')) {
            return None;
        }
        let id = norm_id(name);
        let mut tries = vec![id.clone()];
        for suffix in ["s", "es"] {
            if let Some(s) = id.strip_suffix(suffix) {
                tries.push(s.to_owned());
            }
        }
        if let Some(s) = id.strip_suffix("ies") {
            tries.push(format!("{s}y"));
        }
        if let Some(s) = id.strip_suffix("_block") {
            tries.push(s.to_owned());
        }
        for t in tries.clone() {
            if let Some((_, real)) = ITEM_ALIASES.iter().find(|(a, _)| *a == t) {
                tries.push((*real).to_owned());
            }
        }
        let Some(id) = tries.into_iter().find(|t| self.names.item(t)) else {
            // `diamond sword of sharpness 5 and unbreaking 3`
            let (base, list) = name.split_once(" of ")?;
            let list = enchant_list(list)?;
            let mut item = self.item(&format!("{count} {base}"))?;
            item.ench = list.split(',').filter_map(|e| e.split_once(':')).map(|(id, l)| (id.to_owned(), l.parse().unwrap_or(1))).collect();
            return Some(item);
        };
        Some(Item::new(&id, count))
    }

    fn simple(&mut self, lower: &str) -> Option<Expr> {
        let l = lower.strip_prefix("the ").unwrap_or(lower);
        let ev = |k: &str| Some(Expr::Ev(k.to_owned()));
        match l {
            "true" => return Some(Expr::Lit(Value::Bool(true))),
            "false" => return Some(Expr::Lit(Value::Bool(false))),
            "player" | "event-player" => return ev("player"),
            "victim" | "event-entity" | "entity" => return ev("victim"),
            "event-slot" | "clicked slot" | "index of event-slot" | "index of clicked slot" | "event-slot's index" => return ev("slot"),
            "raw slot" | "clicked raw slot" => return ev("raw slot"),
            "event-inventory" | "clicked inventory" | "current inventory name" | "inventory name" => return ev("inventory"),
            "clicked item" => return ev("item"),
            "click type" | "click action" => return ev("click type"),
            "attacker" | "damager" => return ev("attacker"),
            "event-block" | "block" | "clicked block" => return ev("block"),
            "event-location" => return ev("location"),
            "event-item" | "item" | "event-itemtype" => return ev("item"),
            "message" | "chat message" => return ev("message"),
            "command" | "command label" => return ev("command"),
            "full command" | "complete command" | "whole command" => return ev("full command"),
            "event-world" | "world" => return ev("world"),
            "damage" | "final damage" => return ev("damage"),
            "damage cause" => return ev("cause"),
            "sender" | "command sender" | "executor" | "command executor" => return ev("sender"),
            "death message" => return ev("death message"),
            "join message" | "log in message" | "login message" => return ev("join message"),
            "quit message" | "leave message" | "log out message" => return ev("quit message"),
            "remaining time" => return ev("remaining time"),
            "elapsed time" => return ev("elapsed time"),
            "cooldown time" => return ev("cooldown time"),
            "past world" | "previous world" | "former world" => return ev("past world"),
            "event-gamemode" | "new gamemode" => return ev("gamemode"),
            "console" | "server" => return Some(Expr::Lit(Value::Console)),
            "nl" | "new line" | "newline" | "line break" => return Some(Expr::Lit(Value::text("\n"))),
            "chat format" => {
                self.note = Some("chat_format");
                return None;
            }
            "now" => return Some(Expr::Op(ExprOp::Now, Vec::new(), String::new())),
            "tps" => return Some(Expr::Op(ExprOp::Tps, Vec::new(), String::new())),
            "all players" | "players" | "all the players" | "all of the players" | "online players" | "all online players" => {
                return Some(Expr::Op(ExprOp::AllPlayers, Vec::new(), String::new()));
            }
            "arguments" | "args" | "all arguments" => {
                // in `on command` it is the text after the command
                return Some(if self.in_command { Expr::Arg(0, None) } else { Expr::Ev("arguments".into()) });
            }
            // the money of whoever the event is about
            "balance" | "money" => return Some(Expr::Prop(Box::new(Expr::Ev("player".into())), Prop::Balance)),
            _ => {}
        }
        // arg-1, arg 1, argument-1, 1st argument, arg-text, player-argument
        let ordinal = |w: &str| -> Option<usize> {
            match w {
                "first" => Some(1),
                "second" => Some(2),
                "third" => Some(3),
                "fourth" => Some(4),
                "fifth" => Some(5),
                "last" => None,
                w => w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok().filter(|_| w.len() <= 4),
            }
        };
        for prefix in ["arg-", "arg ", "argument-", "argument "] {
            if let Some(rest) = l.strip_prefix(prefix) {
                if !self.in_command {
                    return None;
                }
                if let Ok(n) = rest.parse::<usize>() {
                    return (1..=32).contains(&n).then_some(Expr::Arg(n, None));
                }
                return Self::arg_type(rest).map(|_| Expr::Arg(0, Some(rest.replace(' ', ""))));
            }
        }
        for suffix in ["-argument", " argument", "-arg", " arg"] {
            if let Some(rest) = l.strip_suffix(suffix) {
                if !self.in_command {
                    return None;
                }
                if let Some(n) = ordinal(rest) {
                    return Some(Expr::Arg(n, None));
                }
                return Self::arg_type(rest).map(|_| Expr::Arg(0, Some(rest.replace(' ', ""))));
            }
        }
        if let Some(rest) = l.strip_prefix("loop-") {
            if self.loops.iter().all(|c| !c.counted) {
                return None;
            }
            let counted = self.loops.iter().filter(|c| c.counted).count();
            let (what, nth) = match rest.rsplit_once('-') {
                Some((w, n)) if n.parse::<usize>().is_ok() => (w, n.parse::<usize>().ok()?),
                _ => (rest, 0),
            };
            if nth > counted {
                return None;
            }
            let kind = match what {
                "index" => 1,
                "iteration" | "counter" => 2,
                w if w.chars().all(|c| c.is_ascii_alphabetic()) => 0,
                _ => return None,
            };
            return Some(Expr::Loop(kind, nth));
        }
        None
    }

    fn call(&mut self, text: &str) -> Option<Expr> {
        let open = text.find('(')?;
        let name = text[..open].to_ascii_lowercase();
        if name.is_empty() || !text.ends_with(')') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || name.chars().next()?.is_ascii_digit() {
            return None;
        }
        let input = Input::new(text);
        if (open + 1..text.len() - 1).any(|i| input.is_top(i)) {
            return None;
        }
        let inner = &text[open + 1..text.len() - 1];
        let builtin = matches!(
            name.as_str(),
            "round" | "floor" | "ceil" | "ceiling" | "abs" | "sqrt" | "min" | "max" | "mod" | "location" | "world" | "sum" | "player" | "offlineplayer" | "pow"
        );
        if !builtin && !self.functions.contains(&name) {
            if name == "vector" {
                self.note = Some("vector");
            }
            return None;
        }
        let mut args = Vec::new();
        if !inner.trim().is_empty() {
            for a in split_top(inner, &[","]) {
                args.push(self.expr(a)?);
            }
        }
        Some(Expr::Call(name, args))
    }

    fn arithmetic(&mut self, text: &str, input: &Input) -> Option<Expr> {
        let b = text.as_bytes();
        let is_op_at = |i: usize, ops: &[u8]| -> bool {
            if !input.is_top(i) || !ops.contains(&b[i]) || i == 0 || i + 1 >= b.len() {
                return false;
            }
            if b[i] != b'-' {
                return true;
            }
            let (prev, next) = (b[i - 1], b[i + 1]);
            (prev == b' ' && next == b' ') || (matches!(prev, b'}' | b')' | b'0'..=b'9') && matches!(next, b'{' | b'(' | b'0'..=b'9'))
        };
        for (ops, right_assoc) in [(&b"+-"[..], false), (&b"*/"[..], false), (&b"^"[..], true)] {
            let mut spots: Vec<usize> = (1..b.len().saturating_sub(1)).filter(|i| is_op_at(*i, ops)).collect();
            if !right_assoc {
                spots.reverse();
            }
            for i in spots {
                // a `-` right after another operator is a sign, not a subtraction
                let before = text[..i].trim_end();
                if before.is_empty() || before.ends_with(['+', '-', '*', '/', '^', '(']) {
                    continue;
                }
                if let (Some(l), Some(r)) = (self.expr(&text[..i]), self.expr(&text[i + 1..])) {
                    return Some(Expr::Bin(Box::new(l), b[i] as char, Box::new(r)));
                }
            }
        }
        if b[0] == b'-' && text.len() > 1 {
            return self.expr(&text[1..]).map(|e| Expr::Neg(Box::new(e)));
        }
        None
    }

    fn prop_named(&mut self, phrase: &str) -> Option<Prop> {
        let p = phrase.trim().to_ascii_lowercase();
        PROPS.iter().find(|(n, _)| *n == p).map(|(_, prop)| *prop)
    }

    fn property(&mut self, text: &str, input: &Input) -> Option<Expr> {
        // [the] prop of X; `name of player's tool` is the name of (player's tool)
        let lower = text.to_ascii_lowercase();
        let start = if lower.starts_with("the ") { 4 } else { 0 };
        for at in input.find_top(" of ") {
            if at <= start {
                continue;
            }
            if let Some(prop) = self.prop_named(&text[start..at]) {
                if let Some(owner) = self.expr(&text[at + 4..]) {
                    return Some(Expr::Prop(Box::new(owner), prop));
                }
            }
            if self.note.is_some() {
                return None;
            }
        }
        // X's prop
        for at in input.find_top("'s ").into_iter().rev() {
            if let Some(prop) = self.prop_named(&text[at + 3..]) {
                if let Some(owner) = self.expr(&text[..at]) {
                    return Some(Expr::Prop(Box::new(owner), prop));
                }
            }
            if self.note.is_some() {
                return None;
            }
        }
        // time / weather without a world mean the world of the event
        match lower[start..].trim() {
            "time" => Some(Expr::Prop(Box::new(Expr::Ev("world".into())), Prop::Time)),
            "weather" => Some(Expr::Prop(Box::new(Expr::Ev("world".into())), Prop::Weather)),
            l => {
                for (word, prop) in [("time in ", Prop::Time), ("weather in ", Prop::Weather)] {
                    if let Some(rest) = l.strip_prefix(word) {
                        let rest = rest.strip_prefix("world ").unwrap_or(rest);
                        let w = self.expr(&text[text.len() - rest.len()..])?;
                        return Some(Expr::Prop(Box::new(Expr::Op(ExprOp::WorldOf, vec![w], String::new())), prop));
                    }
                }
                None
            }
        }
    }

    fn build_expr(&mut self, kind: X, c: &Caps) -> Option<Expr> {
        let mut args = Vec::new();
        let raw: Vec<Option<&str>> = c.slots.clone();
        let mut tag = String::new();
        match kind {
            X::Parsed => {
                args.push(self.expr(raw[0]?)?);
                tag = raw[1]?.trim().to_ascii_lowercase();
                if !matches!(tag.as_str(), "number" | "integer" | "player" | "offline player" | "offlineplayer" | "text" | "string" | "timespan" | "time span" | "item" | "itemtype" | "item type" | "boolean" | "world") {
                    return None;
                }
                return Some(Expr::Op(ExprOp::ParsedAs, args, tag));
            }
            X::SpanOf => {
                let n = self.expr(raw[0]?)?;
                if matches!(n, Expr::Lit(Value::Item(_) | Value::Text(_))) {
                    return None;
                }
                let unit = match c.mark {
                    1 => "tick",
                    2 => "second",
                    4 => "minute",
                    8 => "hour",
                    16 => "day",
                    _ => "week",
                };
                return Some(Expr::Op(ExprOp::SpanOf, vec![n], unit.to_owned()));
            }
            X::BlockRel(dy) => {
                return Some(Expr::Op(ExprOp::BlockRel, vec![Expr::Lit(Value::Num(f64::from(dy))), self.expr(raw[0]?)?], String::new()));
            }
            X::Case(_) => {
                let op = if c.mark & 1 != 0 { ExprOp::Lower } else { ExprOp::Upper };
                return Some(Expr::Op(op, vec![self.expr(raw[0]?)?], String::from("case")));
            }
            X::Rand(int) => {
                let (a, b) = (self.expr(raw[0]?)?, self.expr(raw[1]?)?);
                return Some(Expr::Op(if int { ExprOp::RandInt } else { ExprOp::RandNum }, vec![a, b], String::new()));
            }
            X::LocationAt => {
                for r in &raw {
                    if let Some(r) = r {
                        args.push(self.expr(r)?);
                    }
                }
                return Some(Expr::Op(ExprOp::LocationAt, args, String::new()));
            }
            X::Replace | X::ReplaceRev => {
                let (w, h) = if kind == X::Replace { (1, 2) } else { (2, 1) };
                let (needle, with, hay) = (self.expr(raw[0]?)?, self.expr(raw[w]?)?, self.expr(raw[h]?)?);
                return Some(Expr::Op(ExprOp::Replace, vec![hay, needle, with], String::new()));
            }
            X::Op(op) => {
                if op == ExprOp::ItemOf {
                    // `{_n} of diamond`
                    let n = self.expr(raw[0]?)?;
                    let item = self.expr(raw[1]?)?;
                    if !self.is_items(&item) || matches!(n, Expr::Lit(Value::Item(_) | Value::Text(_))) || matches!(n, Expr::Text(_)) {
                        return None;
                    }
                    return Some(Expr::Op(op, vec![n, item], String::new()));
                }
                if op == ExprOp::Lower {
                    // `coloured "…"`: colours are always applied when a text is shown
                    return self.expr(raw[0]?);
                }
                for r in &raw {
                    match r {
                        Some(r) => args.push(self.expr(r)?),
                        None if op == ExprOp::TargetBlock => args.push(Expr::Ev("player".into())),
                        None => {}
                    }
                }
                if matches!(op, ExprOp::Named | ExprOp::WithLore) && !self.is_items(&args[0]) {
                    return None;
                }
                if op == ExprOp::Formatted {
                    tag = "date".into();
                }
            }
        }
        let X::Op(op) = kind else { return None };
        Some(Expr::Op(op, args, tag))
    }
}

fn negate(inner: Cond) -> Cond {
    match inner {
        Cond::Cmp(a, op, b) => {
            let flipped = match op {
                CmpOp::Eq => CmpOp::Ne,
                CmpOp::Ne => CmpOp::Eq,
                CmpOp::Gt => CmpOp::Le,
                CmpOp::Ge => CmpOp::Lt,
                CmpOp::Lt => CmpOp::Ge,
                CmpOp::Le => CmpOp::Gt,
            };
            Cond::Cmp(a, flipped, b)
        }
        Cond::Check(k, args, neg) => Cond::Check(k, args, !neg),
        other => other,
    }
}
