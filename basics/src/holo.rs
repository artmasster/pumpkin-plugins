//! Holograms: floating text made of one text display entity each. Pumpkin saves the entity with
//! its chunk, so a hologram stays after a restart; Basics keeps the entity's UUID to change or
//! remove it later (`holograms.json`).
use mcsv_common as c;
use pumpkin_plugin_api::command::CommandSender;
use pumpkin_plugin_api::world::{Entity, World};
use pumpkin_plugin_api::{BillboardMode, EntityDisplayExt, EntityType, Server};

use crate::lang::fmt;
use crate::plugin::{State, key, loc_of, world_of};
use crate::store::{self, Holo};

const FILE: &str = "holograms.json";
const MAX: usize = 200;

pub(crate) fn load() -> store::Holos {
    c::load_json(FILE).unwrap_or_default()
}

fn save(st: &State) {
    c::save_json(FILE, &st.holos);
}

fn entity_of(world: &World, uuid: &str) -> Option<Entity> {
    world.get_entities().into_iter().find(|e| key(&e.get_uuid()) == uuid)
}

fn show(e: &Entity, text: &str) -> bool {
    let Some(td) = e.as_text_display() else { return false };
    td.set_text(c::color(&store::holo_text(text)));
    td.get_display().set_billboard(BillboardMode::Center);
    true
}

/// Entities of deleted holograms whose chunk was not loaded then: remove them when they show up.
pub(crate) fn tick(st: &mut State, server: &Server) {
    if st.holos.remove.is_empty() {
        return;
    }
    let before = st.holos.remove.len();
    let pending = std::mem::take(&mut st.holos.remove);
    for (dim, uuid) in pending {
        match world_of(server, &dim).and_then(|w| entity_of(&w, &uuid)) {
            Some(e) => e.remove(),
            None => st.holos.remove.push((dim, uuid)),
        }
    }
    if st.holos.remove.len() != before {
        save(st);
    }
}

pub(crate) fn run(st: &mut State, sender: &CommandSender, server: &Server, w: &[String]) -> Vec<String> {
    let lang = st.cfg.language.clone();
    let l = lang.as_str();
    let say = |k: &str, v: &[(&str, String)]| vec![fmt(l, k, v)];
    let arg = |i: usize| w.get(i).map(String::as_str);
    let rest = |i: usize| w.get(i..).map(|s| s.join(" ")).unwrap_or_default();
    let usage = || say("usage", &[("arg", "/holo <create|text|movehere|delete|list> <name> [text]".into())]);
    let name = arg(1).map(str::to_lowercase).unwrap_or_default();
    let text: String = rest(2).chars().filter(|ch| !ch.is_control()).take(256).collect();
    match arg(0).map(str::to_lowercase).as_deref() {
        Some("list") => {
            if st.holos.holos.is_empty() {
                return say("holo_none", &[]);
            }
            let list = st.holos.holos.iter().map(|(n, h)| format!("{n} ({} {:.0} {:.0} {:.0})", store::dim_name(&h.dim), h.x, h.y, h.z)).collect::<Vec<_>>().join(", ");
            say("holo_list", &[("list", list)])
        }
        Some("create") => {
            let Some(p) = c::sender_player(sender) else { return say("player_only", &[]) };
            if !store::valid_name(&name) || text.trim().is_empty() {
                return usage();
            }
            if st.holos.holos.contains_key(&name) {
                return say("holo_exists", &[("name", name)]);
            }
            if st.holos.holos.len() >= MAX {
                return say("holo_full", &[("max", MAX.to_string())]);
            }
            let at = loc_of(&p);
            let e = p.get_world().spawn_entity(EntityType::TextDisplay, (at.x, at.y + 1.6, at.z));
            if !show(&e, &text) {
                e.remove();
                return say("holo_failed", &[]);
            }
            st.holos.holos.insert(name.clone(), Holo { uuid: key(&e.get_uuid()), dim: at.dim, x: at.x, y: at.y + 1.6, z: at.z, text });
            save(st);
            say("holo_made", &[("name", name)])
        }
        Some(op @ ("text" | "movehere")) => {
            let Some(h) = st.holos.holos.get(&name).cloned() else { return say("holo_unknown", &[("name", name)]) };
            let Some(e) = world_of(server, &h.dim).and_then(|wd| entity_of(&wd, &h.uuid)) else { return say("holo_far", &[("name", name)]) };
            if op == "text" {
                if text.trim().is_empty() {
                    return usage();
                }
                show(&e, &text);
                if let Some(x) = st.holos.holos.get_mut(&name) {
                    x.text = text;
                }
            } else {
                let Some(p) = c::sender_player(sender) else { return say("player_only", &[]) };
                let at = loc_of(&p);
                // An entity cannot be carried to another world: make it again there
                e.remove();
                let fresh = p.get_world().spawn_entity(EntityType::TextDisplay, (at.x, at.y + 1.6, at.z));
                show(&fresh, &h.text);
                if let Some(x) = st.holos.holos.get_mut(&name) {
                    (x.uuid, x.dim, x.x, x.y, x.z) = (key(&fresh.get_uuid()), at.dim, at.x, at.y + 1.6, at.z);
                }
            }
            save(st);
            say("holo_changed", &[("name", name)])
        }
        Some("delete" | "remove") => {
            let Some(h) = st.holos.holos.remove(&name) else { return say("holo_unknown", &[("name", name)]) };
            match world_of(server, &h.dim).and_then(|wd| entity_of(&wd, &h.uuid)) {
                Some(e) => e.remove(),
                // Its chunk is not loaded: taken away the next time someone walks there
                None => st.holos.remove.push((h.dim, h.uuid)),
            }
            save(st);
            say("holo_deleted", &[("name", name)])
        }
        _ => usage(),
    }
}
