//! What the server owner wrote: the settings, the items, the glyphs. No server needed here.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The namespace of everything this plugin puts into the resource pack and onto items.
pub const NS: &str = "customitems";
/// Glyphs get characters from the Unicode private use area: no font draws anything there.
pub const GLYPH_FIRST: u32 = 0xE000;
pub const GLYPH_LAST: u32 = 0xF8FF;
pub const MAX_ITEMS: usize = 5000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// `th` or `en`: the language of the plugin's own messages.
    pub language: String,
    pub pack: PackConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PackConfig {
    /// Send the pack to players when they join.
    pub send: bool,
    /// Where players download the pack from, when it is hosted somewhere else: upload
    /// `pack.zip` from this plugin's folder there after every `/customitems reload`.
    pub url: String,
    /// Or let the plugin serve it: a free TCP port of this server (0 = off) ...
    pub port: u16,
    /// ... and the address players use to reach the server (`play.example.com` or an IP).
    pub host: String,
    /// Players who say no cannot stay (the game asks them to accept or leave).
    pub required: bool,
    /// The question the game shows with the pack. `&` colours work.
    pub prompt: String,
    /// The text of the pack in the game's pack list.
    pub description: String,
    /// Ticks after joining before the pack is sent (20 = one second).
    pub join_delay_ticks: u64,
    /// The pack format range written into `pack.mcmeta`.
    pub format_min: u32,
    pub format_max: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), pack: PackConfig::default() }
    }
}

impl Default for PackConfig {
    fn default() -> Self {
        PackConfig {
            send: true,
            url: String::new(),
            port: 0,
            host: String::new(),
            required: false,
            prompt: "&aเซิร์ฟเวอร์นี้มีไอเท็มพิเศษ &7กด ใช่ เพื่อโหลดภาพของไอเท็ม".into(),
            description: "Custom items of this server".into(),
            join_delay_ticks: 20,
            format_min: 65,
            format_max: 999,
        }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        let p = &mut self.pack;
        p.url = p.url.trim().to_owned();
        if !(p.url.is_empty() || p.url.starts_with("http://") || p.url.starts_with("https://")) || p.url.len() > 500 || p.url.chars().any(char::is_whitespace) {
            p.url.clear();
        }
        // A host as it goes into a URL: a name, an IPv4 address or `[v6]`, nothing else
        p.host = p.host.trim().trim_start_matches("http://").trim_end_matches('/').to_owned();
        if p.host.len() > 253 || !p.host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '[' | ']' | ':')) {
            p.host.clear();
        }
        p.join_delay_ticks = p.join_delay_ticks.min(600);
        p.format_min = p.format_min.clamp(1, 9999);
        p.format_max = p.format_max.clamp(p.format_min, 9999);
        p.description = p.description.chars().filter(|c| !c.is_control()).take(120).collect();
        p.prompt = p.prompt.chars().filter(|c| !c.is_control()).take(200).collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    /// The link players get for a pack with this SHA-1, if the owner said where it is.
    pub fn pack_url(&self, sha1: &str) -> Option<String> {
        let p = &self.pack;
        if !p.url.is_empty() {
            return Some(p.url.clone());
        }
        // The hash in the path: a changed pack is a new link, so nothing serves a stale copy
        (p.port != 0 && !p.host.is_empty()).then(|| format!("http://{}:{}/{sha1}.zip", p.host, p.port))
    }
}

/// What a recipe takes: an item of the game (`minecraft:diamond`, as it comes, never one of
/// ours made of it) or one of our own items, by id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Game(String),
    Ours(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ingredient {
    pub part: Part,
    pub count: u32,
}

/// How an item is made in the crafting window of this plugin. (The crafting table cannot do
/// it: Pumpkin 0.2.2 keeps only the kind and the number of a recipe's result, so what comes out
/// of a table is a bare item of the game, without the look and the id that make it ours.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub ingredients: Vec<Ingredient>,
    /// How many items one crafting gives.
    pub count: u8,
}

pub const MAX_INGREDIENTS: usize = 9;

/// How an item looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// Like the item of the game it is made of.
    Plain,
    /// A picture in `textures/`; `handheld` holds it like a tool.
    Texture { file: String, handheld: bool },
    /// A model that exists already: one of the game or one in `pack/`.
    Model(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    /// The item of the game it really is.
    pub material: String,
    pub name: String,
    pub lore: Vec<String>,
    pub look: Look,
    /// Always shimmer like an enchanted item.
    pub glint: bool,
    /// 0 = as the material.
    pub max_stack: u8,
    pub recipes: Vec<Recipe>,
}

impl Item {
    /// The model the item carries, if it has a look of its own.
    pub fn model_id(&self) -> Option<String> {
        (self.look != Look::Plain).then(|| format!("{NS}:{}", self.id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub name: String,
    /// A picture in `textures/`.
    pub file: String,
    /// How tall it is drawn and how far above the line it starts, in text pixels.
    pub height: i32,
    pub ascent: i32,
}

/// Something in a file the owner should look at. Both languages, the reader picks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub th: String,
    pub en: String,
}

impl Problem {
    pub fn new(place: &str, th: impl AsRef<str>, en: impl AsRef<str>) -> Problem {
        Problem { th: format!("{place}: {}", th.as_ref()), en: format!("{place}: {}", en.as_ref()) }
    }

    pub fn text(&self, thai: bool) -> &str {
        if thai { &self.th } else { &self.en }
    }
}

/// Names that become file names and resource locations: lowercase letters, digits, `_`.
pub fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `namespace:path` as the game accepts it.
pub fn valid_location(s: &str) -> bool {
    let Some((ns, path)) = s.split_once(':') else { return false };
    let ok = |part: &str, slash: bool| !part.is_empty() && part.len() <= 120 && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.') || (slash && b == b'/'));
    ok(ns, false) && ok(path, true) && !path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
}

/// A picture inside `textures/`: a relative path of plain names ending in `.png`.
pub fn valid_picture(file: &str) -> bool {
    file.len() <= 120
        && file.ends_with(".png")
        && !file.starts_with('/')
        && file.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
}

/// `diamond` and `minecraft:diamond` are the same item.
fn item_name(s: &str) -> Option<String> {
    let full = if s.contains(':') { s.to_owned() } else { format!("minecraft:{s}") };
    valid_location(&full).then_some(full)
}

/// `4 minecraft:redstone`, `minecraft:stick` (one of it) or `2 ruby`. A bare name is taken for
/// one of ours here; [`merge_items`] turns it into an item of the game when we have none of
/// that name.
fn ingredient(text: &str) -> Option<Ingredient> {
    let text = text.trim();
    let (count, name) = match text.split_once(' ') {
        Some((n, rest)) => (n.parse::<u32>().ok()?, rest.trim()),
        None => (1, text),
    };
    if !(1..=2304).contains(&count) {
        return None;
    }
    let part = if name.contains(':') { Part::Game(item_name(name)?) } else { valid_id(name).then(|| Part::Ours(name.to_owned()))? };
    Some(Ingredient { part, count })
}

fn text_of(v: &toml::Value, max: usize) -> Option<String> {
    v.as_str().map(|s| s.chars().filter(|c| !c.is_control()).take(max).collect())
}

fn recipe(place: &str, t: &toml::Table, out: &mut Vec<Problem>) -> Option<Recipe> {
    if ["shape", "shapeless", "smelt"].iter().any(|k| t.contains_key(*k)) {
        out.push(Problem::new(
            place,
            "สูตรของโต๊ะคราฟและเตายังทำไม่ได้บน Pumpkin รุ่นนี้ ใช้ ingredients = [\"4 minecraft:redstone\"] แทน (คราฟจาก /craft)",
            "crafting table and furnace recipes cannot be done on this Pumpkin yet: use ingredients = [\"4 minecraft:redstone\"] (crafted from /craft)",
        ));
        return None;
    }
    let list = t.get("ingredients").and_then(toml::Value::as_array);
    let texts: Vec<&str> = list.into_iter().flatten().filter_map(toml::Value::as_str).collect();
    if texts.is_empty() || Some(texts.len()) != list.map(Vec::len) {
        out.push(Problem::new(place, "สูตรต้องมี ingredients เป็นรายการข้อความ เช่น [\"4 minecraft:redstone\", \"2 ruby\"]", "a recipe needs ingredients, a list of texts such as [\"4 minecraft:redstone\", \"2 ruby\"]"));
        return None;
    }
    let mut ingredients: Vec<Ingredient> = Vec::new();
    for text in texts {
        let Some(found) = ingredient(text) else {
            out.push(Problem::new(place, format!("วัตถุดิบ \"{text}\" ต้องเป็นแบบ \"4 minecraft:redstone\" หรือ \"2 ชื่อไอเท็มของเรา\""), format!("ingredient \"{text}\" must look like \"4 minecraft:redstone\" or \"2 one_of_our_items\"")));
            return None;
        };
        // The same thing named twice is one line of the recipe
        match ingredients.iter_mut().find(|i| i.part == found.part) {
            Some(have) => have.count = (have.count + found.count).min(2304),
            None => ingredients.push(found),
        }
    }
    if ingredients.len() > MAX_INGREDIENTS {
        out.push(Problem::new(place, format!("สูตรมีวัตถุดิบได้ไม่เกิน {MAX_INGREDIENTS} อย่าง"), format!("a recipe may have at most {MAX_INGREDIENTS} different ingredients")));
        return None;
    }
    Some(Recipe { ingredients, count: t.get("count").and_then(toml::Value::as_integer).unwrap_or(1).clamp(1, 99) as u8 })
}

const ITEM_KEYS: [&str; 10] = ["material", "name", "lore", "texture", "model", "handheld", "glint", "max-stack", "recipe", "recipes"];
/// Kinds of content that are planned but not in this version: named so the owner knows why
/// nothing happens instead of reading "unknown key".
const LATER_KEYS: [&str; 3] = ["block", "furniture", "armor"];

/// Read one `items/*.toml`: every table is an item, its name is the id.
pub fn parse_items(file: &str, text: &str, out: &mut Vec<Problem>) -> Vec<Item> {
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            let why: String = e.message().chars().take(160).collect();
            out.push(Problem::new(file, format!("อ่านไฟล์ไม่ได้: {why}"), format!("cannot read the file: {why}")));
            return Vec::new();
        }
    };
    let mut items = Vec::new();
    for (id, value) in &table {
        let place = format!("{file} [{id}]");
        let Some(t) = value.as_table() else {
            out.push(Problem::new(&place, "ต้องเป็นตาราง เช่น [ruby]", "must be a table such as [ruby]"));
            continue;
        };
        if !valid_id(id) {
            out.push(Problem::new(&place, "ชื่อใช้ได้แค่ a-z 0-9 และ _ ยาวไม่เกิน 48", "the name may only use a-z, 0-9 and _, at most 48 long"));
            continue;
        }
        for key in t.keys() {
            if LATER_KEYS.contains(&key.as_str()) {
                out.push(Problem::new(&place, format!("\"{key}\" ยังไม่มีในรุ่นนี้ จึงถูกข้าม"), format!("\"{key}\" is not in this version yet and was skipped")));
            } else if !ITEM_KEYS.contains(&key.as_str()) {
                out.push(Problem::new(&place, format!("ไม่รู้จัก \"{key}\""), format!("unknown key \"{key}\"")));
            }
        }
        let material = match t.get("material").and_then(toml::Value::as_str) {
            None => "minecraft:paper".to_owned(),
            Some(m) => match item_name(m) {
                Some(full) => full,
                None => {
                    out.push(Problem::new(&place, format!("material \"{m}\" ไม่ใช่ชื่อไอเท็ม"), format!("material \"{m}\" is not an item name")));
                    continue;
                }
            },
        };
        let look = match (t.get("texture").and_then(toml::Value::as_str), t.get("model").and_then(toml::Value::as_str)) {
            (Some(_), Some(_)) => {
                out.push(Problem::new(&place, "ใส่ texture หรือ model อย่างใดอย่างหนึ่ง", "give either texture or model, not both"));
                continue;
            }
            (Some(file), None) if valid_picture(file) => Look::Texture { file: file.to_owned(), handheld: t.get("handheld").and_then(toml::Value::as_bool).unwrap_or(false) },
            (Some(file), None) => {
                out.push(Problem::new(&place, format!("texture \"{file}\" ต้องเป็นไฟล์ .png ใน textures/"), format!("texture \"{file}\" must be a .png file in textures/")));
                continue;
            }
            (None, Some(model)) if valid_location(model) => Look::Model(model.to_owned()),
            (None, Some(model)) => {
                out.push(Problem::new(&place, format!("model \"{model}\" ต้องเป็นแบบ namespace:path"), format!("model \"{model}\" must look like namespace:path")));
                continue;
            }
            (None, None) => Look::Plain,
        };
        let mut recipes = Vec::new();
        if let Some(one) = t.get("recipe").and_then(toml::Value::as_table) {
            recipes.extend(recipe(&place, one, out));
        }
        for one in t.get("recipes").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table) {
            recipes.extend(recipe(&place, one, out));
        }
        recipes.truncate(8);
        items.push(Item {
            id: id.clone(),
            material,
            name: t.get("name").and_then(|v| text_of(v, 120)).unwrap_or_default(),
            lore: t.get("lore").and_then(toml::Value::as_array).map(|a| a.iter().filter_map(|v| text_of(v, 160)).take(24).collect()).unwrap_or_default(),
            look,
            glint: t.get("glint").and_then(toml::Value::as_bool).unwrap_or(false),
            max_stack: t.get("max-stack").and_then(toml::Value::as_integer).map_or(0, |n| n.clamp(1, 99) as u8),
            recipes,
        });
    }
    items
}

/// Put the items of all files together: sorted by id, the first one of a name wins.
pub fn merge_items(mut all: Vec<Item>, out: &mut Vec<Problem>) -> Vec<Item> {
    all.sort_by(|a, b| a.id.cmp(&b.id));
    let before = all.len();
    all.dedup_by(|later, first| {
        let same = later.id == first.id;
        if same {
            out.push(Problem::new(&later.id, "มีชื่อนี้มากกว่าหนึ่งที่ ใช้อันแรก", "this name is defined more than once, the first one is used"));
        }
        same
    });
    if all.len() > MAX_ITEMS {
        out.push(Problem::new("items", format!("มี {before} ชิ้น ใช้ได้ {MAX_ITEMS} ชิ้นแรก"), format!("{before} items, only the first {MAX_ITEMS} are used")));
        all.truncate(MAX_ITEMS);
    }
    // A bare ingredient name that is none of ours is an item of the game: `stick` = `minecraft:stick`
    let ours: Vec<String> = all.iter().map(|i| i.id.clone()).collect();
    for recipe in all.iter_mut().flat_map(|i| i.recipes.iter_mut()) {
        let mut parts: Vec<Ingredient> = Vec::with_capacity(recipe.ingredients.len());
        for mut ingredient in std::mem::take(&mut recipe.ingredients) {
            if let Part::Ours(name) = &ingredient.part {
                if ours.binary_search(name).is_err() {
                    ingredient.part = Part::Game(format!("minecraft:{name}"));
                }
            }
            // `stick` and `minecraft:stick` are one line of the recipe
            match parts.iter_mut().find(|p| p.part == ingredient.part) {
                Some(have) => have.count = (have.count + ingredient.count).min(2304),
                None => parts.push(ingredient),
            }
        }
        recipe.ingredients = parts;
    }
    all
}

/// Read `glyphs.toml`: every table is a glyph.
pub fn parse_glyphs(text: &str, out: &mut Vec<Problem>) -> Vec<Glyph> {
    let file = "glyphs.toml";
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            let why: String = e.message().chars().take(160).collect();
            out.push(Problem::new(file, format!("อ่านไฟล์ไม่ได้: {why}"), format!("cannot read the file: {why}")));
            return Vec::new();
        }
    };
    let mut glyphs = Vec::new();
    for (name, value) in &table {
        let place = format!("{file} [{name}]");
        let Some(t) = value.as_table() else { continue };
        if !valid_id(name) {
            out.push(Problem::new(&place, "ชื่อใช้ได้แค่ a-z 0-9 และ _ ยาวไม่เกิน 48", "the name may only use a-z, 0-9 and _, at most 48 long"));
            continue;
        }
        let Some(file) = t.get("texture").and_then(toml::Value::as_str).filter(|f| valid_picture(f)) else {
            out.push(Problem::new(&place, "ต้องมี texture เป็นไฟล์ .png ใน textures/", "needs texture, a .png file in textures/"));
            continue;
        };
        let height = t.get("height").and_then(toml::Value::as_integer).unwrap_or(9).clamp(1, 256) as i32;
        // The game refuses a glyph that starts higher than it is tall
        let ascent = (t.get("ascent").and_then(toml::Value::as_integer).unwrap_or(8).clamp(-256, 256) as i32).min(height);
        glyphs.push(Glyph { name: name.clone(), file: file.to_owned(), height, ascent });
    }
    glyphs.sort_by(|a, b| a.name.cmp(&b.name));
    glyphs
}

/// Give every glyph its character. A glyph keeps the one it had (`known`, from the last run):
/// texts that were typed with it keep working when glyphs are added or removed.
pub fn assign_glyphs(glyphs: &[Glyph], known: &BTreeMap<String, u32>, out: &mut Vec<Problem>) -> BTreeMap<String, u32> {
    let mut taken: Vec<u32> = known.values().copied().filter(|c| (GLYPH_FIRST..=GLYPH_LAST).contains(c)).collect();
    taken.sort_unstable();
    let mut map = BTreeMap::new();
    let mut next = GLYPH_FIRST;
    for g in glyphs {
        if let Some(c) = known.get(&g.name).copied().filter(|c| (GLYPH_FIRST..=GLYPH_LAST).contains(c)) {
            map.insert(g.name.clone(), c);
            continue;
        }
        while taken.binary_search(&next).is_ok() {
            next += 1;
        }
        if next > GLYPH_LAST {
            out.push(Problem::new("glyphs.toml", "glyph เต็มแล้ว (6,400 ตัว)", "no characters left for glyphs (6,400)"));
            break;
        }
        map.insert(g.name.clone(), next);
        next += 1;
    }
    map
}

/// The file written on the first start: the owner sees the shape of an item without reading docs.
pub const EXAMPLE_ITEMS: &str = r##"# CustomItems: one table per item. The table's name is the item's id (a-z, 0-9, _).
# Remove the "#" in front of an example to try it, then run /customitems reload.
#
# [ruby]
# name = "&cRuby"                      # & colours work
# lore = ["&7A red gem"]
# texture = "ruby.png"                 # a picture in the textures/ folder next to this one
# glint = false                        # true = shimmer like an enchanted item
# max-stack = 64
#
# [ruby.recipe]                        # players make it with /craft: 4 redstone give 1 ruby
# ingredients = ["4 minecraft:redstone"]
# count = 1
#
# [ruby_sword]
# material = "minecraft:iron_sword"    # the item of the game it really is (default: paper)
# name = "&cRuby Sword"
# texture = "ruby_sword.png"
# handheld = true                      # held like a tool
#
# [ruby_sword.recipe]                  # an item of the game and two of our own items
# ingredients = ["minecraft:iron_sword", "2 ruby"]
#
# [old_coin]
# name = "&6Old Coin"
# model = "minecraft:item/gold_nugget" # a model that exists: one of the game or one in pack/
#
# [[old_coin.recipes]]                 # several recipes: use [[id.recipes]] more than once
# ingredients = ["9 minecraft:gold_nugget"]
#
# [[old_coin.recipes]]
# ingredients = ["minecraft:gold_ingot"]
# count = 2
"##;

pub const EXAMPLE_GLYPHS: &str = r##"# CustomItems: pictures that can be typed as a character into any text (chat, signs, names).
# One table per glyph. %customitems_<name>% gives the character (with the Placeholders plugin),
# /customitems glyphs lists them.
#
# [coin]
# texture = "coin.png"   # a picture in the textures/ folder
# height = 9             # how tall it is drawn; a line of text is 9 tall
# ascent = 8             # how far above the line it starts
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn items(text: &str) -> (Vec<Item>, Vec<Problem>) {
        let mut out = Vec::new();
        let list = parse_items("items/test.toml", text, &mut out);
        (merge_items(list, &mut out), out)
    }

    #[test]
    fn an_item_is_read_with_its_defaults() {
        let (list, problems) = items("[ruby]\nname = \"&cRuby\"\ntexture = \"gems/ruby.png\"\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(list.len(), 1);
        let ruby = &list[0];
        assert_eq!((ruby.id.as_str(), ruby.material.as_str(), ruby.max_stack, ruby.glint), ("ruby", "minecraft:paper", 0, false));
        assert_eq!(ruby.look, Look::Texture { file: "gems/ruby.png".into(), handheld: false });
        assert_eq!(ruby.model_id().as_deref(), Some("customitems:ruby"));
    }

    #[test]
    fn the_example_file_is_valid_once_uncommented() {
        let text: String = EXAMPLE_ITEMS.lines().map(|l| l.strip_prefix("# ").or_else(|| l.strip_prefix('#')).unwrap_or(l)).filter(|l| l.starts_with('[') || l.contains(" = ")).collect::<Vec<_>>().join("\n");
        let (list, problems) = items(&text);
        assert!(problems.is_empty(), "{problems:?}");
        let ids: Vec<&str> = list.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["old_coin", "ruby", "ruby_sword"]);
        assert_eq!(list[0].look, Look::Model("minecraft:item/gold_nugget".into()));
        let game = |name: &str, count: u32| Ingredient { part: Part::Game(name.into()), count };
        assert_eq!(list[0].recipes, [Recipe { ingredients: vec![game("minecraft:gold_nugget", 9)], count: 1 }, Recipe { ingredients: vec![game("minecraft:gold_ingot", 1)], count: 2 }]);
        assert_eq!(list[1].recipes, [Recipe { ingredients: vec![game("minecraft:redstone", 4)], count: 1 }]);
        assert_eq!(list[2].recipes, [Recipe { ingredients: vec![game("minecraft:iron_sword", 1), Ingredient { part: Part::Ours("ruby".into()), count: 2 }], count: 1 }]);
        assert_eq!(list[2].material, "minecraft:iron_sword");
        assert_eq!(list[2].look, Look::Texture { file: "ruby_sword.png".into(), handheld: true });

        let mut out = Vec::new();
        let glyphs: String = EXAMPLE_GLYPHS.lines().filter_map(|l| l.strip_prefix("# ")).filter(|l| l.starts_with('[') || l.contains(" = ")).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_glyphs(&glyphs, &mut out), [Glyph { name: "coin".into(), file: "coin.png".into(), height: 9, ascent: 8 }]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn what_cannot_work_is_said_and_skipped() {
        let (list, problems) = items(
            r#"
            [Bad-Name]
            name = "x"
            [both]
            texture = "a.png"
            model = "minecraft:item/stick"
            [escape]
            texture = "../../config.png"
            [nomodel]
            model = "stick"
            [later]
            name = "A block"
            block = true
            typo = 1
            [craft]
            name = "x"
            [craft.recipe]
            shape = ["AA", "AA"]
            key = { A = "minecraft:redstone" }
            [craft2]
            [craft2.recipe]
            ingredients = ["two minecraft:stick"]
            [craft3]
            [craft3.recipe]
            ingredients = []
            [fine]
            name = "ok"
            [fine.recipe]
            ingredients = ["2 stick", "3 minecraft:stick", "fine", "0 minecraft:dirt"]
            "#,
        );
        let ids: Vec<&str> = list.iter().map(|i| i.id.as_str()).collect();
        // Items whose look is wrong are left out; a bad recipe only loses the recipe
        assert_eq!(ids, ["craft", "craft2", "craft3", "fine", "later"]);
        assert!(list.iter().all(|i| i.recipes.is_empty()));
        let text: Vec<&str> = problems.iter().map(|p| p.text(false)).collect();
        for needle in ["[Bad-Name]", "either texture or model", "[escape]", "namespace:path", "\"block\" is not in this version", "unknown key \"typo\"", "crafting table and furnace recipes", "\"two minecraft:stick\" must look like", "needs ingredients", "\"0 minecraft:dirt\""] {
            assert!(text.iter().any(|t| t.contains(needle)), "{needle}: {text:?}");
        }
        assert!(problems.iter().all(|p| !p.th.is_empty() && p.th != p.en));
    }

    #[test]
    fn ingredients_are_counted_and_told_apart() {
        let (list, problems) = items(
            r#"
            [gem]
            [ring]
            [ring.recipe]
            ingredients = ["2 gem", "stick", "3 minecraft:stick", "minecraft:gold_ingot"]
            count = 200
            "#,
        );
        assert!(problems.is_empty(), "{problems:?}");
        // `gem` is ours; `stick` is not, so it is the game's and joins the other sticks
        assert_eq!(
            list[1].recipes,
            [Recipe {
                ingredients: vec![Ingredient { part: Part::Ours("gem".into()), count: 2 }, Ingredient { part: Part::Game("minecraft:stick".into()), count: 4 }, Ingredient { part: Part::Game("minecraft:gold_ingot".into()), count: 1 }],
                count: 99
            }]
        );
    }

    #[test]
    fn a_name_used_twice_keeps_the_first() {
        let mut out = Vec::new();
        let mut all = parse_items("items/a.toml", "[gem]\nname = \"first\"\n", &mut out);
        all.extend(parse_items("items/b.toml", "[gem]\nname = \"second\"\n", &mut out));
        let merged = merge_items(all, &mut out);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "first");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn glyphs_keep_their_characters() {
        let glyph = |n: &str| Glyph { name: n.into(), file: format!("{n}.png"), height: 9, ascent: 8 };
        let mut out = Vec::new();
        let first = assign_glyphs(&[glyph("coin"), glyph("heart")], &BTreeMap::new(), &mut out);
        assert_eq!(first, BTreeMap::from([("coin".to_owned(), 0xE000), ("heart".to_owned(), 0xE001)]));
        // "coin" is taken out and "axe" added: "heart" stays where it was, and so would "coin"
        // if it came back, so "axe" gets a character nobody had
        let known = first.clone();
        let second = assign_glyphs(&[glyph("axe"), glyph("heart")], &known, &mut out);
        assert_eq!(second, BTreeMap::from([("axe".to_owned(), 0xE002), ("heart".to_owned(), 0xE001)]));
        assert!(out.is_empty());
    }

    #[test]
    fn the_pack_link_is_only_given_when_it_can_work() {
        let mut cfg = Config::default().cleaned();
        assert_eq!(cfg.pack_url("abc"), None);
        cfg.pack.port = 8123;
        assert_eq!(cfg.pack_url("abc"), None, "a port without the address players use");
        cfg.pack.host = "http://play.example.com/".into();
        let cfg = cfg.cleaned();
        assert_eq!(cfg.pack_url("abc").as_deref(), Some("http://play.example.com:8123/abc.zip"));
        let mut hosted = cfg.clone();
        hosted.pack.url = "https://cdn.example.com/pack.zip".into();
        assert_eq!(hosted.cleaned().pack_url("abc").as_deref(), Some("https://cdn.example.com/pack.zip"));
        let mut bad = cfg;
        bad.pack.url = "ftp://x/y".into();
        bad.pack.host = "bad host/../x".into();
        assert_eq!(bad.cleaned().pack_url("abc"), None);
    }

    #[test]
    fn names_and_paths_are_checked() {
        assert!(valid_id("ruby_sword2") && !valid_id("Ruby") && !valid_id("") && !valid_id("a-b"));
        assert!(valid_location("minecraft:item/stick") && valid_location("my_pack:block/a.b-c"));
        for bad in ["stick", ":x", "a:", "a:b//c", "a:../b", "A:b", "a:b c"] {
            assert!(!valid_location(bad), "{bad}");
        }
        assert!(valid_picture("ruby.png") && valid_picture("gems/Ruby-1.png"));
        for bad in ["ruby.jpg", "/ruby.png", "../ruby.png", "a/../b.png", "a//b.png", "a b.png", ".png/"] {
            assert!(!valid_picture(bad), "{bad}");
        }
    }
}
