//! The interpreter: loaded scripts, variables, resumable triggers, commands and events.
//! Everything the server does goes through [`Host`], so the engine runs in unit tests too.
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::ast::*;
use crate::parse::{self, Names, ScriptError};
use crate::value::{self, Item, Loc, PlayerRef, Value, loose_cmp, loose_eq, to_text};

pub trait Host {
    /// Unix time in seconds.
    fn now(&mut self) -> f64;
    /// Uniform in `[0, 1)`.
    fn random(&mut self) -> f64;
    fn players(&mut self) -> Vec<PlayerRef>;
    fn find_player(&mut self, name: &str) -> Option<PlayerRef>;
    /// A player who may be offline (the uuid is the one the server would give that name).
    fn offline_player(&mut self, name: &str) -> PlayerRef;
    fn item(&mut self, text: &str) -> Option<Item>;
    fn get(&mut self, target: &Value, prop: Prop) -> Value;
    fn set(&mut self, target: &Value, prop: Prop, value: &Value) -> Result<(), String>;
    fn check(&mut self, check: Check, args: &[Value]) -> bool;
    fn fx(&mut self, fx: Fx, args: &[Value], flags: u32) -> Result<(), String>;
    fn send(&mut self, to: &Value, text: &str, kind: SendKind);
    fn broadcast(&mut self, text: &str);
    fn log(&mut self, line: &str);
    fn tps(&mut self) -> f64 {
        20.0
    }
    fn target_block(&mut self, _player: &Value) -> Value {
        Value::None
    }
    /// Entities (players as players) within `radius` of a place, or all of them. Bounded by the host.
    fn entities(&mut self, _around: Option<(&Loc, f64)>) -> Vec<Value> {
        Vec::new()
    }
    /// Seconds added to a date before it is printed (the server's time zone).
    fn date_offset(&self) -> f64 {
        0.0
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    /// Instructions one trigger may run without a `wait`.
    pub run_ops: u64,
    /// Instructions all scripts together may run in one server tick.
    pub tick_ops: u64,
    pub max_variables: usize,
    pub max_list: usize,
    pub max_waits: usize,
    pub max_text: usize,
    pub call_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { run_ops: 50_000, tick_ops: 200_000, max_variables: 100_000, max_list: 10_000, max_waits: 2000, max_text: 4096, call_depth: 16 }
    }
}

/// Sentences the engine itself says, already in the server's language.
#[derive(Debug, Clone)]
pub struct Messages {
    pub no_permission: String,
    pub cooldown: String,
    pub players_only: String,
    pub console_only: String,
    pub usage: String,
    pub no_player: String,
    pub limit: String,
    pub tick_limit: String,
    pub wait_in_value: String,
    pub too_many_waits: String,
    pub too_many_variables: String,
    pub recursion: String,
}

impl Default for Messages {
    fn default() -> Self {
        Messages {
            no_permission: "&cYou don't have permission to use this command.".into(),
            cooldown: "&cYou have to wait {time} before using this command again.".into(),
            players_only: "&cOnly players can use this command.".into(),
            console_only: "&cOnly the console can use this command.".into(),
            usage: "&cUsage: {usage}".into(),
            no_player: "&cPlayer not found: {name}".into(),
            limit: "stopped after {n} instructions without a wait (endless loop?)".into(),
            tick_limit: "skipped: the scripts used up this tick's budget".into(),
            wait_in_value: "`wait` inside a function that is used as a value is ignored".into(),
            too_many_waits: "too many waiting triggers; this one was dropped".into(),
            too_many_variables: "variable limit reached; the value was not stored".into(),
            recursion: "functions call each other too deeply".into(),
        }
    }
}

// ---- variables -------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct VarStore {
    map: BTreeMap<String, Value>,
    pub dirty: bool,
}

impl VarStore {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, key: &str) -> Value {
        self.map.get(key).cloned().unwrap_or(Value::None)
    }

    /// False when the store is full and the key is new.
    pub fn set(&mut self, key: &str, v: Value, max: usize) -> bool {
        let v = v.first();
        if v.is_none() {
            self.delete(key);
            return true;
        }
        if !self.map.contains_key(key) && self.map.len() >= max {
            return false;
        }
        self.map.insert(key.to_owned(), v);
        self.dirty = true;
        true
    }

    /// Removes the variable and everything under it (`name::…`).
    pub fn delete(&mut self, key: &str) {
        self.dirty |= self.map.remove(key).is_some();
        self.clear_list(key);
    }

    fn child_keys(&self, name: &str) -> Vec<String> {
        let prefix = format!("{name}::");
        self.map.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)).map(|(k, _)| k.clone()).collect()
    }

    pub fn clear_list(&mut self, name: &str) {
        for k in self.child_keys(name) {
            self.map.remove(&k);
            self.dirty = true;
        }
    }

    /// Direct children of `name::`, numbers first in numeric order, then the rest by name.
    pub fn list(&self, name: &str) -> Vec<(String, Value)> {
        let prefix = format!("{name}::");
        let mut out: Vec<(String, Value)> = self
            .map
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| !k[prefix.len()..].contains("::"))
            .map(|(k, v)| (k[prefix.len()..].to_owned(), v.clone()))
            .collect();
        out.sort_by(|(a, _), (b, _)| match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        });
        out
    }

    /// The next free numeric index of a list.
    fn next_index(&self, name: &str) -> usize {
        let mut i = self.list(name).len() + 1;
        while self.map.contains_key(&format!("{name}::{i}")) {
            i += 1;
        }
        i
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.map).unwrap_or(serde_json::Value::Null)
    }

    pub fn from_json(v: serde_json::Value) -> Option<VarStore> {
        let map: BTreeMap<String, Value> = serde_json::from_value(v).ok()?;
        Some(VarStore { map, dirty: false })
    }
}

// ---- running state ---------------------------------------------------------------------------

/// What a trigger knows about the event or command that started it.
#[derive(Debug, Default, Clone)]
pub struct Ctx {
    pub vals: HashMap<String, Value>,
    pub cancelled: bool,
    pub args: Vec<(ArgType, Value)>,
    pub cooldown_key: Option<(String, String)>,
}

impl Ctx {
    pub fn with(mut self, key: &str, v: Value) -> Self {
        self.vals.insert(key.to_owned(), v);
        self
    }

    pub fn get(&self, key: &str) -> Value {
        self.vals.get(key).cloned().unwrap_or(Value::None)
    }
}

#[derive(Debug)]
struct LoopFrame {
    items: Vec<(Value, Value)>,
    pos: usize,
    /// `loop n times` / `loop integers`: current, last
    counter: Option<(f64, f64)>,
    iteration: u64,
}

#[derive(Debug)]
struct Frame {
    code: Arc<Code>,
    ip: usize,
    locals: VarStore,
    loops: Vec<LoopFrame>,
    function: bool,
}

impl Frame {
    fn new(code: Arc<Code>, function: bool) -> Frame {
        Frame { code, ip: 0, locals: VarStore::default(), loops: Vec::new(), function }
    }
}

#[derive(Debug)]
struct Task {
    frames: Vec<Frame>,
    ctx: Ctx,
    wake: u64,
    script: String,
}

enum End {
    Done(Value),
    Wait(u64),
}

struct Rt<'a> {
    host: &'a mut dyn Host,
    budget: u64,
    depth: usize,
    /// The budget is what was left of the tick, not the per-trigger limit.
    capped: bool,
}

#[derive(Debug, Default)]
pub struct Loaded {
    pub commands: Vec<Arc<CommandDef>>,
    pub triggers: Vec<Arc<Trigger>>,
    pub functions: Vec<Arc<FunctionDef>>,
    pub errors: Vec<ScriptError>,
    pub warnings: Vec<ScriptError>,
}

struct Timer {
    trigger: Arc<Trigger>,
    /// `every`: next tick to run; `at`: the world time seen last.
    state: u64,
}

pub struct Engine {
    pub limits: Limits,
    pub msgs: Messages,
    pub vars: VarStore,
    pub scripts: BTreeMap<String, Loaded>,
    /// Function names of files that are about to be loaded (so load order does not matter).
    pub extra_functions: HashSet<String>,
    commands: HashMap<String, Arc<CommandDef>>,
    functions: HashMap<String, Arc<FunctionDef>>,
    triggers: HashMap<EventKind, Vec<Arc<Trigger>>>,
    timers: Vec<Timer>,
    tasks: Vec<Task>,
    cooldowns: HashMap<(String, String), f64>,
    pub tick: u64,
    tick_ops: u64,
    pub runtime_errors: VecDeque<String>,
    error_window: (u64, u32),
    /// Problems that are reported once (a missing economy would fill the log otherwise).
    once: HashSet<String>,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new(Limits::default(), Messages::default())
    }
}

fn clamp_text(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

impl Engine {
    pub fn new(limits: Limits, msgs: Messages) -> Engine {
        Engine {
            limits,
            msgs,
            vars: VarStore::default(),
            scripts: BTreeMap::new(),
            extra_functions: HashSet::new(),
            commands: HashMap::new(),
            functions: HashMap::new(),
            triggers: HashMap::new(),
            timers: Vec::new(),
            tasks: Vec::new(),
            cooldowns: HashMap::new(),
            tick: 0,
            tick_ops: 0,
            runtime_errors: VecDeque::new(),
            error_window: (0, 0),
            once: HashSet::new(),
        }
    }

    // ---- scripts -----------------------------------------------------------------------------

    fn reindex(&mut self) {
        self.commands.clear();
        self.functions.clear();
        self.triggers.clear();
        let old: HashMap<usize, u64> = self.timers.iter().map(|t| (Arc::as_ptr(&t.trigger) as usize, t.state)).collect();
        self.timers.clear();
        for loaded in self.scripts.values() {
            for c in &loaded.commands {
                self.commands.insert(c.name.clone(), Arc::clone(c));
                for a in &c.aliases {
                    self.commands.entry(a.clone()).or_insert_with(|| Arc::clone(c));
                }
            }
            for f in &loaded.functions {
                self.functions.insert(f.name.clone(), Arc::clone(f));
            }
            for t in &loaded.triggers {
                self.triggers.entry(t.kind).or_default().push(Arc::clone(t));
                if matches!(t.kind, EventKind::Every | EventKind::At) {
                    let fresh = if t.kind == EventKind::Every { self.tick + period(t) } else { u64::MAX };
                    let state = old.get(&(Arc::as_ptr(t) as usize)).copied().unwrap_or(fresh);
                    self.timers.push(Timer { trigger: Arc::clone(t), state });
                }
            }
        }
    }

    /// Parses and installs a script, replacing an older one of the same name.
    pub fn load(&mut self, name: &str, src: &str, names: &dyn Names) -> &Loaded {
        self.drop_script(name);
        self.once.clear();
        let mut known: HashSet<String> = self.functions.keys().cloned().collect();
        known.extend(self.extra_functions.iter().cloned());
        let unit = parse::parse_script(name, src, names, &known);
        let mut loaded = Loaded {
            commands: unit.commands.into_iter().map(Arc::new).collect(),
            triggers: unit.triggers.into_iter().map(Arc::new).collect(),
            functions: unit.functions.into_iter().map(Arc::new).collect(),
            errors: unit.errors,
            warnings: unit.warnings,
        };
        for c in &loaded.commands {
            if let Some(other) = self.commands.get(&c.name).filter(|o| o.script != name) {
                loaded.warnings.push(ScriptError { line: c.line, code: "duplicate", reason: None, detail: format!("/{} ({})", c.name, other.script) });
            }
        }
        // defaults of the `variables:` section are literals, no event is needed to read them
        for (var, value) in &unit.defaults {
            let key: String = var.parts.iter().map(|p| if let TextPart::Lit(s) = p { s.to_lowercase() } else { String::new() }).collect();
            if let (Expr::Lit(v), true) = (value, self.vars.get(&key).is_none()) {
                self.vars.set(&key, v.clone(), self.limits.max_variables);
            }
        }
        self.scripts.insert(name.to_owned(), loaded);
        self.reindex();
        &self.scripts[name]
    }

    fn drop_script(&mut self, name: &str) {
        if self.scripts.remove(name).is_some() {
            self.tasks.retain(|t| t.script != name);
            self.reindex();
        }
    }

    /// Runs the script's `on unload` triggers, then removes it with its waiting triggers.
    pub fn unload(&mut self, host: &mut dyn Host, name: &str) -> bool {
        if !self.scripts.contains_key(name) {
            return false;
        }
        self.fire_script(host, name, EventKind::Unload);
        self.drop_script(name);
        true
    }

    /// `on load` / `on unload` of one script.
    pub fn fire_script(&mut self, host: &mut dyn Host, name: &str, kind: EventKind) {
        let list: Vec<Arc<Trigger>> = self.scripts.get(name).map(|l| l.triggers.iter().filter(|t| t.kind == kind).cloned().collect()).unwrap_or_default();
        for t in list {
            self.start(host, &t.code, Ctx::default(), Vec::new());
        }
    }

    pub fn command(&self, label: &str) -> Option<&Arc<CommandDef>> {
        self.commands.get(&label.to_lowercase())
    }

    pub fn command_labels(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }

    pub fn has(&self, kind: EventKind) -> bool {
        self.triggers.get(&kind).is_some_and(|l| !l.is_empty())
    }

    /// True when a trigger of this kind looks at the block or item of the event.
    pub fn has_filtered(&self, kind: EventKind) -> bool {
        self.triggers.get(&kind).is_some_and(|l| l.iter().any(|t| !t.items.is_empty()))
    }

    /// True when a damage or death trigger is about something other than players.
    pub fn wants_entities(&self, kind: EventKind) -> bool {
        self.triggers.get(&kind).is_some_and(|l| l.iter().any(|t| t.filter.as_deref() != Some("player")))
    }

    pub fn waiting(&self) -> usize {
        self.tasks.len()
    }

    // ---- errors ------------------------------------------------------------------------------

    fn report(&mut self, host: &mut dyn Host, code: &Code, ip: usize, msg: &str) {
        let line = code.lines.get(ip.min(code.lines.len().saturating_sub(1))).copied().unwrap_or(0);
        let text = format!("{} line {line}: {msg}", code.script);
        if self.runtime_errors.len() >= 30 {
            self.runtime_errors.pop_front();
        }
        self.runtime_errors.push_back(text.clone());
        // at most ten console lines every ten seconds
        if self.tick.saturating_sub(self.error_window.0) >= 200 {
            self.error_window = (self.tick, 0);
        }
        self.error_window.1 += 1;
        if self.error_window.1 <= 10 {
            host.log(&text);
        }
    }

    /// Like `report`, but the same problem at the same line is said only once until the next reload.
    fn report_once(&mut self, host: &mut dyn Host, code: &Code, ip: usize, msg: &str) {
        let line = code.lines.get(ip.min(code.lines.len().saturating_sub(1))).copied().unwrap_or(0);
        if self.once.len() < 500 && self.once.insert(format!("{}:{line}:{msg}", code.script)) {
            self.report(host, code, ip, msg);
        }
    }

    // ---- events ------------------------------------------------------------------------------

    fn matches(t: &Trigger, ctx: &Ctx) -> bool {
        let id_of = |key: &str| match ctx.vals.get(key) {
            Some(Value::Item(i)) => i.id.clone(),
            Some(Value::Text(s)) => value::norm_id(s),
            _ => String::new(),
        };
        if !t.items.is_empty() {
            let key = match t.kind {
                EventKind::Drop | EventKind::Pickup | EventKind::Consume => "item",
                EventKind::Damage | EventKind::Death => "victim type",
                _ => "block type",
            };
            if !t.items.contains(&id_of(key)) {
                return false;
            }
        }
        if !t.holding.is_empty() && !t.holding.contains(&id_of("tool")) {
            return false;
        }
        if let Some(f) = &t.filter {
            if !matches!(ctx.vals.get("filter"), Some(Value::Text(v)) if v == f) {
                return false;
            }
        }
        if let Some(w) = &t.world {
            if !matches!(ctx.vals.get("world"), Some(Value::World(v)) if value::world_name(v) == *w) {
                return false;
            }
        }
        true
    }

    /// Runs every trigger of this kind; returns the context with `cancelled` and changed values.
    pub fn fire(&mut self, host: &mut dyn Host, kind: EventKind, mut ctx: Ctx) -> Ctx {
        let list: Vec<Arc<Trigger>> = match self.triggers.get(&kind) {
            Some(l) => l.iter().filter(|t| Self::matches(t, &ctx)).cloned().collect(),
            None => return ctx,
        };
        for t in list {
            ctx = self.start(host, &t.code, ctx, Vec::new());
        }
        ctx
    }

    /// Starts a trigger; a trigger that waits keeps a copy of the context.
    fn start(&mut self, host: &mut dyn Host, code: &Arc<Code>, ctx: Ctx, locals: Vec<(String, Value)>) -> Ctx {
        let mut frame = Frame::new(Arc::clone(code), false);
        for (k, v) in locals {
            frame.locals.set(&k, v, usize::MAX);
        }
        let task = Task { frames: vec![frame], ctx, wake: 0, script: code.script.clone() };
        if self.tick_ops >= self.limits.tick_ops {
            let msg = self.msgs.tick_limit.clone();
            self.report(host, code, 0, &msg);
            return task.ctx;
        }
        self.resume(host, task)
    }

    /// Runs a task until it ends or waits, and gives its context back.
    fn resume(&mut self, host: &mut dyn Host, mut task: Task) -> Ctx {
        let tick_left = self.limits.tick_ops.saturating_sub(self.tick_ops);
        let budget = self.limits.run_ops.min(tick_left).max(1);
        let mut rt = Rt { host, budget, depth: 0, capped: tick_left < self.limits.run_ops };
        let end = self.run(&mut rt, &mut task, false);
        self.tick_ops += budget - rt.budget;
        match end {
            Some(End::Wait(ticks)) => {
                let ctx = task.ctx.clone();
                if self.tasks.len() >= self.limits.max_waits {
                    let msg = self.msgs.too_many_waits.clone();
                    if let Some(f) = task.frames.last() {
                        let (code, ip) = (Arc::clone(&f.code), f.ip);
                        self.report(rt.host, &code, ip.saturating_sub(1), &msg);
                    }
                } else {
                    task.wake = self.tick + ticks.max(1);
                    // a waiting trigger can no longer cancel its event
                    self.tasks.push(task);
                }
                ctx
            }
            _ => task.ctx,
        }
    }

    /// Once per server tick: wake waiting triggers and run the timers.
    pub fn tick(&mut self, host: &mut dyn Host) {
        self.tick += 1;
        self.tick_ops = 0;
        if self.tasks.iter().any(|t| t.wake <= self.tick) {
            let (due, rest): (Vec<Task>, Vec<Task>) = std::mem::take(&mut self.tasks).into_iter().partition(|t| t.wake <= self.tick);
            self.tasks = rest;
            for mut task in due {
                if self.tick_ops >= self.limits.tick_ops {
                    task.wake = self.tick + 1;
                    self.tasks.push(task);
                } else {
                    self.resume(host, task);
                }
            }
        }
        let mut due = Vec::new();
        for timer in &mut self.timers {
            let t = &timer.trigger;
            if t.kind == EventKind::Every {
                if self.tick >= timer.state {
                    timer.state = self.tick + period(t);
                    due.push(Arc::clone(t));
                }
            } else if self.tick % 20 == 0 {
                let world = Value::World(t.world.clone().unwrap_or_else(|| "world".into()));
                let Some(now) = host.get(&world, Prop::Time).num().map(|n| n as u64 % 24_000) else { continue };
                let (last, target) = (timer.state, t.number as u64);
                timer.state = now;
                let crossed = if last == u64::MAX {
                    false
                } else if last <= now {
                    last < target && target <= now
                } else {
                    target > last || target <= now
                };
                if crossed {
                    due.push(Arc::clone(t));
                }
            }
        }
        for t in due {
            let world = t.world.clone().unwrap_or_else(|| "world".into());
            self.start(host, &t.code, Ctx::default().with("world", Value::World(world)), Vec::new());
        }
    }

    // ---- commands ----------------------------------------------------------------------------

    pub fn usage_of(cmd: &CommandDef) -> String {
        let mut out = format!("/{}", cmd.name);
        for p in &cmd.parts {
            match p {
                ArgPart::Word(w) => out.push_str(&format!(" {w}")),
                ArgPart::Arg(a) => {
                    let name = a.name.clone().unwrap_or_else(|| format!("{:?}", a.ty).to_lowercase());
                    out.push_str(&if a.optional { format!(" [<{name}>]") } else { format!(" <{name}>") });
                }
            }
        }
        out
    }

    /// Runs a script command. False when no script defines it.
    pub fn run_command(&mut self, host: &mut dyn Host, label: &str, sender: Value, raw_args: &str) -> bool {
        let label = label.to_lowercase();
        let Some(cmd) = self.commands.get(&label).cloned() else { return false };
        let player = match &sender {
            Value::Player(p) => Some(p.clone()),
            _ => None,
        };
        let mut ctx = Ctx::default().with("sender", sender.clone()).with("command", Value::text(label.clone()));
        ctx.vals.insert("full command".into(), Value::text(format!("{label} {raw_args}").trim().to_owned()));
        if player.is_some() {
            ctx.vals.insert("player".into(), sender.clone());
            let world = host.get(&sender, Prop::World);
            ctx.vals.insert("world".into(), world);
        }
        let say = |eng: &mut Engine, host: &mut dyn Host, ctx: &Ctx, custom: &Option<Expr>, default: String| {
            let text = match custom {
                Some(e) => {
                    let mut task = Task { frames: vec![Frame::new(Arc::clone(&cmd.code), false)], ctx: ctx.clone(), wake: 0, script: cmd.script.clone() };
                    let mut rt = Rt { host: &mut *host, budget: 2000, depth: 0, capped: false };
                    to_text(&eng.eval(&mut rt, &mut task, e))
                }
                None => default,
            };
            host.send(&sender, &text, SendKind::Chat);
        };
        match (cmd.executable_by, player.is_some()) {
            (ExecBy::Players, false) => {
                let m = self.msgs.players_only.clone();
                say(self, host, &ctx, &None, m);
                return true;
            }
            (ExecBy::Console, true) => {
                let m = self.msgs.console_only.clone();
                say(self, host, &ctx, &None, m);
                return true;
            }
            _ => {}
        }
        if let (Some(node), Some(_)) = (&cmd.permission, &player) {
            if !host.check(Check::HasPerm, &[sender.clone(), Value::text(node.clone())]) {
                let m = self.msgs.no_permission.clone();
                say(self, host, &ctx, &cmd.permission_message, m);
                return true;
            }
        }
        // arguments
        let usage = |eng: &mut Engine, host: &mut dyn Host, ctx: &Ctx| {
            let m = eng.msgs.usage.replace("{usage}", &Self::usage_of(&cmd));
            say(eng, host, ctx, &cmd.usage, m);
        };
        let words: Vec<&str> = raw_args.split_whitespace().collect();
        let mut at = 0;
        let mut locals = Vec::new();
        let last_arg = cmd.parts.iter().rposition(|p| matches!(p, ArgPart::Arg(_)));
        for (i, part) in cmd.parts.iter().enumerate() {
            match part {
                ArgPart::Word(w) => {
                    if !words.get(at).is_some_and(|x| x.eq_ignore_ascii_case(w)) {
                        usage(self, host, &ctx);
                        return true;
                    }
                    at += 1;
                }
                ArgPart::Arg(def) => {
                    let value = if at >= words.len() {
                        if !def.optional {
                            usage(self, host, &ctx);
                            return true;
                        }
                        match &def.default {
                            Some(e) => {
                                let mut task = Task { frames: vec![Frame::new(Arc::clone(&cmd.code), false)], ctx: ctx.clone(), wake: 0, script: cmd.script.clone() };
                                let mut rt = Rt { host: &mut *host, budget: 2000, depth: 0, capped: false };
                                let v = self.eval(&mut rt, &mut task, e);
                                match (&def.ty, v) {
                                    (ArgType::Text, v) if !v.is_none() => Value::Text(to_text(&v)),
                                    (_, v) => v,
                                }
                            }
                            None => Value::None,
                        }
                    } else {
                        let greedy = Some(i) == last_arg && matches!(def.ty, ArgType::Text | ArgType::Timespan);
                        let raw = if greedy { words[at..].join(" ") } else { words[at].to_owned() };
                        at = if greedy { words.len() } else { at + 1 };
                        let raw = clamp_text(raw, self.limits.max_text);
                        let parsed = match def.ty {
                            ArgType::Text => Some(Value::Text(raw.clone())),
                            ArgType::Number => value::parse_number(&raw).map(Value::Num),
                            ArgType::Integer => raw.parse::<i64>().ok().map(|n| Value::Num(n as f64)),
                            ArgType::Boolean => match raw.to_lowercase().as_str() {
                                "true" | "yes" | "on" => Some(Value::Bool(true)),
                                "false" | "no" | "off" => Some(Value::Bool(false)),
                                _ => None,
                            },
                            ArgType::Player => match host.find_player(&raw) {
                                Some(p) => Some(Value::Player(p)),
                                None => {
                                    let m = self.msgs.no_player.replace("{name}", &raw);
                                    host.send(&sender, &m, SendKind::Chat);
                                    return true;
                                }
                            },
                            ArgType::OfflinePlayer => Some(Value::Player(host.offline_player(&raw))),
                            ArgType::Item => host.item(&raw).map(Value::Item),
                            ArgType::World => Some(Value::World(value::world_name(&raw))),
                            ArgType::Timespan => short_span(&raw).map(Value::Span),
                        };
                        match parsed {
                            Some(v) => v,
                            None => {
                                usage(self, host, &ctx);
                                return true;
                            }
                        }
                    };
                    if let Some(name) = &def.name {
                        locals.push((name.clone(), value.clone()));
                    }
                    ctx.args.push((def.ty.clone(), value));
                }
            }
        }
        if at < words.len() {
            usage(self, host, &ctx);
            return true;
        }
        // cooldown
        if let (Some(p), true) = (&player, cmd.cooldown > 0.0) {
            let bypass = cmd.cooldown_bypass.as_ref().is_some_and(|node| host.check(Check::HasPerm, &[sender.clone(), Value::text(node.clone())]));
            if !bypass {
                let key = (cmd.name.clone(), p.uuid.clone());
                let now = host.now();
                if let Some(last) = self.cooldowns.get(&key) {
                    let elapsed = now - last;
                    if elapsed < cmd.cooldown {
                        let left = (cmd.cooldown - elapsed).ceil();
                        ctx.vals.insert("remaining time".into(), Value::Span(left));
                        ctx.vals.insert("elapsed time".into(), Value::Span(elapsed.floor()));
                        ctx.vals.insert("cooldown time".into(), Value::Span(cmd.cooldown));
                        let m = self.msgs.cooldown.replace("{time}", &value::fmt_span(left));
                        say(self, host, &ctx, &cmd.cooldown_message, m);
                        return true;
                    }
                }
                self.cooldowns.insert(key.clone(), now);
                ctx.cooldown_key = Some(key);
                if self.cooldowns.len() > 20_000 {
                    self.cooldowns.clear();
                }
            }
        }
        self.start(host, &cmd.code, ctx, locals);
        true
    }

    // ---- interpreter -------------------------------------------------------------------------

    fn run(&mut self, rt: &mut Rt, task: &mut Task, sync: bool) -> Option<End> {
        let mut last = Value::None;
        loop {
            let Some(frame) = task.frames.last_mut() else { return Some(End::Done(last)) };
            let code = Arc::clone(&frame.code);
            let ip = frame.ip;
            let Some(op) = code.ops.get(ip) else {
                task.frames.pop();
                continue;
            };
            if rt.budget == 0 {
                let msg = if rt.capped { self.msgs.tick_limit.clone() } else { self.msgs.limit.replace("{n}", &self.limits.run_ops.to_string()) };
                self.report(rt.host, &code, ip, &msg);
                task.frames.clear();
                return None;
            }
            rt.budget -= 1;
            frame.ip += 1;
            match op {
                Op::Effect(effect) => self.effect(rt, task, effect, &code, ip),
                Op::JumpIfNot(cond, target) => {
                    if !self.cond(rt, task, cond) {
                        Self::jump(task, *target);
                    }
                }
                Op::Jump(target) => Self::jump(task, *target),
                Op::LoopInit(src, end) => {
                    let lf = match src {
                        LoopSrc::Times(e) => {
                            let n = self.eval(rt, task, e).num().unwrap_or(0.0).floor();
                            (n >= 1.0).then_some(LoopFrame { items: Vec::new(), pos: 0, counter: Some((1.0, n)), iteration: 1 })
                        }
                        LoopSrc::Range(a, b) => {
                            let (a, b) = (self.eval(rt, task, a).num().unwrap_or(0.0).ceil(), self.eval(rt, task, b).num().unwrap_or(0.0).floor());
                            (a <= b).then_some(LoopFrame { items: Vec::new(), pos: 0, counter: Some((a, b)), iteration: 1 })
                        }
                        LoopSrc::Items(e) => {
                            let items: Vec<(Value, Value)> = match e {
                                Expr::Var(name) if name.list => {
                                    let key = self.var_key(rt, task, name);
                                    let list = if name.local { task.frames.last().map(|f| f.locals.list(&key)).unwrap_or_default() } else { self.vars.list(&key) };
                                    list.into_iter().map(|(k, v)| (Value::Text(k), v)).collect()
                                }
                                _ => self.eval(rt, task, e).flat().into_iter().enumerate().map(|(i, v)| (Value::Num(i as f64 + 1.0), v)).collect(),
                            };
                            (!items.is_empty()).then_some(LoopFrame { items, pos: 0, counter: None, iteration: 1 })
                        }
                    };
                    match lf {
                        Some(lf) => {
                            if let Some(f) = task.frames.last_mut() {
                                f.loops.push(lf);
                            }
                        }
                        None => Self::jump(task, *end),
                    }
                }
                Op::LoopNext(start) => {
                    if let Some(f) = task.frames.last_mut() {
                        let more = match f.loops.last_mut() {
                            Some(l) => {
                                l.iteration += 1;
                                match &mut l.counter {
                                    Some((cur, end)) => {
                                        *cur += 1.0;
                                        *cur <= *end
                                    }
                                    None => {
                                        l.pos += 1;
                                        l.pos < l.items.len()
                                    }
                                }
                            }
                            None => false,
                        };
                        if more {
                            f.ip = *start;
                        } else {
                            f.loops.pop();
                        }
                    }
                }
                Op::PopLoops(n) => {
                    if let Some(f) = task.frames.last_mut() {
                        let keep = f.loops.len().saturating_sub(*n);
                        f.loops.truncate(keep);
                    }
                }
                Op::Wait(e) => {
                    let secs = match self.eval(rt, task, e) {
                        Value::Span(s) => s,
                        other => other.num().unwrap_or(0.0),
                    };
                    if sync {
                        let msg = self.msgs.wait_in_value.clone();
                        self.report(rt.host, &code, ip, &msg);
                    } else {
                        return Some(End::Wait((secs * 20.0).round().clamp(1.0, 1.728e8) as u64));
                    }
                }
                Op::WaitFor(cond, while_true) => {
                    if sync {
                        let msg = self.msgs.wait_in_value.clone();
                        self.report(rt.host, &code, ip, &msg);
                    } else if self.cond(rt, task, cond) == *while_true {
                        // not yet: look again next tick
                        Self::jump(task, ip);
                        return Some(End::Wait(1));
                    }
                }
                Op::Return(e) => {
                    last = match e {
                        Some(e) => self.eval(rt, task, e),
                        None => Value::None,
                    };
                    task.frames.pop();
                    if sync || task.frames.is_empty() {
                        return Some(End::Done(last));
                    }
                }
                Op::Stop => {
                    let function = task.frames.last().is_some_and(|f| f.function);
                    if function && task.frames.len() > 1 {
                        task.frames.pop();
                    } else {
                        task.frames.clear();
                        return Some(End::Done(Value::None));
                    }
                }
            }
        }
    }

    fn jump(task: &mut Task, target: usize) {
        if let Some(f) = task.frames.last_mut() {
            f.ip = target;
        }
    }

    fn var_key(&mut self, rt: &mut Rt, task: &mut Task, name: &VarName) -> String {
        let mut key = String::new();
        for p in &name.parts {
            match p {
                TextPart::Lit(s) => key.push_str(s),
                TextPart::Expr(e) => key.push_str(&to_text(&self.eval(rt, task, e))),
            }
        }
        clamp_text(key.to_lowercase(), 200)
    }

    fn text(&mut self, rt: &mut Rt, task: &mut Task, parts: &[TextPart]) -> String {
        let mut out = String::new();
        for p in parts {
            match p {
                TextPart::Lit(s) => out.push_str(s),
                TextPart::Expr(e) => {
                    let v = if is_event_block(e) && task.ctx.vals.contains_key("block type") { task.ctx.get("block type") } else { self.eval(rt, task, e) };
                    out.push_str(&self.show(rt, &v));
                }
            }
            if out.len() > self.limits.max_text {
                break;
            }
        }
        clamp_text(out, self.limits.max_text)
    }

    fn show(&mut self, rt: &mut Rt, v: &Value) -> String {
        match v {
            Value::Date(d) => value::fmt_date(*d + rt.host.date_offset(), value::DATE_FORMAT),
            Value::Block(_) => to_text(&rt.host.get(v, Prop::Type)),
            Value::List(l) => value::join_and(&l.iter().map(|x| self.show(rt, x)).collect::<Vec<_>>()),
            other => to_text(other),
        }
    }

    fn var_get(&mut self, rt: &mut Rt, task: &mut Task, name: &VarName) -> Value {
        let key = self.var_key(rt, task, name);
        let store = if name.local { task.frames.last().map(|f| &f.locals) } else { Some(&self.vars) };
        let Some(store) = store else { return Value::None };
        if name.list { Value::from_list(store.list(&key).into_iter().map(|(_, v)| v).collect()) } else { store.get(&key) }
    }

    fn call(&mut self, rt: &mut Rt, def: &Arc<FunctionDef>, args: Vec<Value>) -> Option<Frame> {
        let mut frame = Frame::new(Arc::clone(&def.code), true);
        let mut args = args.into_iter();
        for (name, default) in &def.params {
            let v = match (args.next(), default) {
                (Some(v), _) if !v.is_none() => v,
                (_, Some(d)) => {
                    let mut empty = Task { frames: Vec::new(), ctx: Ctx::default(), wake: 0, script: String::new() };
                    self.eval(rt, &mut empty, d)
                }
                _ => Value::None,
            };
            match v {
                Value::List(items) => {
                    for (i, item) in items.into_iter().enumerate() {
                        frame.locals.set(&format!("{name}::{}", i + 1), item, self.limits.max_list);
                    }
                }
                v => {
                    frame.locals.set(name, v, usize::MAX);
                }
            }
        }
        Some(frame)
    }

    fn call_value(&mut self, rt: &mut Rt, name: &str, args: Vec<Value>, code: Option<(&Code, usize)>) -> Value {
        let Some(def) = self.functions.get(name).cloned() else { return Value::None };
        if rt.depth >= self.limits.call_depth {
            let msg = self.msgs.recursion.clone();
            if let Some((code, ip)) = code {
                self.report(rt.host, code, ip, &msg);
            }
            return Value::None;
        }
        let Some(frame) = self.call(rt, &def, args) else { return Value::None };
        let mut task = Task { frames: vec![frame], ctx: Ctx::default(), wake: 0, script: def.script.clone() };
        rt.depth += 1;
        let end = self.run(rt, &mut task, true);
        rt.depth -= 1;
        match end {
            Some(End::Done(v)) => v,
            _ => Value::None,
        }
    }

    fn builtin(&mut self, rt: &mut Rt, name: &str, args: Vec<Value>) -> Option<Value> {
        let n = |i: usize| args.get(i).and_then(Value::num).unwrap_or(0.0);
        let all = || args.iter().cloned().flat_map(Value::flat).filter_map(|v| v.num());
        Some(match name {
            "round" => {
                let scale = 10f64.powi(n(1).clamp(0.0, 9.0) as i32);
                Value::Num((n(0) * scale).round() / scale)
            }
            "floor" => Value::Num(n(0).floor()),
            "ceil" | "ceiling" => Value::Num(n(0).ceil()),
            "abs" => Value::Num(n(0).abs()),
            "sqrt" => Value::Num(n(0).max(0.0).sqrt()),
            "pow" => Value::Num(finite(n(0).powf(n(1)))),
            "mod" => Value::Num(if n(1) == 0.0 { 0.0 } else { n(0).rem_euclid(n(1)) }),
            "min" => all().reduce(f64::min).map_or(Value::None, Value::Num),
            "max" => all().reduce(f64::max).map_or(Value::None, Value::Num),
            "sum" => Value::Num(all().sum()),
            "world" => Value::World(value::world_name(&to_text(args.first()?))),
            "player" => rt.host.find_player(&to_text(args.first()?)).map_or(Value::None, Value::Player),
            "offlineplayer" => Value::Player(rt.host.offline_player(&to_text(args.first()?))),
            "location" => {
                let world = match args.get(3) {
                    Some(Value::World(w)) => w.clone(),
                    Some(Value::Text(t)) => value::world_name(t),
                    _ => "world".into(),
                };
                Value::Loc(Loc { x: n(0), y: n(1), z: n(2), world, yaw: n(4) as f32, pitch: n(5) as f32 })
            }
            _ => return None,
        })
    }

    fn loc_of(&mut self, rt: &mut Rt, v: &Value) -> Option<Loc> {
        match v {
            Value::Loc(l) => Some(l.clone()),
            Value::Block(l) => {
                let (x, y, z) = l.block();
                Some(Loc { x: f64::from(x), y: f64::from(y), z: f64::from(z), ..l.clone() })
            }
            Value::Player(_) | Value::Entity(_) => match rt.host.get(v, Prop::Location) {
                Value::Loc(l) => Some(l),
                _ => None,
            },
            Value::List(l) if l.len() == 1 => self.loc_of(rt, &l[0]),
            _ => None,
        }
    }

    fn prop(&mut self, rt: &mut Rt, target: Value, prop: Prop) -> Value {
        if let Value::List(items) = target {
            return Value::from_list(items.into_iter().map(|v| self.prop(rt, v, prop)).filter(|v| !v.is_none()).collect());
        }
        match (prop, &target) {
            (_, Value::None) => Value::None,
            (Prop::Name, Value::Player(p)) => Value::Text(p.name.clone()),
            (Prop::Uuid, Value::Player(p)) => Value::Text(p.uuid.clone()),
            (Prop::Name, Value::Console) => Value::text("Console"),
            // the name of a menu is its title
            (Prop::Name, Value::Text(t)) => Value::Text(t.clone()),
            (Prop::Name | Prop::Type, Value::Entity(e)) => Value::Text(e.kind.replace('_', " ")),
            (Prop::Name, Value::World(w)) => Value::Text(w.clone()),
            (Prop::Name, Value::Item(i)) => Value::Text(i.name.clone().unwrap_or_else(|| i.id.replace('_', " "))),
            (Prop::Type, Value::Item(i)) => Value::Item(Item::new(&i.id, 1)),
            (Prop::Amount, Value::Item(i)) => Value::Num(f64::from(i.count)),
            (Prop::Enchants, Value::Item(i)) => Value::from_list(i.ench.iter().map(|(id, level)| Value::Text(format!("{} {level}", id.replace('_', " ")))).collect()),
            (Prop::Length, v) => Value::Num(to_text(v).chars().count() as f64),
            (Prop::X | Prop::Y | Prop::Z | Prop::Yaw | Prop::Pitch | Prop::Location | Prop::World, Value::Loc(_) | Value::Block(_) | Value::Player(_) | Value::Entity(_)) => {
                if prop == Prop::World && matches!(target, Value::Player(_)) {
                    return rt.host.get(&target, Prop::World);
                }
                let Some(l) = self.loc_of(rt, &target) else { return Value::None };
                match prop {
                    Prop::X => Value::Num(l.x),
                    Prop::Y => Value::Num(l.y),
                    Prop::Z => Value::Num(l.z),
                    Prop::Yaw => Value::Num(f64::from(l.yaw)),
                    Prop::Pitch => Value::Num(f64::from(l.pitch)),
                    Prop::World => Value::World(l.world),
                    _ => Value::Loc(l),
                }
            }
            _ => rt.host.get(&target, prop),
        }
    }

    fn eval(&mut self, rt: &mut Rt, task: &mut Task, e: &Expr) -> Value {
        match e {
            Expr::Lit(v) => v.clone(),
            Expr::Text(parts) => Value::Text(self.text(rt, task, parts)),
            Expr::Var(name) => self.var_get(rt, task, name),
            Expr::Ev(key) => match task.ctx.vals.get(key.as_str()) {
                Some(v) => v.clone(),
                // inside a command `player` is whoever ran it
                None if key == "player" => task.ctx.get("sender"),
                None => Value::None,
            },
            Expr::Arg(n, ty) => match (n, ty) {
                (0, None) => Value::Text(task.ctx.args.iter().map(|(_, v)| to_text(v)).filter(|s| s != "<none>").collect::<Vec<_>>().join(" ")),
                (0, Some(ty)) => {
                    let want = parse_arg_type(ty);
                    task.ctx.args.iter().find(|(t, _)| Some(t) == want.as_ref()).map_or(Value::None, |(_, v)| v.clone())
                }
                (n, _) => task.ctx.args.get(n - 1).map_or(Value::None, |(_, v)| v.clone()),
            },
            Expr::Loop(kind, nth) => {
                let Some(f) = task.frames.last() else { return Value::None };
                let l = if *nth == 0 { f.loops.last() } else { f.loops.get(nth - 1) };
                let Some(l) = l else { return Value::None };
                match (kind, &l.counter) {
                    (2, _) => Value::Num(l.iteration as f64),
                    (_, Some((cur, _))) => Value::Num(*cur),
                    (1, None) => l.items.get(l.pos).map_or(Value::None, |(k, _)| k.clone()),
                    (_, None) => l.items.get(l.pos).map_or(Value::None, |(_, v)| v.clone()),
                }
            }
            Expr::Prop(inner, Prop::Type) if is_event_block(inner) && task.ctx.vals.contains_key("block type") => task.ctx.get("block type"),
            Expr::Prop(inner, prop) => {
                let target = self.eval(rt, task, inner);
                self.prop(rt, target, *prop)
            }
            Expr::Neg(inner) => self.eval(rt, task, inner).num().map_or(Value::None, |n| Value::Num(-n)),
            Expr::Bin(a, op, b) => {
                let (a, b) = (self.eval(rt, task, a), self.eval(rt, task, b));
                self.arith(a, *op, b)
            }
            Expr::List(items, _) => {
                let mut out = Vec::new();
                for i in items {
                    out.extend(self.eval(rt, task, i).flat());
                    if out.len() > self.limits.max_list {
                        break;
                    }
                }
                Value::from_list(out)
            }
            Expr::Call(name, args) => {
                let vals: Vec<Value> = args.iter().map(|a| self.eval(rt, task, a)).collect();
                if self.functions.contains_key(name) {
                    let at = task.frames.last().map(|f| (Arc::clone(&f.code), f.ip.saturating_sub(1)));
                    return self.call_value(rt, name, vals, at.as_ref().map(|(c, ip)| (&**c, *ip)));
                }
                self.builtin(rt, name, vals).unwrap_or(Value::None)
            }
            Expr::Op(op, args, tag) => {
                let vals: Vec<Value> = args.iter().map(|a| self.eval(rt, task, a)).collect();
                self.op(rt, *op, vals, tag)
            }
        }
    }

    fn arith(&mut self, a: Value, op: char, b: Value) -> Value {
        use Value::{Date, Num, Span, Text};
        match (&a, op, &b) {
            (Date(x), '-', Date(y)) => return Span(x - y),
            (Date(x), '+', Span(y)) | (Span(y), '+', Date(x)) => return Date(x + y),
            (Date(x), '-', Span(y)) => return Date(x - y),
            (Span(x), '+', Span(y)) => return Span(x + y),
            (Span(x), '-', Span(y)) => return Span((x - y).max(0.0)),
            (Span(x), '*', Num(y)) | (Num(y), '*', Span(x)) => return Span(x * y),
            (Span(x), '/', Num(y)) if *y != 0.0 => return Span(x / y),
            (Text(_), '+', _) | (_, '+', Text(_)) if a.num().is_none() || b.num().is_none() => {
                return Text(clamp_text(format!("{}{}", to_text(&a), to_text(&b)), self.limits.max_text));
            }
            _ => {}
        }
        // an unset value counts as 0, as in current Skript
        let x = if a.is_none() { Some(0.0) } else { a.num() };
        let y = if b.is_none() { Some(0.0) } else { b.num() };
        let (Some(x), Some(y)) = (x, y) else { return Value::None };
        Num(finite(match op {
            '+' => x + y,
            '-' => x - y,
            '*' => x * y,
            '/' => {
                if y == 0.0 {
                    0.0
                } else {
                    x / y
                }
            }
            _ => x.powf(y),
        }))
    }

    fn op(&mut self, rt: &mut Rt, op: ExprOp, mut a: Vec<Value>, tag: &str) -> Value {
        let text = |i: usize| a.get(i).map(to_text).unwrap_or_default();
        let num = |i: usize| a.get(i).and_then(Value::num);
        match op {
            ExprOp::Default => a.into_iter().find(|v| !v.is_none()).unwrap_or(Value::None),
            ExprOp::Now => Value::Date(rt.host.now()),
            ExprOp::BlocksAround => {
                let Some(center) = a.get(1).and_then(|v| self.loc_of(rt, v)) else { return Value::None };
                let r = num(0).unwrap_or(0.0).clamp(0.0, 12.0);
                let (cx, cy, cz) = center.block();
                let n = r.floor() as i32;
                let mut out = Vec::new();
                for x in -n..=n {
                    for y in -n..=n {
                        for z in -n..=n {
                            if f64::from(x * x + y * y + z * z) <= r * r && out.len() < self.limits.max_list {
                                out.push(Value::Block(Loc { x: f64::from(cx + x), y: f64::from(cy + y), z: f64::from(cz + z), world: center.world.clone(), yaw: 0.0, pitch: 0.0 }));
                            }
                        }
                    }
                }
                Value::from_list(out)
            }
            ExprOp::Entities => {
                let found = match a.get(1).and_then(|v| self.loc_of(rt, v)) {
                    Some(center) => rt.host.entities(Some((&center, num(0).unwrap_or(0.0).clamp(0.0, 64.0)))),
                    None if a.is_empty() => rt.host.entities(None),
                    None => Vec::new(),
                };
                Value::from_list(found)
            }
            ExprOp::Enchanted => match a.pop() {
                Some(Value::Item(mut i)) => {
                    for (id, level) in tag.split(',').filter_map(|e| e.split_once(':')) {
                        i.ench.retain(|(have, _)| have != id);
                        i.ench.push((id.to_owned(), level.parse().unwrap_or(1)));
                    }
                    Value::Item(i)
                }
                _ => Value::None,
            },
            ExprOp::Tps => Value::Num(rt.host.tps()),
            ExprOp::AllPlayers => Value::from_list(rt.host.players().into_iter().map(Value::Player).collect()),
            ExprOp::PlayersIn => {
                let want = a.first().map(|w| value::world_name(&to_text(w))).unwrap_or_default();
                let mut out = Vec::new();
                for p in rt.host.players() {
                    let p = Value::Player(p);
                    if value::world_name(&to_text(&rt.host.get(&p, Prop::World))) == want {
                        out.push(p);
                    }
                }
                Value::from_list(out)
            }
            ExprOp::WorldOf => match a.first() {
                Some(Value::World(w)) => Value::World(w.clone()),
                Some(v) if !v.is_none() => Value::World(value::world_name(&to_text(v))),
                _ => Value::None,
            },
            ExprOp::BlockAt => a.first().and_then(|v| self.loc_of(rt, v)).map_or(Value::None, Value::Block),
            ExprOp::BlockRel => match a.get(1).and_then(|v| self.loc_of(rt, v)) {
                Some(mut l) => {
                    l.y += num(0).unwrap_or(0.0);
                    Value::Block(l)
                }
                None => Value::None,
            },
            ExprOp::TargetBlock => rt.host.target_block(a.first().unwrap_or(&Value::None)),
            ExprOp::Distance => match (a.first().and_then(|v| self.loc_of(rt, v)), a.get(1).and_then(|v| self.loc_of(rt, v))) {
                (Some(p), Some(q)) if p.world == q.world => Value::Num(((p.x - q.x).powi(2) + (p.y - q.y).powi(2) + (p.z - q.z).powi(2)).sqrt()),
                _ => Value::None,
            },
            ExprOp::Difference => match (a.first(), a.get(1)) {
                (Some(Value::Date(x)), Some(Value::Date(y))) => Value::Span((x - y).abs()),
                (Some(x), Some(y)) => match (x.num(), y.num()) {
                    (Some(p), Some(q)) => {
                        if matches!(x, Value::Span(_)) {
                            Value::Span((p - q).abs())
                        } else {
                            Value::Num((p - q).abs())
                        }
                    }
                    _ => Value::None,
                },
                _ => Value::None,
            },
            ExprOp::TimeSince => match a.first() {
                Some(Value::Date(d)) => Value::Span((rt.host.now() - d).max(0.0)),
                _ => Value::None,
            },
            ExprOp::Formatted => match a.first() {
                Some(Value::Date(d)) => {
                    let pattern = a.get(1).map(to_text).unwrap_or_else(|| value::DATE_FORMAT.to_owned());
                    Value::Text(value::fmt_date(*d + rt.host.date_offset(), &clamp_text(pattern, 64)))
                }
                Some(v) => Value::Text(self.show(rt, v)),
                None => Value::None,
            },
            ExprOp::RandInt | ExprOp::RandNum => {
                let (Some(x), Some(y)) = (num(0), num(1)) else { return Value::None };
                let (lo, hi) = (x.min(y), x.max(y));
                let r = rt.host.random();
                if op == ExprOp::RandInt {
                    let (lo, hi) = (lo.ceil(), hi.floor());
                    if lo > hi {
                        return Value::None;
                    }
                    Value::Num((lo + (r * (hi - lo + 1.0)).floor()).min(hi))
                } else {
                    Value::Num(lo + r * (hi - lo))
                }
            }
            ExprOp::RandElem | ExprOp::FirstElem | ExprOp::LastElem => {
                let mut items = a.pop().unwrap_or(Value::None).flat();
                if items.is_empty() {
                    return Value::None;
                }
                let i = match op {
                    ExprOp::FirstElem => 0,
                    ExprOp::LastElem => items.len() - 1,
                    _ => ((rt.host.random() * items.len() as f64) as usize).min(items.len() - 1),
                };
                items.swap_remove(i)
            }
            ExprOp::Size => Value::Num(a.pop().unwrap_or(Value::None).flat().len() as f64),
            ExprOp::ParsedAs => {
                let raw = text(0);
                let raw = raw.trim();
                match tag {
                    "number" => value::parse_number(raw).map_or(Value::None, Value::Num),
                    "integer" => value::parse_number(raw).map_or(Value::None, |n| Value::Num(n.trunc())),
                    "player" => rt.host.find_player(raw).map_or(Value::None, Value::Player),
                    "offline player" | "offlineplayer" => Value::Player(rt.host.offline_player(raw)),
                    "timespan" | "time span" => value::parse_span(raw).or_else(|| short_span(raw)).map_or(Value::None, Value::Span),
                    "item" | "itemtype" | "item type" => rt.host.item(raw).map_or(Value::None, Value::Item),
                    "boolean" => match raw.to_lowercase().as_str() {
                        "true" | "yes" | "on" => Value::Bool(true),
                        "false" | "no" | "off" => Value::Bool(false),
                        _ => Value::None,
                    },
                    "world" => Value::World(value::world_name(raw)),
                    _ => Value::Text(raw.to_owned()),
                }
            }
            ExprOp::Upper => Value::Text(text(0).to_uppercase()),
            ExprOp::Lower => Value::Text(text(0).to_lowercase()),
            ExprOp::Capital => {
                let t = text(0);
                let mut c = t.chars();
                Value::Text(c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default())
            }
            ExprOp::Uncolored => Value::Text(value::uncolor(&text(0))),
            ExprOp::Subtext => {
                let chars: Vec<char> = text(0).chars().collect();
                let from = (num(1).unwrap_or(1.0).max(1.0) as usize).min(chars.len() + 1);
                let to = (num(2).unwrap_or(0.0).max(0.0) as usize).min(chars.len());
                Value::Text(if from > to { String::new() } else { chars[from - 1..to].iter().collect() })
            }
            ExprOp::FirstChars | ExprOp::LastChars => {
                let chars: Vec<char> = text(1).chars().collect();
                let n = (num(0).unwrap_or(0.0).max(0.0) as usize).min(chars.len());
                Value::Text(if op == ExprOp::FirstChars { chars[..n].iter().collect() } else { chars[chars.len() - n..].iter().collect() })
            }
            ExprOp::Join => {
                let delim = if a.len() > 1 { text(1) } else { String::new() };
                let parts: Vec<String> = a.swap_remove(0).flat().iter().map(|v| self.show(rt, v)).collect();
                Value::Text(clamp_text(parts.join(&delim), self.limits.max_text))
            }
            ExprOp::Split => {
                let (t, d) = (text(0), text(1));
                if d.is_empty() {
                    return Value::Text(t);
                }
                Value::from_list(t.split(&d).take(self.limits.max_list).map(Value::text).collect())
            }
            ExprOp::Replace => Value::Text(clamp_text(replace_ci(&text(0), &text(1), &text(2)), self.limits.max_text)),
            ExprOp::IndexOf => {
                let (needle, hay) = (text(0).to_lowercase(), text(1).to_lowercase());
                Value::Num(hay.find(&needle).map_or(-1.0, |i| hay[..i].chars().count() as f64 + 1.0))
            }
            ExprOp::LocationAt => {
                let world = match a.get(3) {
                    Some(Value::World(w)) => w.clone(),
                    Some(v) if !v.is_none() => value::world_name(&to_text(v)),
                    _ => "world".into(),
                };
                match (num(0), num(1), num(2)) {
                    (Some(x), Some(y), Some(z)) => Value::Loc(Loc { x, y, z, world, yaw: 0.0, pitch: 0.0 }),
                    _ => Value::None,
                }
            }
            ExprOp::ItemOf => match (num(0), a.get(1)) {
                (Some(n), Some(Value::Item(i))) => Value::Item(Item { count: n.clamp(0.0, 2304.0) as u32, ..i.clone() }),
                _ => Value::None,
            },
            ExprOp::SpanOf => match (num(0), value::unit_seconds(tag)) {
                (Some(n), Some(unit)) => Value::Span((n * unit).max(0.0)),
                _ => Value::None,
            },
            ExprOp::Named | ExprOp::WithLore => {
                let extra = a.pop().unwrap_or(Value::None);
                match a.pop() {
                    Some(Value::Item(mut i)) => {
                        if op == ExprOp::Named {
                            i.name = Some(clamp_text(to_text(&extra), 200));
                        } else {
                            i.lore = extra.flat().iter().take(20).map(|l| clamp_text(to_text(l), 200)).collect();
                        }
                        Value::Item(i)
                    }
                    _ => Value::None,
                }
            }
        }
    }

    // ---- conditions --------------------------------------------------------------------------

    /// A block compares by what it is made of.
    fn solid(&mut self, rt: &mut Rt, v: Value, other: &Value) -> Value {
        match (&v, other) {
            (Value::Block(_), Value::Item(_) | Value::Text(_) | Value::List(_)) => rt.host.get(&v, Prop::Type),
            _ => v,
        }
    }

    fn compare(&mut self, rt: &mut Rt, a: &Value, op: CmpOp, b: &Value) -> bool {
        let a = self.solid(rt, a.clone(), b);
        let b = self.solid(rt, b.clone(), &a);
        match op {
            CmpOp::Eq => loose_eq(&a, &b),
            CmpOp::Ne => !loose_eq(&a, &b),
            _ => match loose_cmp(&a, &b) {
                Some(o) => match op {
                    CmpOp::Gt => o.is_gt(),
                    CmpOp::Ge => o.is_ge(),
                    CmpOp::Lt => o.is_lt(),
                    _ => o.is_le(),
                },
                None => false,
            },
        }
    }

    fn cond(&mut self, rt: &mut Rt, task: &mut Task, c: &Cond) -> bool {
        match c {
            Cond::All(list) => list.iter().all(|c| self.cond(rt, task, c)),
            Cond::Any(list) => list.iter().any(|c| self.cond(rt, task, c)),
            Cond::Cmp(l, op, r) => {
                // the block of the event is compared by the type it had when the event happened
                let known = task.ctx.vals.get("block type").cloned();
                let side = |eng: &mut Engine, rt: &mut Rt, task: &mut Task, e: &Expr, other: &Expr| match &known {
                    Some(ty) if is_event_block(e) && !matches!(other, Expr::Op(ExprOp::BlockAt | ExprOp::BlockRel, ..)) => ty.clone(),
                    _ => eng.eval(rt, task, e),
                };
                let left = side(self, rt, task, l, r);
                let right = side(self, rt, task, r, l);
                let any = matches!(r, Expr::List(_, false));
                let (lefts, rights) = (if left.is_none() { vec![Value::None] } else { left.flat() }, if right.is_none() { vec![Value::None] } else { right.flat() });
                // `is not a or b` means neither
                let (base, negate) = if *op == CmpOp::Ne { (CmpOp::Eq, true) } else { (*op, false) };
                let ok = lefts.iter().all(|a| {
                    if any || base == CmpOp::Eq && rights.len() > 1 && !matches!(r, Expr::List(_, true)) {
                        rights.iter().any(|b| self.compare(rt, a, base, b))
                    } else {
                        rights.iter().all(|b| self.compare(rt, a, base, b))
                    }
                });
                ok != negate
            }
            Cond::Check(kind, args, neg) => {
                if *kind == Check::IsSet {
                    return args.first().is_some_and(|a| !self.eval(rt, task, a).is_none()) != *neg;
                }
                if *kind == Check::Cancelled {
                    return task.ctx.cancelled != *neg;
                }
                let vals: Vec<Value> = args.iter().map(|a| self.eval(rt, task, a)).collect();
                let any = matches!(args.get(1), Some(Expr::List(_, false)));
                self.check(rt, *kind, &vals, any) != *neg
            }
        }
    }

    /// `any`: the second argument was written as `a or b`.
    fn check(&mut self, rt: &mut Rt, kind: Check, v: &[Value], any: bool) -> bool {
        let none = Value::None;
        let a = v.first().unwrap_or(&none);
        let b = v.get(1).unwrap_or(&none);
        let ta = to_text(a).to_lowercase();
        match kind {
            Check::Chance => {
                let n = a.num().unwrap_or(0.0);
                rt.host.random() < if n > 1.0 { n / 100.0 } else { n }
            }
            Check::Afford => {
                let who = a.clone().flat();
                let need = b.num().unwrap_or(f64::INFINITY);
                !who.is_empty() && who.into_iter().all(|p| self.prop(rt, p, Prop::Balance).num().is_some_and(|have| have >= need))
            }
            Check::IsPlayer => !a.is_none() && a.clone().flat().iter().all(|v| matches!(v, Value::Player(_))),
            Check::Empty => a.is_none() || matches!(a, Value::Text(t) if t.is_empty()) || matches!(a, Value::Item(i) if i.id == "air" || i.count == 0),
            Check::Contains => match a {
                Value::List(items) => {
                    let wanted = b.clone().flat();
                    let has = |x: &Value| items.iter().any(|i| loose_eq(i, x));
                    !wanted.is_empty() && if any { wanted.iter().any(has) } else { wanted.iter().all(has) }
                }
                Value::None => false,
                Value::Text(_) => {
                    let wanted = b.clone().flat();
                    let has = |x: &Value| ta.contains(&to_text(x).to_lowercase());
                    !wanted.is_empty() && if any { wanted.iter().any(has) } else { wanted.iter().all(has) }
                }
                other => loose_eq(other, b),
            },
            Check::StartsWith => !b.is_none() && b.clone().flat().iter().any(|x| ta.starts_with(&to_text(x).to_lowercase())),
            Check::EndsWith => !b.is_none() && b.clone().flat().iter().any(|x| ta.ends_with(&to_text(x).to_lowercase())),
            Check::Matches => {
                let pattern = to_text(b);
                pattern.len() <= 300 && regex_lite::Regex::new(&format!("^(?:{pattern})$")).is_ok_and(|re| re.is_match(&to_text(a)))
            }
            Check::Between => match (a.num(), b.num(), v.get(2).and_then(Value::num)) {
                (Some(x), Some(lo), Some(hi)) => x >= lo.min(hi) && x <= lo.max(hi),
                _ => false,
            },
            Check::InWorld => {
                let targets = a.clone().flat();
                !targets.is_empty() && targets.iter().all(|p| value::world_name(&to_text(&self.prop(rt, p.clone(), Prop::World))) == value::world_name(&to_text(b)))
            }
            Check::HasPerm | Check::HasItem => {
                let (who, what) = (a.clone().flat(), b.clone().flat());
                !who.is_empty()
                    && !what.is_empty()
                    && who.iter().all(|p| {
                        let mut has = |x: &Value| rt.host.check(kind, &[p.clone(), x.clone()]);
                        matches!(p, Value::Console) || if any { what.iter().any(&mut has) } else { what.iter().all(&mut has) }
                    })
            }
            _ => {
                let who = a.clone().flat();
                !who.is_empty() && who.iter().all(|p| rt.host.check(kind, &[p.clone()]))
            }
        }
    }

    // ---- effects -----------------------------------------------------------------------------

    fn effect(&mut self, rt: &mut Rt, task: &mut Task, effect: &Effect, code: &Arc<Code>, ip: usize) {
        match effect {
            Effect::Send(text, to, kind) => {
                let lines = self.eval(rt, task, text).flat();
                let to = match to {
                    Some(t) => self.eval(rt, task, t),
                    None => match task.ctx.get("player") {
                        Value::None => match task.ctx.get("sender") {
                            Value::None => Value::Console,
                            s => s,
                        },
                        p => p,
                    },
                };
                for target in to.flat() {
                    for line in &lines {
                        let line = self.show(rt, line);
                        rt.host.send(&target, &line, *kind);
                    }
                }
            }
            Effect::Broadcast(text) => {
                for line in self.eval(rt, task, text).flat() {
                    let line = self.show(rt, &line);
                    rt.host.broadcast(&line);
                }
            }
            Effect::Cancel(on) => task.ctx.cancelled = *on,
            Effect::CancelCooldown => {
                if let Some(key) = &task.ctx.cooldown_key {
                    self.cooldowns.remove(key);
                }
            }
            Effect::CallFn(name, args) => {
                let vals: Vec<Value> = args.iter().map(|a| self.eval(rt, task, a)).collect();
                match self.functions.get(name).cloned() {
                    Some(def) => {
                        if task.frames.len() >= self.limits.call_depth {
                            let msg = self.msgs.recursion.clone();
                            self.report(rt.host, code, ip, &msg);
                        } else if let Some(frame) = self.call(rt, &def, vals) {
                            task.frames.push(frame);
                        }
                    }
                    None => {
                        self.builtin(rt, name, vals);
                    }
                }
            }
            Effect::ReplaceIn(target, needle, with) => {
                let (hay, needle, with) = (self.eval(rt, task, target), self.eval(rt, task, needle), self.eval(rt, task, with));
                if hay.is_none() {
                    return;
                }
                let mut out = to_text(&hay);
                for n in needle.flat() {
                    out = replace_ci(&out, &to_text(&n), &to_text(&with));
                }
                let out = Value::Text(clamp_text(out, self.limits.max_text));
                self.change(rt, task, target, Mode::Set, out, code, ip);
            }
            Effect::Host(fx, args, flags) => {
                let mut vals: Vec<Value> = args.iter().map(|a| a.as_ref().map_or(Value::None, |e| self.eval(rt, task, e))).collect();
                // the player of the event is the default target
                let default_target = match fx {
                    Fx::Title => Some(2),
                    Fx::Sound if flags & 3 == 0 => Some(3),
                    _ => None,
                };
                if let Some(i) = default_target {
                    if vals.get(i).is_some_and(Value::is_none) {
                        vals[i] = task.ctx.get("player");
                    }
                }
                if *fx == Fx::Log {
                    vals[0] = Value::Text(self.show(rt, &vals[0]));
                }
                if let Err(msg) = rt.host.fx(*fx, &vals, *flags) {
                    self.report(rt.host, code, ip, &msg);
                }
            }
            Effect::Change(target, mode, value) => {
                let v = match value {
                    Some(e) => self.eval(rt, task, e),
                    None => Value::None,
                };
                self.change(rt, task, target, *mode, v, code, ip);
            }
        }
    }

    fn change(&mut self, rt: &mut Rt, task: &mut Task, target: &Expr, mode: Mode, v: Value, code: &Arc<Code>, ip: usize) {
        match target {
            Expr::Var(name) => {
                let key = self.var_key(rt, task, name);
                let (max_vars, max_list) = (self.limits.max_variables, self.limits.max_list);
                let store = if name.local {
                    match task.frames.last_mut() {
                        Some(f) => &mut f.locals,
                        None => return,
                    }
                } else {
                    &mut self.vars
                };
                let max = if name.local { max_list } else { max_vars };
                let mut full = false;
                if name.list {
                    match mode {
                        Mode::Set | Mode::Delete | Mode::Reset => {
                            store.clear_list(&key);
                            for (i, item) in v.flat().into_iter().take(max_list).enumerate() {
                                full |= !store.set(&format!("{key}::{}", i + 1), item, max);
                            }
                        }
                        Mode::Add => {
                            let mut next = store.next_index(&key);
                            if next > max_list {
                                full = true;
                            } else {
                                for item in v.flat() {
                                    full |= !store.set(&format!("{key}::{next}"), item, max);
                                    next += 1;
                                }
                            }
                        }
                        Mode::Remove | Mode::RemoveAll => {
                            for item in v.flat() {
                                for (index, have) in store.list(&key) {
                                    if loose_eq(&have, &item) {
                                        store.delete(&format!("{key}::{index}"));
                                        if mode == Mode::Remove {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    let old = store.get(&key);
                    let new = match mode {
                        Mode::Set => v,
                        Mode::Delete | Mode::Reset => Value::None,
                        Mode::Add | Mode::Remove | Mode::RemoveAll => {
                            let sign = if mode == Mode::Add { '+' } else { '-' };
                            match (&old, &v) {
                                (Value::None, v) if mode == Mode::Add && v.num().is_none() => v.clone(),
                                _ => {
                                    let mut sum = old;
                                    for item in v.flat() {
                                        sum = self_arith(sum, sign, item);
                                    }
                                    sum
                                }
                            }
                        }
                    };
                    if matches!(mode, Mode::Delete | Mode::Reset) {
                        store.delete(&key);
                    } else {
                        full |= !store.set(&key, new, max);
                    }
                }
                if full {
                    let msg = self.msgs.too_many_variables.clone();
                    self.report(rt.host, code, ip, &msg);
                }
            }
            Expr::Ev(key) => {
                let v = match key.as_str() {
                    "block" => {
                        let block = task.ctx.get("block");
                        if let Err(msg) = rt.host.set(&block, Prop::Type, &v) {
                            self.report(rt.host, code, ip, &msg);
                        }
                        return;
                    }
                    "damage" => Value::Num(v.num().unwrap_or(0.0).max(0.0)),
                    _ => Value::Text(self.show(rt, &v)),
                };
                task.ctx.vals.insert(key.clone(), v);
            }
            Expr::Op(..) => {
                let block = self.eval(rt, task, target);
                if let Err(msg) = rt.host.set(&block, Prop::Type, &v) {
                    self.report(rt.host, code, ip, &msg);
                }
            }
            Expr::Prop(owner, prop) => {
                let owners = self.eval(rt, task, owner).flat();
                for who in owners {
                    let result = match (prop, mode) {
                        (Prop::Inventory, Mode::Add) => rt.host.fx(Fx::Give, &[v.clone(), who.clone()], 0),
                        (Prop::Inventory, Mode::Remove | Mode::RemoveAll) => rt.host.fx(Fx::Take, &[v.clone(), who.clone()], u32::from(mode == Mode::RemoveAll)),
                        (Prop::Inventory, Mode::Delete | Mode::Reset) => rt.host.fx(Fx::ClearInventory, &[who.clone()], 0),
                        // money moves in one step on the other side: a withdrawal that does not fit changes nothing
                        (Prop::Balance, Mode::Add) => rt.host.fx(Fx::Money, &[who.clone(), v.clone()], 0),
                        (Prop::Balance, Mode::Remove | Mode::RemoveAll) => rt.host.fx(Fx::Money, &[who.clone(), v.clone()], 1),
                        (Prop::Balance, Mode::Delete | Mode::Reset) => rt.host.set(&who, Prop::Balance, &Value::Num(0.0)),
                        (_, Mode::Set) => rt.host.set(&who, *prop, &v),
                        (_, Mode::Delete | Mode::Reset) => rt.host.set(&who, *prop, &Value::None),
                        (_, Mode::Add | Mode::Remove | Mode::RemoveAll) => {
                            let old = self.prop(rt, who.clone(), *prop);
                            let new = self_arith(old, if mode == Mode::Add { '+' } else { '-' }, v.clone());
                            rt.host.set(&who, *prop, &new)
                        }
                    };
                    if let Err(msg) = result {
                        if *prop == Prop::Balance {
                            self.report_once(rt.host, code, ip, &msg);
                        } else {
                            self.report(rt.host, code, ip, &msg);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn is_event_block(e: &Expr) -> bool {
    matches!(e, Expr::Ev(k) if k == "block")
}

fn period(t: &Trigger) -> u64 {
    ((t.number * 20.0).round() as u64).max(1)
}

fn finite(n: f64) -> f64 {
    if n.is_finite() { n } else { 0.0 }
}

fn parse_arg_type(name: &str) -> Option<ArgType> {
    Some(match name {
        "text" | "string" => ArgType::Text,
        "player" => ArgType::Player,
        "offlineplayer" => ArgType::OfflinePlayer,
        "number" => ArgType::Number,
        "integer" | "int" => ArgType::Integer,
        "boolean" => ArgType::Boolean,
        "item" | "itemtype" => ArgType::Item,
        "world" => ArgType::World,
        "timespan" => ArgType::Timespan,
        _ => return None,
    })
}

/// Arithmetic for `add`/`remove` on a single value.
fn self_arith(a: Value, op: char, b: Value) -> Value {
    use Value::{Date, Num, Span};
    match (&a, &b) {
        (Span(x), Span(y)) => Span(if op == '+' { x + y } else { (x - y).max(0.0) }),
        (Date(x), Span(y)) => Date(if op == '+' { x + y } else { x - y }),
        (Value::None, Span(y)) if op == '+' => Span(*y),
        _ => {
            let x = if a.is_none() { Some(0.0) } else { a.num() };
            match (x, b.num()) {
                (Some(x), Some(y)) => Num(finite(if op == '+' { x + y } else { x - y })),
                _ => a,
            }
        }
    }
}

/// `30s`, `5m`, `2h`, `1d`, or a plain number of seconds.
pub fn short_span(raw: &str) -> Option<f64> {
    let raw = raw.trim().to_lowercase();
    if let Some(s) = value::parse_span(&raw) {
        return Some(s);
    }
    let split = raw.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(raw.len());
    let n = value::parse_number(&raw[..split]).filter(|n| *n >= 0.0)?;
    let unit = match &raw[split..] {
        "" | "s" => 1.0,
        "t" => 0.05,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86_400.0,
        "w" => 604_800.0,
        other => value::unit_seconds(other)?,
    };
    Some(n * unit)
}

/// Replace every occurrence, ignoring ASCII case (as Skript does by default).
pub fn replace_ci(hay: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return hay.to_owned();
    }
    // ASCII lowering keeps every byte offset, so the cuts below are always on character boundaries
    let (lower, needle) = (hay.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let mut out = String::with_capacity(hay.len());
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        out.push_str(&hay[from..from + i]);
        out.push_str(with);
        from += i + needle.len();
    }
    out.push_str(&hay[from..]);
    out
}
