//! What players do, in one shape for the plugins that pay or count it (Jobs, Quests, Skills).
//! Each plugin listens to the events itself (handlers are per plugin) and turns them into a
//! [`Deed`] with these helpers.
use std::collections::HashMap;

use pumpkin_plugin_api::{EntityType, Server};

/// One thing a player did. Names are the game's, without `minecraft:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deed {
    Break(String),
    Place(String),
    Kill(String),
    Fish,
    Craft(String),
    Smelt(String, u32),
    Eat(String),
    Breed(String),
    Tame(String),
}

impl Deed {
    /// `break`, `place`, … as the owners' files name them.
    pub fn kind(&self) -> &'static str {
        match self {
            Deed::Break(_) => "break",
            Deed::Place(_) => "place",
            Deed::Kill(_) => "kill",
            Deed::Fish => "fish",
            Deed::Craft(_) => "craft",
            Deed::Smelt(..) => "smelt",
            Deed::Eat(_) => "eat",
            Deed::Breed(_) => "breed",
            Deed::Tame(_) => "tame",
        }
    }

    /// What it was done to (`stone`, `zombie`); fishing has none (`*`).
    pub fn target(&self) -> &str {
        match self {
            Deed::Break(t) | Deed::Place(t) | Deed::Kill(t) | Deed::Craft(t) | Deed::Smelt(t, _) | Deed::Eat(t) | Deed::Breed(t) | Deed::Tame(t) => t,
            Deed::Fish => "*",
        }
    }

    /// How many at once.
    pub fn count(&self) -> u32 {
        match self {
            Deed::Smelt(_, n) => (*n).max(1),
            _ => 1,
        }
    }
}

pub use crate::names::{bare, matches};

/// `CaveSpider` → `cave_spider`: the game's name of a creature.
pub fn entity_name(t: EntityType) -> String {
    let full = format!("{t:?}");
    let debug = full.rsplit("::").next().unwrap_or(&full);
    let mut out = String::with_capacity(debug.len() + 4);
    for (i, ch) in debug.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Who last hit which creature, so a death can be put down to a player. A hit counts for
/// `KEEP` ticks. Only hits by a player's own hand count (the game tells plugins the arrow,
/// not the archer, as the damager).
#[derive(Default)]
pub struct Kills {
    last: HashMap<i32, (String, String, u64)>,
}

impl Kills {
    pub const KEEP: u64 = 200;

    /// From `EntityDamageByEntityEvent`: when the damager is a player, note the hit.
    pub fn hit(&mut self, server: &Server, victim: i32, damager: i32, tick: u64) {
        let Some(p) = server.get_all_players().into_iter().find(|p| p.as_entity().get_id() as i32 == damager) else { return };
        let Some(kind) = p.as_entity().get_nearby_entities(8.0, 8.0, 8.0).into_iter().find(|e| e.get_id() as i32 == victim).map(|e| entity_name(e.get_type())) else { return };
        if self.last.len() > 4096 {
            self.last.retain(|_, (_, _, t)| tick < *t + Self::KEEP);
        }
        self.last.insert(victim, (crate::game::key(&p), kind, tick));
    }

    /// From `EntityDeathEvent`: the player who killed it and what it was.
    pub fn died(&mut self, victim: i32, tick: u64) -> Option<(String, String)> {
        let (player, kind, at) = self.last.remove(&victim)?;
        (tick < at + Self::KEEP).then_some((player, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeds_say_what_and_how_many() {
        assert_eq!(Deed::Smelt("iron_ingot".into(), 5).count(), 5);
        assert_eq!(Deed::Fish.target(), "*");
        assert_eq!(Deed::Kill("zombie".into()).kind(), "kill");
        assert_eq!(bare("minecraft:Stone"), "stone");
        assert_eq!(entity_name(EntityType::CaveSpider), "cave_spider");
        assert_eq!(entity_name(EntityType::Zombie), "zombie");
    }
}

// ---- listening ------------------------------------------------------------------------

use pumpkin_plugin_api::events::{BlockBreakEvent, BlockPlaceEvent, CraftItemEvent, EntityDamageByEntityEvent, EntityDeathEvent, EntityTameEvent, EventHandler, EventPriority, FromIntoEvent, FurnaceExtractEvent, PlayerFishEvent, PlayerItemConsumeEvent};
use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::wit::pumpkin::plugin::event::PlayerFishState;
use pumpkin_plugin_api::Context;

/// A block: world, x, y, z.
pub type Spot = (String, i32, i32, i32);

/// Where a plugin takes its players' deeds.
pub trait Sink: Copy + Send + Sync + 'static {
    /// A deed by an online player; `spot` is the block of a break or a place. Events that
    /// another plugin cancelled before are not told.
    fn deed(&self, server: &Server, p: &Player, deed: Deed, spot: Option<Spot>);
    /// Run `f` on the plugin's kill notes and its tick count (skip it when the state is busy).
    fn with_kills(&self, f: &mut dyn FnMut(&mut Kills, u64));
}

#[derive(Clone, Copy)]
struct Hook<S>(S);

type Data<E> = <E as FromIntoEvent>::Data;

impl<S: Sink> EventHandler<BlockBreakEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<BlockBreakEvent>) -> Data<BlockBreakEvent> {
        if let (false, Some(p)) = (e.cancelled, &e.player) {
            let spot = (p.get_world().get_dimension(), e.block_pos.x, e.block_pos.y, e.block_pos.z);
            self.0.deed(&server, p, Deed::Break(bare(&e.block)), Some(spot));
        }
        e
    }
}

impl<S: Sink> EventHandler<BlockPlaceEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<BlockPlaceEvent>) -> Data<BlockPlaceEvent> {
        if !e.cancelled {
            let spot = (e.player.get_world().get_dimension(), e.block_pos.x, e.block_pos.y, e.block_pos.z);
            self.0.deed(&server, &e.player, Deed::Place(bare(&e.block_placed)), Some(spot));
        }
        e
    }
}

impl<S: Sink> EventHandler<EntityDamageByEntityEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<EntityDamageByEntityEvent>) -> Data<EntityDamageByEntityEvent> {
        if !e.cancelled {
            self.0.with_kills(&mut |kills, tick| kills.hit(&server, e.entity_id, e.damager_id, tick));
        }
        e
    }
}

impl<S: Sink> EventHandler<EntityDeathEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<EntityDeathEvent>) -> Data<EntityDeathEvent> {
        let mut killed = None;
        self.0.with_kills(&mut |kills, tick| killed = kills.died(e.entity_id, tick));
        if let Some((player, kind)) = killed
            && let Some(p) = crate::game::online(&server, &player)
        {
            self.0.deed(&server, &p, Deed::Kill(kind), None);
        }
        e
    }
}

impl<S: Sink> EventHandler<PlayerFishEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<PlayerFishEvent>) -> Data<PlayerFishEvent> {
        if !e.cancelled && matches!(e.state, PlayerFishState::CaughtFish) {
            self.0.deed(&server, &e.player, Deed::Fish, None);
        }
        e
    }
}

impl<S: Sink> EventHandler<CraftItemEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<CraftItemEvent>) -> Data<CraftItemEvent> {
        if !e.cancelled {
            self.0.deed(&server, &e.player, Deed::Craft(bare(&e.recipe_id)), None);
        }
        e
    }
}

impl<S: Sink> EventHandler<FurnaceExtractEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<FurnaceExtractEvent>) -> Data<FurnaceExtractEvent> {
        self.0.deed(&server, &e.player, Deed::Smelt(bare(&e.item_id), e.item_amount), None);
        e
    }
}

impl<S: Sink> EventHandler<PlayerItemConsumeEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<PlayerItemConsumeEvent>) -> Data<PlayerItemConsumeEvent> {
        if !e.cancelled {
            self.0.deed(&server, &e.player, Deed::Eat(bare(&e.item_name)), None);
        }
        e
    }
}

impl<S: Sink> EventHandler<EntityTameEvent> for Hook<S> {
    fn handle(&self, server: Server, e: Data<EntityTameEvent>) -> Data<EntityTameEvent> {
        if !e.cancelled
            && let Some(kind) = e.owner.as_entity().get_nearby_entities(8.0, 8.0, 8.0).into_iter().find(|x| x.get_id() as i32 == e.entity_id).map(|x| entity_name(x.get_type()))
        {
            self.0.deed(&server, &e.owner, Deed::Tame(kind), None);
        }
        e
    }
}

/// Listen for every deed and tell `sink`. Call once in `on_load`.
pub fn listen<S: Sink>(context: &Context, sink: S) -> Result<(), String> {
    let p = EventPriority::Lowest;
    context.register_event_handler::<BlockBreakEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<BlockPlaceEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<EntityDamageByEntityEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<EntityDeathEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<PlayerFishEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<CraftItemEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<FurnaceExtractEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<PlayerItemConsumeEvent, _>(Hook(sink), p, false)?;
    context.register_event_handler::<EntityTameEvent, _>(Hook(sink), p, false)?;
    Ok(())
}
