//! What the parser produces and the interpreter runs.
use std::sync::Arc;

use crate::value::Value;

#[derive(Debug, Clone)]
pub enum TextPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct VarName {
    pub parts: Vec<TextPart>,
    pub local: bool,
    /// `{name::*}`
    pub list: bool,
}

/// Things a player, a world, a block, an item or a text has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prop {
    Name,
    DisplayName,
    TabName,
    Uuid,
    Location,
    World,
    Health,
    MaxHealth,
    Food,
    Saturation,
    Level,
    Gamemode,
    Tool,
    Ip,
    Ping,
    X,
    Y,
    Z,
    Yaw,
    Pitch,
    Type,
    Inventory,
    Flight,
    WalkSpeed,
    FlySpeed,
    Time,
    Weather,
    Length,
    Amount,
    Helmet,
    Chestplate,
    Leggings,
    Boots,
    FireTicks,
    /// Money, kept by Basics.
    Balance,
    /// `enchantments of player's tool`
    Enchants,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprOp {
    BlockAt,
    /// `block below` (-1) / `block above` (1): the offset is in the first argument.
    BlockRel,
    Distance,
    Now,
    Difference,
    TimeSince,
    Formatted,
    RandInt,
    RandNum,
    RandElem,
    Size,
    AllPlayers,
    PlayersIn,
    ParsedAs,
    Upper,
    Lower,
    Capital,
    Subtext,
    FirstChars,
    LastChars,
    Join,
    Split,
    Replace,
    LocationAt,
    ItemOf,
    SpanOf,
    Named,
    WithLore,
    TargetBlock,
    Uncolored,
    Tps,
    FirstElem,
    LastElem,
    IndexOf,
    WorldOf,
    /// Blocks within a radius of a place: radius, place.
    BlocksAround,
    /// Entities within a radius of a place, or all of them without arguments.
    Entities,
    /// The item with the enchantments named in the tag (`sharpness:5,unbreaking:3`).
    Enchanted,
    /// `a ? b`: the first value that is set.
    Default,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    Text(Vec<TextPart>),
    Var(VarName),
    /// A value of the event or command that started the trigger (`player`, `event-block`, `message`).
    Ev(String),
    /// Command argument by number (1-based), by type name, or all of them (0).
    Arg(usize, Option<String>),
    /// 0 value, 1 index, 2 number/iteration; `nth` 0 = innermost loop, n = n-th loop from outside.
    Loop(u8, usize),
    Prop(Box<Expr>, Prop),
    Bin(Box<Expr>, char, Box<Expr>),
    Neg(Box<Expr>),
    List(Vec<Expr>, bool),
    Call(String, Vec<Expr>),
    Op(ExprOp, Vec<Expr>, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    HasPerm,
    IsOp,
    Online,
    Sneaking,
    Flying,
    Sprinting,
    Alive,
    Burning,
    OnGround,
    Swimming,
    Banned,
    InWorld,
    HasItem,
    Contains,
    StartsWith,
    EndsWith,
    Matches,
    Chance,
    Empty,
    Cancelled,
    IsSet,
    Between,
    IsPlayer,
    /// `player can afford 50`, `player has 50 money`
    Afford,
}

#[derive(Debug, Clone)]
pub enum Cond {
    Cmp(Expr, CmpOp, Expr),
    Check(Check, Vec<Expr>, bool),
    All(Vec<Cond>),
    Any(Vec<Cond>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Set,
    Add,
    Remove,
    RemoveAll,
    Delete,
    Reset,
}

/// Effects the server does for us; the arguments are in the order the syntax reference lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fx {
    Teleport,
    Give,
    Take,
    Kill,
    Heal,
    Feed,
    Kick,
    Ban,
    Unban,
    Op,
    Deop,
    ConsoleCmd,
    PlayerCmd,
    Sound,
    Lightning,
    Explosion,
    Potion,
    Unpotion,
    Spawn,
    Push,
    Ignite,
    Extinguish,
    Log,
    Title,
    ClearInventory,
    /// Deposit (flag 0) or withdraw (flag 1) money: player, amount.
    Money,
    /// Chest menu: rows, title, player.
    OpenMenu,
    /// slot, player, item, command; flag 1 = the open menu, 16 = close on click, 32 = run the command on click.
    SetSlot,
    CloseMenu,
    /// items, place
    Drop,
    /// player, armour items
    Equip,
    /// player, projectile name
    Shoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendKind {
    Chat,
    ActionBar,
}

#[derive(Debug, Clone)]
pub enum Effect {
    Send(Expr, Option<Expr>, SendKind),
    Broadcast(Expr),
    Change(Expr, Mode, Option<Expr>),
    Host(Fx, Vec<Option<Expr>>, u32),
    Cancel(bool),
    CallFn(String, Vec<Expr>),
    ReplaceIn(Expr, Expr, Expr),
    CancelCooldown,
}

#[derive(Debug, Clone)]
pub enum LoopSrc {
    Items(Expr),
    Times(Expr),
    Range(Expr, Expr),
}

#[derive(Debug, Clone)]
pub enum Op {
    Effect(Effect),
    JumpIfNot(Cond, usize),
    Jump(usize),
    LoopInit(LoopSrc, usize),
    LoopNext(usize),
    PopLoops(usize),
    Wait(Expr),
    /// `wait until` (false) / `wait while` (true): checked again every tick.
    WaitFor(Cond, bool),
    Return(Option<Expr>),
    Stop,
}

/// One compiled trigger body.
#[derive(Debug)]
pub struct Code {
    pub script: String,
    pub ops: Vec<Op>,
    /// Source line of every op.
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgType {
    Text,
    Player,
    OfflinePlayer,
    Number,
    Integer,
    Boolean,
    Item,
    World,
    Timespan,
}

#[derive(Debug, Clone)]
pub struct ArgDef {
    pub name: Option<String>,
    pub ty: ArgType,
    pub optional: bool,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum ArgPart {
    Word(String),
    Arg(ArgDef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecBy {
    Both,
    Players,
    Console,
}

#[derive(Debug)]
pub struct CommandDef {
    pub script: String,
    pub line: u32,
    pub name: String,
    pub aliases: Vec<String>,
    pub parts: Vec<ArgPart>,
    pub description: String,
    pub usage: Option<Expr>,
    pub permission: Option<String>,
    pub permission_message: Option<Expr>,
    pub executable_by: ExecBy,
    pub cooldown: f64,
    pub cooldown_message: Option<Expr>,
    pub cooldown_bypass: Option<String>,
    pub code: Arc<Code>,
}

#[derive(Debug)]
pub struct FunctionDef {
    pub script: String,
    pub name: String,
    pub params: Vec<(String, Option<Expr>)>,
    pub code: Arc<Code>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Join,
    FirstJoin,
    Quit,
    Chat,
    Command,
    Death,
    Respawn,
    Break,
    Place,
    Click,
    Damage,
    Drop,
    Pickup,
    Consume,
    Sneak,
    Sprint,
    Flight,
    WorldChange,
    GamemodeChange,
    LevelChange,
    BedEnter,
    BedLeave,
    Teleport,
    Move,
    WeatherChange,
    InvClick,
    InvClose,
    ToolChange,
    Load,
    Unload,
    Every,
    At,
}

#[derive(Debug)]
pub struct Trigger {
    pub script: String,
    pub line: u32,
    pub kind: EventKind,
    /// Block or item ids the event must be about (empty = any).
    pub items: Vec<String>,
    /// Held item ids for click events.
    pub holding: Vec<String>,
    /// `on command "/x"`: the label; clicks: `left`/`right`; gamemode/weather: the new value.
    pub filter: Option<String>,
    /// `every`: seconds; `at`: time of day in ticks.
    pub number: f64,
    /// `in world`
    pub world: Option<String>,
    pub code: Arc<Code>,
}
