//! Pumpkin WASM glue for Mobs: making the creatures, giving them their numbers and behaviour,
//! running their skills, handing out what they drop, and keeping spawners filled.
//!
//! A mob of this plugin is a real creature of the game that carries our tag. It lives as long
//! as the server runs and players are near: when nobody is within `despawn-range` it is taken
//! out of the world, and a spawner makes a new one when players are back. Nothing is carried
//! over a restart; what an earlier run left in the world is removed when it is seen.
//!
//! Pumpkin 0.2.2 does not say who hurt a creature, so the player closest to it when it was
//! last hit counts as the one who killed it.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mcsv_common as c;
use pumpkin_plugin_api::boss_bar::{BossBar, BossBarColor, BossBarDivision};
use pumpkin_plugin_api::command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::common::GameMode;
use pumpkin_plugin_api::events::{EntityDamageEvent, EntityDeathEvent, EventHandler, EventPriority, FromIntoEvent};
use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::permissions::{FS_READ_DATA, FS_WRITE_DATA};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::wit::pumpkin::plugin::common::{NbtTag, NbtTree};
use pumpkin_plugin_api::wit::pumpkin::plugin::server::CommandSender as ServerSender;
use pumpkin_plugin_api::wit::pumpkin::plugin::sounds::SoundCategory;
use pumpkin_plugin_api::wit::pumpkin::plugin::status_effect::StatusEffectInstance;
use pumpkin_plugin_api::wit::pumpkin::plugin::world::{BuiltinAiGoal, Entity, EquipmentSlot, LivingEntity, Mob, World};
use pumpkin_plugin_api::{Attribute, Context, DamageType, ItemStack, Plugin, PluginMetadata, Server, register_plugin};

use crate::defs::{self, Action, Config, Goal, MobDef, NS, Note, Rng, Spawner, Trigger};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const P: &str = "&8[&cMobs&8]&r ";
const SPAWNERS_FILE: &str = "spawners.json";
/// The tag on every creature this plugin makes.
const MARK: &str = "mob";
/// Ticks between two looks at every mob (targets, timers, who is near).
const LOOK_EVERY: u64 = 10;
/// Ticks between two looks for creatures an earlier run left in the world.
const SWEEP_EVERY: u64 = 6000;
/// How long (ticks) the player closest to a mob when it was hit counts as the one fighting it.
const FIGHT_TICKS: u64 = 200;

struct Live {
    def: Arc<MobDef>,
    entity: Entity,
    id: i32,
    living: LivingEntity,
    mob: Option<Mob>,
    world: String,
    /// The model the Models plugin put on it.
    model: Option<u64>,
    spawner: Option<String>,
    born: u64,
    max_health: f32,
    health: f32,
    shown: String,
    /// By skill: the tick from which it may happen again (its cooldown).
    ready: Vec<u64>,
    /// By skill: the tick a `timer` skill is next due.
    timer: Vec<u64>,
    /// By skill: a `health` skill that has happened.
    fired: Vec<bool>,
    bar: Option<BossBar>,
    bar_players: Vec<String>,
    /// Who was closest when it was last hit, and when.
    fighter: Option<(String, u64)>,
    targeting: bool,
    lived: i32,
    stale: u8,
}

struct Place {
    def: Spawner,
    next: u64,
}

/// A mob to make as soon as nothing else is being worked on.
struct Summon {
    def: Arc<MobDef>,
    world: World,
    at: (f64, f64, f64),
    spawner: Option<String>,
}

/// A player and where they are, looked up once for a whole pass.
struct Near {
    player: Player,
    name: String,
    world: String,
    at: (f64, f64, f64),
    /// Survival or adventure: can be hurt and gone after.
    fair: bool,
}

#[derive(Default)]
struct Stat {
    ticks: u64,
    us: u64,
    spawned: u64,
    died: u64,
    skills: u64,
}

struct State {
    cfg: Config,
    defs: Vec<Arc<MobDef>>,
    notes: Vec<Note>,
    lives: Vec<Live>,
    places: Vec<Place>,
    queue: Vec<Summon>,
    /// Mobs another plugin asked for: (mob, world, place).
    wanted: Vec<(String, String, (f64, f64, f64))>,
    /// The models the Models plugin has (`None`: it is not installed, or not asked yet).
    models: Option<Vec<String>>,
    tick: u64,
    rng: Rng,
    stat: Stat,
    swept: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Never blocks: WASM has one thread and Pumpkin calls handlers from inside host calls.
fn try_lock() -> Option<std::sync::MutexGuard<'static, Option<State>>> {
    match STATE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn msg(th: bool, thai: &str, english: &str) -> String {
    format!("{P}{}", if th { thai } else { english })
}

/// What this run of the server writes on its creatures: the time it started.
static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn run_mark() -> &'static str {
    RUN.get_or_init(|| c::now_secs().to_string())
}

/// A creature of ours from an earlier run: it has the looks but nothing drives it any more.
fn leftover(entity: &Entity) -> bool {
    match entity.get_custom_data(NS, MARK) {
        Some(tree) => !matches!(tree.tags.get(tree.root as usize), Some(NbtTag::StringTag(s)) if s == run_mark()),
        None => false,
    }
}

// ---- reading the owner's files ------------------------------------------------

fn load() -> (Config, Vec<Arc<MobDef>>, Vec<Note>) {
    let cfg = c::load_config::<Config>("config.toml").cleaned();
    let mut notes = Vec::new();
    let first = !c::data_path("mobs").exists();
    let _ = std::fs::create_dir_all(c::data_path("mobs"));
    if first {
        let _ = std::fs::write(c::data_path("mobs/example.toml"), defs::EXAMPLE_MOBS);
    }
    let mut names: Vec<String> = std::fs::read_dir(c::data_path("mobs")).map(|dir| dir.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".toml")).collect()).unwrap_or_default();
    names.sort();
    let mut all = Vec::new();
    for name in names {
        let file = format!("mobs/{name}");
        match std::fs::read_to_string(c::data_path(&file)) {
            Ok(text) => all.extend(defs::parse_mobs(&file, &text, &mut notes)),
            Err(_) => notes.push(Note::new(&file, "อ่านไฟล์ไม่ได้ (ต้องเป็นข้อความ UTF-8)", "cannot be read (it must be UTF-8 text)")),
        }
    }
    let mut mobs = defs::merge_mobs(all, &mut notes);
    let real_item = |item: &str| !ItemStack::new(item, 1).get_registry_key().ends_with("air");
    mobs.retain(|m| {
        let known = c::tables::entity_type(&m.base).is_some();
        if !known {
            notes.push(Note::new(&m.id, format!("ไม่มี mob ชนิด {} ในเกม", m.base), format!("the game has no creature {}", m.base)));
        }
        known
    });
    for m in &mut mobs {
        let id = m.id.clone();
        m.equipment.retain(|(_, item)| {
            let ok = real_item(item);
            if !ok {
                notes.push(Note::new(&id, format!("ไม่มีไอเท็ม {item} ในเกม"), format!("the game has no item {item}")));
            }
            ok
        });
        m.drops.retain(|d| {
            // An item of another plugin is that plugin's to know
            let ok = !d.item.starts_with("minecraft:") || real_item(&d.item);
            if !ok {
                notes.push(Note::new(&id, format!("ไม่มีไอเท็ม {} ในเกม", d.item), format!("the game has no item {}", d.item)));
            }
            ok
        });
    }
    (cfg, mobs.into_iter().map(Arc::new).collect(), notes)
}

fn find_def(st: &State, id: &str) -> Option<Arc<MobDef>> {
    st.defs.iter().find(|d| d.id == id).cloned()
}

fn save_places(st: &State) {
    c::save_json(SPAWNERS_FILE, &st.places.iter().map(|p| p.def.clone()).collect::<Vec<Spawner>>());
}

/// Ask the Models plugin which models it has, and say which mobs name one it does not have.
fn ask_models(st: &mut State) {
    let answer = c::ipc("models", &serde_json::json!({ "kind": "models" }));
    st.models = answer.as_ref().and_then(|v| v.get("models")).and_then(serde_json::Value::as_array).map(|list| list.iter().filter_map(|m| m.get("name").and_then(serde_json::Value::as_str).map(str::to_owned)).collect());
    st.notes.retain(|n| !n.en.contains("model "));
    for def in &st.defs {
        let Some(model) = &def.model else { continue };
        match &st.models {
            None => st.notes.push(Note::new(&def.id, format!("ใช้โมเดล {model} แต่ไม่ได้ลงปลั๊กอิน Models จึงเห็นเป็น mob ธรรมดา"), format!("it names the model {model}, but the Models plugin is not installed: it looks like the plain creature"))),
            Some(list) if !list.contains(model) => st.notes.push(Note::new(&def.id, format!("ปลั๊กอิน Models ไม่มีโมเดล {model}"), format!("the Models plugin has no model {model}"))),
            Some(_) => {}
        }
    }
}

// ---- making and removing mobs ---------------------------------------------------

fn goal(g: Goal) -> BuiltinAiGoal {
    match g {
        Goal::Swim => BuiltinAiGoal::Swim,
        Goal::Wander(speed) => BuiltinAiGoal::WanderAround(speed),
        Goal::Melee(speed) => BuiltinAiGoal::MeleeAttack(speed),
        Goal::LookAtPlayer(range) => BuiltinAiGoal::LookAtPlayer(range),
        Goal::LookAround => BuiltinAiGoal::LookAround,
        Goal::Escape(speed) => BuiltinAiGoal::EscapeDanger(speed),
    }
}

fn slot(name: &str) -> EquipmentSlot {
    match name {
        "offhand" => EquipmentSlot::OffHand,
        "head" => EquipmentSlot::Head,
        "chest" => EquipmentSlot::Chest,
        "legs" => EquipmentSlot::Legs,
        "feet" => EquipmentSlot::Feet,
        _ => EquipmentSlot::MainHand,
    }
}

fn bar_colour(name: &str) -> BossBarColor {
    match name {
        "pink" => BossBarColor::Pink,
        "blue" => BossBarColor::Blue,
        "green" => BossBarColor::Green,
        "yellow" => BossBarColor::Yellow,
        "purple" => BossBarColor::Purple,
        "white" => BossBarColor::White,
        _ => BossBarColor::Red,
    }
}

/// Put one mob into the world. `Err` says why not, for the one who asked.
fn spawn(st: &mut State, world: &World, def: Arc<MobDef>, at: (f64, f64, f64), spawner: Option<String>) -> Result<i32, &'static str> {
    if st.lives.len() >= st.cfg.max_mobs as usize {
        return Err("too many mobs");
    }
    let kind = c::tables::entity_type(&def.base).ok_or("unknown creature")?;
    let entity = world.spawn_entity(kind, at);
    let Some(living) = entity.as_living() else {
        entity.remove();
        return Err("not a living creature");
    };
    // A creature a plugin makes starts without air and would drown on dry land
    entity.set_remaining_air(entity.get_max_air());
    entity.set_custom_data(NS, MARK, &NbtTree { root: 0, tags: vec![NbtTag::StringTag(run_mark().to_owned())] });

    let set = |attribute: Attribute, value: Option<f32>| {
        if let Some(v) = value {
            living.set_attribute_base(attribute, f64::from(v));
        }
    };
    set(Attribute::MaxHealth, def.health);
    set(Attribute::AttackDamage, def.damage);
    set(Attribute::MovementSpeed, def.speed);
    set(Attribute::Armor, def.armor);
    set(Attribute::FollowRange, def.follow_range);
    set(Attribute::KnockbackResistance, def.knockback_resistance);
    set(Attribute::Scale, def.scale);
    if let Some(h) = def.health {
        living.set_max_health(h);
        living.set_health(h);
    }
    let max_health = def.health.unwrap_or_else(|| living.get_max_health());

    let mob = entity.as_mob();
    if let (Some(mob), Some(ai)) = (&mob, &def.ai) {
        mob.clear_ai_goals();
        for (priority, g) in ai.iter().enumerate() {
            mob.add_ai_goal(priority as u8, goal(*g));
        }
    }
    if def.glowing {
        entity.set_glowing(true);
    }
    if def.silent {
        entity.set_silent(true);
    }
    for (name, item) in &def.equipment {
        living.set_equipment(slot(name), Some(ItemStack::new(item, 1)));
    }
    let shown = defs::shown_name(&def, max_health, max_health);
    entity.set_custom_name(c::color(&shown));
    entity.set_custom_name_visible(true);

    let id = entity.get_id() as i32;
    let model = def.model.as_ref().filter(|m| st.models.as_ref().is_some_and(|list| list.contains(m))).and_then(|model| {
        let mut ask = serde_json::json!({ "kind": "attach", "model": model, "entity": id, "name": shown });
        if let Some(scale) = def.model_scale {
            ask["scale"] = scale.into();
        }
        c::ipc("models", &ask)?.get("id")?.as_u64()
    });
    let bar = def.bossbar.as_deref().map(|colour| BossBar::new(c::color(&def.name), bar_colour(colour), BossBarDivision::NoDivision));
    let tick = st.tick;
    let timer = def.skills.iter().map(|s| if let Trigger::Timer(n) = s.on { tick + u64::from(n) } else { 0 }).collect();
    st.lives.push(Live {
        fired: vec![false; def.skills.len()],
        ready: vec![0; def.skills.len()],
        timer,
        world: world.get_dimension(),
        lived: entity.get_ticks_lived(),
        def,
        entity,
        id,
        living,
        mob,
        model,
        spawner,
        born: tick,
        max_health,
        health: max_health,
        shown,
        bar,
        bar_players: Vec::new(),
        fighter: None,
        targeting: false,
        stale: 0,
    });
    st.stat.spawned += 1;
    Ok(id)
}

/// Forget a mob. `remove`: also take its creature out of the world.
fn drop_live(st: &mut State, at: usize, remove: bool) {
    let live = st.lives.swap_remove(at);
    if let Some(bar) = &live.bar {
        bar.remove_all();
    }
    if remove {
        if let Some(model) = live.model {
            let _ = c::ipc("models", &serde_json::json!({ "kind": "remove", "id": model }));
        }
        live.entity.remove();
    }
}

/// Remove every creature with our tag that is from an earlier run.
fn sweep(server: &Server) -> usize {
    let mut removed = 0;
    for world in server.get_all_worlds() {
        for entity in world.get_entities() {
            if leftover(&entity) {
                entity.remove();
                removed += 1;
            }
        }
    }
    removed
}

// ---- skills -----------------------------------------------------------------------

/// Something to do once the state is let go of.
enum After {
    Console(String),
}

fn run_after(server: &Server, jobs: Vec<After>) {
    for job in jobs {
        match job {
            After::Console(command) => server.execute_command(&command, ServerSender::Console),
        }
    }
}

fn near_players(server: &Server) -> Vec<Near> {
    server
        .get_all_players()
        .into_iter()
        .map(|player| Near { name: player.get_name(), world: player.get_world().get_dimension(), at: player.get_position(), fair: matches!(player.get_gamemode(), GameMode::Survival | GameMode::Adventure), player })
        .collect()
}

fn distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// The players of a world, closest first, with how far each is.
fn around<'a>(players: &'a [Near], world: &str, at: (f64, f64, f64)) -> Vec<(&'a Near, f64)> {
    let mut list: Vec<(&Near, f64)> = players.iter().filter(|p| p.world == world).map(|p| (p, distance(p.at, at))).collect();
    list.sort_by(|a, b| a.1.total_cmp(&b.1));
    list
}

/// Run the skills of one mob that `which` picks. `target` is who `%target%` and the aimed
/// actions mean when given; otherwise it is the closest player.
fn fire(st: &mut State, at: usize, which: &dyn Fn(usize, &Trigger) -> bool, players: &[Near], target: Option<&str>, after: &mut Vec<After>) {
    let State { lives, rng, queue, defs: all, tick, stat, .. } = st;
    let Some(live) = lives.get_mut(at) else { return };
    let def = live.def.clone();
    let here = live.entity.get_position();
    let close = around(players, &live.world, here);
    let aimed = target.and_then(|name| close.iter().find(|(p, _)| p.name == name)).or_else(|| close.iter().find(|(p, _)| p.fair)).or(close.first()).copied();
    let target_name = target.map(str::to_owned).or_else(|| aimed.map(|(p, _)| p.name.clone())).unwrap_or_default();
    for (i, skill) in def.skills.iter().enumerate() {
        if !which(i, &skill.on) || *tick < live.ready[i] {
            continue;
        }
        if let Some(range) = skill.range
            && !close.iter().any(|(p, d)| p.fair && *d <= f64::from(range))
        {
            continue;
        }
        if !rng.chance(skill.chance) {
            continue;
        }
        if skill.cooldown > 0 {
            live.ready[i] = *tick + u64::from(skill.cooldown);
        }
        stat.skills += 1;
        for action in &skill.actions {
            match action {
                Action::Animation(name) => {
                    if let Some(model) = live.model {
                        let _ = c::ipc("models", &serde_json::json!({ "kind": "play", "id": model, "animation": name, "mode": "once" }));
                    }
                }
                Action::Message(text) => {
                    let line = defs::fill(text, &def.name, &target_name, here, &live.world);
                    for (p, _) in close.iter().filter(|(_, d)| *d <= 32.0) {
                        c::tell(&p.player, &line);
                    }
                }
                Action::Sound { id, volume, pitch } => live.entity.get_world().play_custom_sound(id, SoundCategory::Hostile, here, *volume, *pitch),
                Action::Particle { name, count } => {
                    if let Some(particle) = c::particles::particle(name) {
                        live.entity.get_world().spawn_particle(particle, (here.0, here.1 + 1.0, here.2), (0.6, 0.6, 0.6), 0.05, *count as i32);
                    }
                }
                Action::Damage { amount, radius } => {
                    for (p, _) in close.iter().filter(|(p, d)| p.fair && *d <= f64::from(*radius)) {
                        p.player.damage(*amount, DamageType::MobAttack);
                    }
                }
                Action::Heal(amount) => {
                    live.health = (live.living.get_health() + amount).min(live.max_health);
                    live.living.set_health(live.health);
                }
                Action::Summon { mob, count } => {
                    if let Some(other) = all.iter().find(|d| d.id == *mob) {
                        for n in 0..*count {
                            let angle = rng.unit() * std::f32::consts::TAU + n as f32;
                            queue.push(Summon { def: other.clone(), world: live.entity.get_world(), at: (here.0 + f64::from(angle.cos()) * 1.5, here.1, here.2 + f64::from(angle.sin()) * 1.5), spawner: None });
                        }
                    }
                }
                Action::Command(text) => after.push(After::Console(defs::fill(text, &def.name, &target_name, here, &live.world))),
                Action::Effect { effect, seconds, level, radius } => {
                    if let Some(kind) = c::tables::status_effect(effect) {
                        for (p, _) in close.iter().filter(|(p, d)| p.fair && *d <= f64::from(*radius)) {
                            p.player.add_effect(StatusEffectInstance { effect_type: kind, duration: seconds * 20, amplifier: (*level - 1).min(254) as u8, ambient: false, show_particles: true, show_icon: true });
                        }
                    }
                }
                Action::Leap(strength) => {
                    if let Some((p, d)) = aimed
                        && d > 0.5
                    {
                        let push = 0.6 * f64::from(*strength);
                        live.entity.set_velocity(((p.at.0 - here.0) / d * push, 0.42, (p.at.2 - here.2) / d * push));
                    }
                }
                Action::Ignite { seconds, radius } => {
                    for (p, _) in close.iter().filter(|(p, d)| p.fair && *d <= f64::from(*radius)) {
                        p.player.as_entity().set_fire_ticks((*seconds * 20) as i32);
                    }
                }
                Action::Teleport => {
                    if let Some((p, _)) = aimed {
                        live.entity.teleport(p.at, live.entity.get_world());
                    }
                }
            }
        }
    }
}

/// Show a mob's health where it is shown: behind its name and on its bar.
fn show_health(live: &mut Live, health: f32) {
    live.health = health;
    if let Some(bar) = &live.bar {
        bar.set_health((health / live.max_health.max(0.01)).clamp(0.0, 1.0));
    }
    if !live.def.show_health {
        return;
    }
    let shown = defs::shown_name(&live.def, health, live.max_health);
    if shown != live.shown {
        live.entity.set_custom_name(c::color(&shown));
        if let Some(model) = live.model {
            let _ = c::ipc("models", &serde_json::json!({ "kind": "name", "id": model, "name": shown }));
        }
        live.shown = shown;
    }
}

/// Make the mobs that skills, spawners and other plugins asked for.
fn make_queued(st: &mut State, server: &Server, players: &[Near], after: &mut Vec<After>) {
    // A summon may summon: a few rounds, then the rest waits for the next tick
    for _ in 0..3 {
        let wanted = std::mem::take(&mut st.wanted);
        for (mob, world, at) in wanted {
            if let (Some(def), Some(world)) = (find_def(st, &mob), server.get_all_worlds().into_iter().find(|w| w.get_dimension() == world)) {
                st.queue.push(Summon { def, world, at, spawner: None });
            }
        }
        let queue = std::mem::take(&mut st.queue);
        if queue.is_empty() {
            return;
        }
        for s in queue {
            if spawn(st, &s.world, s.def, s.at, s.spawner).is_ok() {
                let at = st.lives.len() - 1;
                fire(st, at, &|_, on| matches!(on, Trigger::Spawn), players, None, after);
            }
        }
    }
}

// ---- the tick --------------------------------------------------------------------

/// Every half second: is each mob still there, who is near it, who is it after, what is due.
fn look(st: &mut State, server: &Server, after: &mut Vec<After>) {
    let players = near_players(server);
    let tick = st.tick;
    let (despawn, bar_range) = (f64::from(st.cfg.despawn_range), f64::from(st.cfg.bar_range));
    let mut gone = Vec::new();
    for at in 0..st.lives.len() {
        let live = &mut st.lives[at];
        // A creature that was taken out of the world no longer grows older
        let lived = live.entity.get_ticks_lived();
        live.stale = if lived == live.lived { live.stale + 1 } else { 0 };
        live.lived = lived;
        if live.stale >= 4 || live.living.is_dead() {
            gone.push((live.id, false));
            continue;
        }
        let here = live.entity.get_position();
        let close = around(&players, &live.world, here);
        let nearest = close.first().map_or(f64::INFINITY, |(_, d)| *d);
        if despawn > 0.0 && nearest > despawn && tick > live.born + 100 {
            gone.push((live.id, true));
            continue;
        }
        let health = live.living.get_health();
        if (health - live.health).abs() > 0.01 {
            show_health(live, health);
        }
        // Who it goes for: the closest player it could hurt, within its follow range
        if live.def.hostile == Some(true)
            && let Some(mob) = &live.mob
        {
            let reach = f64::from(live.def.follow_range.unwrap_or(16.0));
            match close.iter().find(|(p, d)| p.fair && *d <= reach) {
                Some((p, _)) => {
                    mob.set_target(Some(p.player.as_entity()));
                    live.targeting = true;
                }
                None if live.targeting => {
                    mob.set_target(None);
                    live.targeting = false;
                }
                None => {}
            }
        }
        if let Some(bar) = &live.bar {
            let now: Vec<String> = close.iter().filter(|(_, d)| *d <= bar_range).map(|(p, _)| p.name.clone()).collect();
            for name in now.iter().filter(|n| !live.bar_players.contains(n)) {
                if let Some(p) = server.get_player_by_name(name) {
                    bar.add_player(p);
                }
            }
            for name in live.bar_players.iter().filter(|n| !now.contains(n)) {
                if let Some(p) = server.get_player_by_name(name) {
                    bar.remove_player(p);
                }
            }
            live.bar_players = now;
        }
        // Timers that are due; a health skill that a hit without an event may have reached
        let share = health / live.max_health.max(0.01);
        let (timer, fired, skills) = (live.timer.clone(), live.fired.clone(), live.def.skills.clone());
        let mut due = vec![false; skills.len()];
        for (i, skill) in skills.iter().enumerate() {
            match skill.on {
                Trigger::Timer(n) if tick >= timer[i] => {
                    due[i] = true;
                    st.lives[at].timer[i] = tick + u64::from(n);
                }
                Trigger::Health(below) if !fired[i] && share < below => {
                    due[i] = true;
                    st.lives[at].fired[i] = true;
                }
                _ => {}
            }
        }
        if due.contains(&true) {
            fire(st, at, &|i, _| due[i], &players, None, after);
        }
    }
    for (id, remove) in gone {
        if let Some(at) = st.lives.iter().position(|l| l.id == id) {
            drop_live(st, at, remove);
        }
    }

    // Spawners: once a second
    if tick % 20 < LOOK_EVERY {
        let reach = f64::from(st.cfg.spawner_range);
        for i in 0..st.places.len() {
            let place = &st.places[i];
            let at = (place.def.x, place.def.y, place.def.z);
            if tick < place.next || !players.iter().any(|p| p.world == place.def.world && distance(p.at, at) <= reach) {
                continue;
            }
            let alive = st.lives.iter().filter(|l| l.spawner.as_deref() == Some(place.def.name.as_str())).count() as u32;
            let queued = st.queue.iter().filter(|s| s.spawner.as_deref() == Some(place.def.name.as_str())).count() as u32;
            if alive + queued >= place.def.max {
                continue;
            }
            let (Some(def), Some(world)) = (find_def(st, &place.def.mob), server.get_all_worlds().into_iter().find(|w| w.get_dimension() == place.def.world)) else { continue };
            let (angle, far) = (st.rng.unit() * std::f32::consts::TAU, st.rng.unit() * st.places[i].def.radius);
            let name = st.places[i].def.name.clone();
            st.queue.push(Summon { def, world, at: (at.0 + f64::from(angle.cos() * far), at.1, at.2 + f64::from(angle.sin() * far)), spawner: Some(name) });
            st.places[i].next = tick + u64::from(st.places[i].def.seconds) * 20;
        }
    }

    // What an earlier run left near a player: it would stand there as a plain creature
    if tick % 100 < LOOK_EVERY {
        let mut removed = 0;
        for p in &players {
            for entity in p.player.as_entity().get_nearby_entities(48.0, 48.0, 48.0) {
                if leftover(&entity) {
                    entity.remove();
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            c::log_info(&format!("removed {removed} mob(s) that an earlier run left near a player"));
        }
    }
    make_queued(st, server, &players, after);
}

fn on_tick(server: Server) {
    let mut after = Vec::new();
    {
        let Some(mut guard) = try_lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.tick += 1;
        let tick = st.tick;
        let started = Instant::now();
        if tick == 20 {
            ask_models(st);
        }
        if tick == 40 || tick >= st.swept + SWEEP_EVERY {
            st.swept = tick;
            let n = sweep(&server);
            if n > 0 {
                c::log_info(&format!("removed {n} mob(s) that an earlier run left in the world"));
            }
        }
        if tick % LOOK_EVERY == 0 {
            look(st, &server, &mut after);
        } else if !st.queue.is_empty() || !st.wanted.is_empty() {
            let players = near_players(&server);
            make_queued(st, &server, &players, &mut after);
        }
        st.stat.ticks += 1;
        st.stat.us += started.elapsed().as_micros() as u64;
    }
    run_after(&server, after);
}

// ---- events ----------------------------------------------------------------------

type Data<E> = <E as FromIntoEvent>::Data;

struct OnHurt;
impl EventHandler<EntityDamageEvent> for OnHurt {
    fn handle(&self, server: Server, e: Data<EntityDamageEvent>) -> Data<EntityDamageEvent> {
        let mut after = Vec::new();
        {
            let Some(mut guard) = try_lock() else { return e };
            let Some(st) = guard.as_mut() else { return e };
            let Some(at) = st.lives.iter().position(|l| l.id == e.entity_id) else { return e };
            if e.cancelled {
                return e;
            }
            let players = near_players(&server);
            let tick = st.tick;
            let live = &mut st.lives[at];
            let here = live.entity.get_position();
            // Pumpkin does not say who did it: the player closest to it is the one fighting it
            if let Some((p, _)) = around(&players, &live.world, here).into_iter().find(|(_, d)| *d <= 8.0) {
                live.fighter = Some((p.name.clone(), tick));
            }
            // The hit lands after this event: what is left is worked out here
            let left = (live.living.get_health() - e.damage).max(0.0);
            show_health(live, left);
            let share = left / live.max_health.max(0.01);
            let mut due = vec![false; live.def.skills.len()];
            for (i, skill) in live.def.skills.iter().enumerate() {
                match skill.on {
                    Trigger::Hurt if left > 0.0 => due[i] = true,
                    Trigger::Health(below) if !live.fired[i] && share < below && left > 0.0 => {
                        due[i] = true;
                        live.fired[i] = true;
                    }
                    _ => {}
                }
            }
            let fighter = live.fighter.as_ref().map(|(name, _)| name.clone());
            if due.contains(&true) {
                fire(st, at, &|i, _| due[i], &players, fighter.as_deref(), &mut after);
            }
            make_queued(st, &server, &players, &mut after);
        }
        run_after(&server, after);
        e
    }
}

struct OnDeath;
impl EventHandler<EntityDeathEvent> for OnDeath {
    fn handle(&self, server: Server, mut e: Data<EntityDeathEvent>) -> Data<EntityDeathEvent> {
        let mut after = Vec::new();
        {
            let Some(mut guard) = try_lock() else { return e };
            let Some(st) = guard.as_mut() else { return e };
            let Some(at) = st.lives.iter().position(|l| l.id == e.entity_id) else { return e };
            let players = near_players(&server);
            let tick = st.tick;
            let live = &st.lives[at];
            let def = live.def.clone();
            let here = live.entity.get_position();
            // Who gets what it leaves: whoever fought it last, or whoever stands closest
            let killer = live.fighter.as_ref().filter(|(_, when)| tick < when + FIGHT_TICKS).map(|(name, _)| name.clone()).or_else(|| around(&players, &live.world, here).into_iter().find(|(_, d)| *d <= 8.0).map(|(p, _)| p.name.clone()));
            if let Some(exp) = def.exp {
                e.dropped_exp = exp as i32;
            }
            fire(st, at, &|_, on| matches!(on, Trigger::Death), &players, killer.as_deref(), &mut after);
            if let Some(killer) = &killer {
                for (item, count) in defs::roll_drops(&def.drops, &mut st.rng) {
                    after.push(After::Console(match item.strip_prefix("customitems:") {
                        Some(id) => format!("customitems give {killer} {id} {count}"),
                        None => format!("give {killer} {item} {count}"),
                    }));
                }
            }
            st.stat.died += 1;
            // Its model plays its death by itself; the creature is the game's to remove
            drop_live(st, at, false);
            make_queued(st, &server, &players, &mut after);
        }
        run_after(&server, after);
        e
    }
}

// ---- commands --------------------------------------------------------------------

fn help(th: bool) -> Vec<String> {
    let lines: &[(&str, &str, &str)] = &[
        ("/mobs list", "mob ที่นิยามไว้", "the mobs that are defined"),
        ("/mobs info [mob]", "รายละเอียดของ mob หรือปัญหาในไฟล์", "what a mob is, or the problems in the files"),
        ("/mobs spawn <mob> [จำนวน]", "เสก mob ตรงที่ยืน", "make mobs where you stand"),
        ("/mobs near", "mob ที่อยู่ในโลกตอนนี้", "the mobs that are in the world now"),
        ("/mobs killall [mob]", "เอา mob ของปลั๊กอินนี้ออกทั้งหมด", "take every mob of this plugin away"),
        ("/mobs spawner add <ชื่อ> <mob> [สูงสุด] [วินาที] [รัศมี]", "จุดเกิดตรงที่ยืน", "a spawner where you stand"),
        ("/mobs spawner remove <ชื่อ> | list", "", ""),
        ("/mobs reload | stat | version", "", ""),
    ];
    let mut out = vec![format!("{P}&f{VERSION}")];
    out.extend(lines.iter().map(|(cmd, thai, english)| format!("&e{cmd} &7{}", if th { thai } else { english })));
    out
}

fn describe(def: &MobDef, th: bool) -> Vec<String> {
    let mut numbers = Vec::new();
    for (label, value) in [("health", def.health), ("damage", def.damage), ("speed", def.speed), ("armor", def.armor), ("follow-range", def.follow_range), ("scale", def.scale)] {
        if let Some(v) = value {
            numbers.push(format!("{label} {v}"));
        }
    }
    let mut out = vec![format!("{P}&f{} &7({}) {}", def.id, def.base, def.name)];
    if !numbers.is_empty() {
        out.push(format!("&7{}", numbers.join(", ")));
    }
    if let Some(model) = &def.model {
        out.push(format!("&7model {model}"));
    }
    if def.ai.is_some() || def.hostile.is_some() {
        out.push(format!("&7{}{}", if def.ai.is_some() { if th { "พฤติกรรมของตัวเอง " } else { "its own behaviour " } } else { "" }, match def.hostile {
            Some(true) => if th { "ไล่ผู้เล่น" } else { "goes for players" },
            _ => "",
        }));
    }
    if !def.drops.is_empty() {
        out.push(format!("&7drops: {}", def.drops.iter().map(|d| format!("{} {}-{} {:.0}%", d.item, d.min, d.max, d.chance * 100.0)).collect::<Vec<_>>().join(", ")));
    }
    out.push(format!("&7skills: {}", def.skills.len()));
    out
}

fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String], after: &mut Vec<After>) -> Vec<String> {
    let th = st.cfg.thai();
    let player = c::sender_player(sender);
    let no_mob = |name: &str| msg(th, &format!("&cไม่มี mob {name} &7(ดู /mobs list)"), &format!("&cThere is no mob {name} &7(see /mobs list)"));
    let players_only = || msg(th, "&cคำสั่งนี้ใช้ในเกมเท่านั้น", "&cThis command is for players in the game");
    match w.first().map(String::as_str) {
        Some("version") => vec![format!("Mobs {VERSION}")],
        Some("list") => {
            if st.defs.is_empty() {
                return vec![msg(th, "&7ยังไม่มี mob ดูตัวอย่างใน plugins/data/mobs/mobs/example.toml แล้วสั่ง /mobs reload", "&7No mobs yet. See the examples in plugins/data/mobs/mobs/example.toml, then run /mobs reload")];
            }
            let mut out = vec![msg(th, &format!("&fmob {} ตัว", st.defs.len()), &format!("&f{} mob(s)", st.defs.len()))];
            out.extend(st.defs.iter().map(|d| format!("&e{} &7{} {}", d.id, d.base, d.name)));
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /mobs info", st.notes.len()), &format!("&6{} thing(s) to look at: /mobs info", st.notes.len())));
            }
            out
        }
        Some("info") => match w.get(1) {
            Some(name) => find_def(st, name).map_or_else(|| vec![no_mob(name)], |d| describe(&d, th)),
            None => {
                let models = match &st.models {
                    Some(list) => format!("Models: {}", list.len()),
                    None => if th { "ไม่มีปลั๊กอิน Models".to_owned() } else { "no Models plugin".to_owned() },
                };
                let mut out = vec![msg(th, &format!("&fรุ่น {VERSION} &7mob {} ชนิด ในโลก {} ตัว จุดเกิด {} จุด, {models}", st.defs.len(), st.lives.len(), st.places.len()), &format!("&fversion {VERSION} &7{} mob(s) defined, {} in the world, {} spawner(s), {models}", st.defs.len(), st.lives.len(), st.places.len()))];
                out.extend(st.notes.iter().take(30).map(|n| format!("&6! &7{}", n.text(th))));
                out
            }
        },
        Some("reload") => {
            let (cfg, defs, notes) = load();
            (st.cfg, st.defs, st.notes) = (cfg, defs, notes);
            ask_models(st);
            // Mobs in the world follow their new description; ones whose description is gone leave
            let mut gone = Vec::new();
            for live in &mut st.lives {
                match st.defs.iter().find(|d| d.id == live.def.id) {
                    Some(def) => {
                        live.timer = def.skills.iter().map(|s| if let Trigger::Timer(n) = s.on { st.tick + u64::from(n) } else { 0 }).collect();
                        live.ready = vec![0; def.skills.len()];
                        live.fired = vec![false; def.skills.len()];
                        live.def = def.clone();
                    }
                    None => gone.push(live.id),
                }
            }
            for id in gone {
                if let Some(at) = st.lives.iter().position(|l| l.id == id) {
                    drop_live(st, at, true);
                }
            }
            let mut out = vec![msg(th, &format!("&aโหลดใหม่แล้ว &7mob {} ชนิด", st.defs.len()), &format!("&aReloaded &7{} mob(s)", st.defs.len()))];
            if !st.notes.is_empty() {
                out.push(msg(th, &format!("&6มี {} เรื่องที่ควรดู: /mobs info", st.notes.len()), &format!("&6{} thing(s) to look at: /mobs info", st.notes.len())));
            }
            out
        }
        Some("spawn") => {
            let Some(p) = player else { return vec![players_only()] };
            let Some(name) = w.get(1) else { return help(th) };
            let Some(def) = find_def(st, name) else { return vec![no_mob(name)] };
            let count = w.get(2).and_then(|n| n.parse::<u32>().ok()).unwrap_or(1).clamp(1, 50);
            let (world, at) = (p.get_world(), p.get_position());
            let players = near_players(server);
            let mut made = 0;
            for n in 0..count {
                let angle = n as f32 * 2.4;
                let spread = if count == 1 { 0.0 } else { 1.0 + n as f64 * 0.15 };
                if spawn(st, &world, def.clone(), (at.0 + f64::from(angle.cos()) * spread, at.1, at.2 + f64::from(angle.sin()) * spread), None).is_ok() {
                    made += 1;
                    let at = st.lives.len() - 1;
                    fire(st, at, &|_, on| matches!(on, Trigger::Spawn), &players, None, after);
                }
            }
            make_queued(st, server, &players, after);
            if made == 0 {
                return vec![msg(th, &format!("&cเสกไม่ได้: มี mob ครบ {} ตัวแล้ว", st.cfg.max_mobs), &format!("&cNone made: there are {} mobs already", st.cfg.max_mobs))];
            }
            vec![msg(th, &format!("&aเสก {name} {made} ตัว"), &format!("&aMade {made} {name}"))]
        }
        Some("near") => {
            if st.lives.is_empty() {
                return vec![msg(th, "&7ยังไม่มี mob ของปลั๊กอินนี้ในโลก", "&7No mob of this plugin is in the world")];
            }
            let mut out = vec![msg(th, &format!("&fmob ในโลก {} ตัว", st.lives.len()), &format!("&f{} mob(s) in the world", st.lives.len()))];
            for live in st.lives.iter().take(40) {
                let at = live.entity.get_position();
                out.push(format!("&e{} &7{:.0} {:.0} {:.0} {} &c{:.0}&7/&c{:.0}{}{}", live.def.id, at.0, at.1, at.2, live.world.trim_start_matches("minecraft:"), live.health, live.max_health, live.spawner.as_ref().map_or(String::new(), |s| format!(" &8[{s}]")), live.model.map_or(String::new(), |m| format!(" &8model #{m}"))));
            }
            out
        }
        Some("killall") => {
            let only = w.get(1);
            let ids: Vec<i32> = st.lives.iter().filter(|l| only.is_none_or(|name| l.def.id == *name)).map(|l| l.id).collect();
            for id in &ids {
                if let Some(at) = st.lives.iter().position(|l| l.id == *id) {
                    drop_live(st, at, true);
                }
            }
            // A spawner would fill up again at once: it waits its usual time
            let tick = st.tick;
            for place in &mut st.places {
                place.next = tick + u64::from(place.def.seconds) * 20;
            }
            vec![msg(th, &format!("&aเอา mob ออก {} ตัว", ids.len()), &format!("&aRemoved {} mob(s)", ids.len()))]
        }
        Some("spawner") => match w.get(1).map(String::as_str) {
            Some("add") => {
                let Some(p) = player else { return vec![players_only()] };
                let (Some(name), Some(mob)) = (w.get(2), w.get(3)) else { return help(th) };
                if !defs::valid_id(name) {
                    return vec![msg(th, "&cชื่อจุดเกิดใช้ได้แค่ a-z, 0-9 และ _", "&cA spawner's name may only have a-z, 0-9 and _")];
                }
                if find_def(st, mob).is_none() {
                    return vec![no_mob(mob)];
                }
                if st.places.iter().any(|p| p.def.name == *name) {
                    return vec![msg(th, &format!("&cมีจุดเกิดชื่อ {name} อยู่แล้ว"), &format!("&cThere already is a spawner named {name}"))];
                }
                if st.places.len() >= 500 {
                    return vec![msg(th, "&cมีจุดเกิดครบ 500 จุดแล้ว", "&cThere are 500 spawners already")];
                }
                let n = |i: usize, default: f32, low: f32, high: f32| w.get(i).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(default).clamp(low, high);
                let at = p.get_position();
                let def = Spawner { name: name.clone(), mob: mob.clone(), world: p.get_world().get_dimension(), x: at.0, y: at.1, z: at.2, max: n(4, 3.0, 1.0, 50.0) as u32, seconds: n(5, 30.0, 1.0, 86_400.0) as u32, radius: n(6, 4.0, 0.0, 32.0) };
                let line = msg(th, &format!("&aตั้งจุดเกิด {name}: {mob} สูงสุด {} ตัว ทุก {} วินาที รัศมี {}", def.max, def.seconds, def.radius), &format!("&aSpawner {name}: up to {} {mob}, one every {} second(s), within {} block(s)", def.max, def.seconds, def.radius));
                st.places.push(Place { def, next: 0 });
                save_places(st);
                vec![line]
            }
            Some("remove") => {
                let Some(name) = w.get(2) else { return help(th) };
                let Some(at) = st.places.iter().position(|p| p.def.name == *name) else { return vec![msg(th, &format!("&cไม่มีจุดเกิดชื่อ {name}"), &format!("&cThere is no spawner named {name}"))] };
                st.places.remove(at);
                save_places(st);
                // Its mobs go with it
                let ids: Vec<i32> = st.lives.iter().filter(|l| l.spawner.as_deref() == Some(name.as_str())).map(|l| l.id).collect();
                for id in ids {
                    if let Some(at) = st.lives.iter().position(|l| l.id == id) {
                        drop_live(st, at, true);
                    }
                }
                vec![msg(th, &format!("&aลบจุดเกิด {name} แล้ว"), &format!("&aRemoved the spawner {name}"))]
            }
            _ => {
                if st.places.is_empty() {
                    return vec![msg(th, "&7ยังไม่มีจุดเกิด", "&7No spawners yet")];
                }
                let mut out = vec![msg(th, &format!("&fจุดเกิด {} จุด", st.places.len()), &format!("&f{} spawner(s)", st.places.len()))];
                for place in &st.places {
                    let d = &place.def;
                    let alive = st.lives.iter().filter(|l| l.spawner.as_deref() == Some(d.name.as_str())).count();
                    out.push(format!("&e{} &7{} {alive}/{} {}s r{} &8{:.0} {:.0} {:.0} {}", d.name, d.mob, d.max, d.seconds, d.radius, d.x, d.y, d.z, d.world.trim_start_matches("minecraft:")));
                }
                out
            }
        },
        Some("stat") => {
            let s = &st.stat;
            let t = s.ticks.max(1);
            let line = format!("STAT mobs={} spawners={} us_per_tick={} spawned={} died={} skills={} ticks={}", st.lives.len(), st.places.len(), s.us / t, s.spawned, s.died, s.skills, s.ticks);
            st.stat = Stat::default();
            vec![line]
        }
        _ => help(th),
    }
}

struct Cmd;
impl CommandHandler for Cmd {
    fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
        let w = c::words(&args, "args");
        let mut after = Vec::new();
        let lines = {
            let Some(mut guard) = try_lock() else { return Ok(0) };
            let Some(st) = guard.as_mut() else { return Ok(0) };
            run(st, &sender, &server, &w, &mut after)
        };
        for line in lines {
            c::reply(&sender, &line);
        }
        run_after(&server, after);
        Ok(1)
    }
}

// ---- the plugin ------------------------------------------------------------------

struct Mobs;

impl Plugin for Mobs {
    fn new() -> Self {
        Mobs
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NS.into(),
            version: VERSION.into(),
            authors: vec!["MCSV".into()],
            description: "Custom mobs with their own stats, skills, drops and spawners".into(),
            dependencies: vec![],
            permissions: vec![FS_READ_DATA.into(), FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        c::init_data_dir(context.get_data_folder());
        let _ = context.register_permission(&Permission { node: format!("{NS}:command"), description: "Manage mobs".into(), default: PermissionDefault::Op(PermissionLevel::Two), children: vec![] });
        let (cfg, defs, mut notes) = load();
        let places: Vec<Place> = c::load_json::<Vec<Spawner>>(SPAWNERS_FILE).unwrap_or_default().into_iter().map(|def| Place { def, next: 0 }).collect();
        for place in &places {
            if !defs.iter().any(|d| d.id == place.def.mob) {
                notes.push(Note::new(SPAWNERS_FILE, format!("จุดเกิด {} ใช้ mob {} ซึ่งไม่มีแล้ว", place.def.name, place.def.mob), format!("the spawner {} is for the mob {}, which is not there any more", place.def.name, place.def.mob)));
            }
        }
        c::log_info(&format!("{} mob(s), {} spawner(s){}", defs.len(), places.len(), if notes.is_empty() { String::new() } else { format!(", {} thing(s) to look at (/mobs info)", notes.len()) }));
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(State {
            cfg,
            defs,
            notes,
            lives: Vec::new(),
            places,
            queue: Vec::new(),
            wanted: Vec::new(),
            models: None,
            tick: 0,
            rng: Rng(c::now_secs() ^ 0x9E37_79B9_7F4A_7C15),
            stat: Stat::default(),
            swept: 0,
        });
        let cmd = Command::new(&["mobs".to_owned(), "mob".to_owned()], "Custom mobs").execute(Cmd).then(CommandNode::argument("args", &ArgumentType::String(StringType::Greedy)).execute(Cmd));
        context.register_command(cmd, &format!("{NS}:command"));
        context.register_event_handler(OnHurt, EventPriority::Lowest, false)?;
        context.register_event_handler(OnDeath, EventPriority::Normal, true)?;
        context.schedule_repeating_task(1, 1, on_tick);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        if let Ok(mut guard) = STATE.lock()
            && let Some(st) = guard.as_mut()
        {
            while !st.lives.is_empty() {
                drop_live(st, 0, true);
            }
        }
        Ok(())
    }

    /// Other plugins: `{"kind":"mobs"}` lists the mobs, `{"kind":"spawn","mob","world"?,"x",
    /// "y","z"}` makes one at the next tick. Messages from Models about its models are heard
    /// and need no answer.
    fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
        let v: serde_json::Value = serde_json::from_slice(&message).map_err(|e| e.to_string())?;
        let answer = match v.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "model_gone" | "model_click" => serde_json::json!({ "ok": true }),
            "mobs" => {
                let guard = try_lock().ok_or("busy")?;
                let st = guard.as_ref().ok_or("not loaded")?;
                serde_json::json!({ "ok": true, "mobs": st.defs.iter().map(|d| serde_json::json!({ "id": d.id, "type": d.base, "name": d.name })).collect::<Vec<_>>() })
            }
            "spawn" => {
                let mut guard = try_lock().ok_or("busy")?;
                let st = guard.as_mut().ok_or("not loaded")?;
                let mob = v.get("mob").and_then(serde_json::Value::as_str).ok_or("mob is missing")?;
                find_def(st, mob).ok_or("no such mob")?;
                let n = |key: &str| v.get(key).and_then(serde_json::Value::as_f64);
                let (Some(x), Some(y), Some(z)) = (n("x"), n("y"), n("z")) else { return Err("x, y and z are missing".into()) };
                if st.wanted.len() >= 100 {
                    return Err("too many at once".into());
                }
                st.wanted.push((mob.to_owned(), v.get("world").and_then(serde_json::Value::as_str).unwrap_or("minecraft:overworld").to_owned(), (x, y, z)));
                serde_json::json!({ "ok": true })
            }
            other => return Err(format!("unknown kind {other}")),
        };
        serde_json::to_vec(&answer).map_err(|e| e.to_string())
    }
}

register_plugin!(Mobs);
