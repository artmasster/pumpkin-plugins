//! mcbot: minimal offline-mode Java client for lab tests of Pumpkin plugins.
//!
//! The lab server needs `online_mode = false` and `encryption = false` in
//! `[networking.java]`. The bot logs in, answers keep-alives and teleports, and
//! runs a script; every chat/system/action-bar line it receives is printed as
//! `CHAT <text>`, every position as `POS x y z`, so a test can grep the output.
//!
//! mcbot <host:port> <name> "<script>"
//! script steps separated by `;`:
//!   wait <seconds>     cmd <command without />     chat <message>
//!   move <dx> <dy> <dz> (relative walk, one packet)  quit
//!   dig <x> <y> <z> [seconds]   place <x> <y> <z> [face]   drop   slot <0-8>
//!   (block coordinates accept `~` and `~N`, relative to the block the bot stands in)
//!   click <slot> [button] [mode]  (in the window the server opened)   closewin
//!   useitem [yaw] [pitch]  (right click into the air with the held item; pitch 90 = down)
//!   interact <entity id | type=N>   (right click an entity; ids come from the `ENTITY <id>
//!   type=<n>` lines, `type=N` means the newest entity of that type)   attack <same>
//! `MCBOT_FIND=a,b` prints `FOUND <text> in <packet>` for every play packet that carries one of
//! these strings: enough to see an item model or a font reach the client without reading items.
//! `MCBOT_META=1` prints what entities are told: `META <entity id> <index>:<value> ...` for
//! entity data (the value types of display entities are read; another type ends the line as
//! `<index>:type<n>..`), `PASSENGERS <vehicle> [ids]`, `LINK <leashed> <holder>`, `REMOVED [ids]`. It also counts every
//! play packet: the step `counts` prints `COUNTS <packet>=<n> ...` and `MOVES type<n>=<count>
//! ...` (movement packets by the type of the entity that moved) since the last `counts`.
//! Also printed: `BLOCK x y z <state id>` for single block updates, `WINDOW id type title`,
//! `CONTENT window state slots` when a window's items arrive, `SLOT window state slot count=n`
//! for one slot (5 is the helmet in the player's own window 0), `WINDOWCLOSED`.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use pumpkin_data::packet::{clientbound as cb, serverbound as sb};

const PROTOCOL: i32 = 777; // 26.3, matches the pinned Pumpkin tag

// ---- wire -----------------------------------------------------------------

fn put_varint(buf: &mut Vec<u8>, mut v: i32) {
    loop {
        let mut b = (v as u32 & 0x7f) as u8;
        v = ((v as u32) >> 7) as i32;
        if v != 0 {
            b |= 0x80;
        }
        buf.push(b);
        if v == 0 {
            break;
        }
    }
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn varint(&mut self) -> Option<i32> {
        let mut out = 0u32;
        for i in 0..5 {
            let (&b, rest) = self.0.split_first()?;
            self.0 = rest;
            out |= u32::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(out as i32);
            }
        }
        None
    }
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let n = self.varint()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Collect the visible text of a network-NBT text component (strings of `text`, `translate`, list items).
fn nbt_text(data: &[u8]) -> String {
    fn walk(r: &mut Rd, tag: u8, key: &str, out: &mut Vec<String>) -> Option<()> {
        match tag {
            1 => drop(r.take(1)?),
            2 => drop(r.take(2)?),
            3 | 5 => drop(r.take(4)?),
            4 | 6 => drop(r.take(8)?),
            7 => {
                let n = r.i32()? as usize;
                r.take(n)?;
            }
            8 => {
                let n = u16::from_be_bytes(r.take(2)?.try_into().ok()?) as usize;
                let s = String::from_utf8_lossy(r.take(n)?).into_owned();
                if matches!(key, "text" | "" | "translate") {
                    out.push(s);
                }
            }
            9 => {
                let t = r.take(1)?[0];
                let n = r.i32()?;
                for _ in 0..n.max(0) {
                    walk(r, t, "", out)?;
                }
            }
            10 => loop {
                let t = r.take(1)?[0];
                if t == 0 {
                    break;
                }
                let n = u16::from_be_bytes(r.take(2)?.try_into().ok()?) as usize;
                let k = String::from_utf8_lossy(r.take(n)?).into_owned();
                walk(r, t, &k, out)?;
            },
            11 => {
                let n = r.i32()? as usize;
                r.take(n * 4)?;
            }
            12 => {
                let n = r.i32()? as usize;
                r.take(n * 8)?;
            }
            _ => return None,
        }
        Some(())
    }
    let mut r = Rd(data);
    let mut out = Vec::new();
    if let Some(&t) = r.0.first() {
        r.0 = &r.0[1..];
        let _ = walk(&mut r, t, "", &mut out);
    }
    out.join("")
}

struct Conn {
    stream: TcpStream,
    threshold: Option<usize>,
}

impl Conn {
    fn send(&mut self, id: i32, body: &[u8]) -> std::io::Result<()> {
        let mut data = Vec::with_capacity(body.len() + 5);
        put_varint(&mut data, id);
        data.extend_from_slice(body);
        let mut frame = Vec::new();
        match self.threshold {
            Some(t) if data.len() >= t => {
                let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
                z.write_all(&data)?;
                let comp = z.finish()?;
                let mut inner = Vec::new();
                put_varint(&mut inner, data.len() as i32);
                inner.extend_from_slice(&comp);
                put_varint(&mut frame, inner.len() as i32);
                frame.extend_from_slice(&inner);
            }
            Some(_) => {
                put_varint(&mut frame, data.len() as i32 + 1);
                frame.push(0);
                frame.extend_from_slice(&data);
            }
            None => {
                put_varint(&mut frame, data.len() as i32);
                frame.extend_from_slice(&data);
            }
        }
        self.stream.write_all(&frame)
    }

    fn recv(&mut self) -> std::io::Result<(i32, Vec<u8>)> {
        let mut len = 0u32;
        for i in 0..5 {
            let mut b = [0u8];
            self.stream.read_exact(&mut b)?;
            len |= u32::from(b[0] & 0x7f) << (7 * i);
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        let mut frame = vec![0u8; len as usize];
        self.stream.read_exact(&mut frame)?;
        let data = if self.threshold.is_some() {
            let mut r = Rd(&frame);
            let raw = r.varint().unwrap_or(0);
            if raw == 0 {
                r.0.to_vec()
            } else {
                let mut out = Vec::with_capacity(raw as usize);
                ZlibDecoder::new(r.0).read_to_end(&mut out)?;
                out
            }
        } else {
            frame
        };
        let mut r = Rd(&data);
        let id = r.varint().unwrap_or(-1);
        Ok((id, r.0.to_vec()))
    }
}

// ---- client ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct Pos {
    x: f64,
    y: f64,
    z: f64,
}

fn play_move(conn: &Arc<Mutex<Conn>>, p: Pos) {
    let mut b = Vec::new();
    for v in [p.x, p.y, p.z] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&0f32.to_be_bytes());
    b.extend_from_slice(&0f32.to_be_bytes());
    b.push(0x01); // on ground
    let _ = conn.lock().unwrap().send(sb::play::MOVE_PLAYER_POS_ROT.0, &b);
}

fn pack_pos((x, y, z): (i32, i32, i32)) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38) | ((i64::from(z) & 0x3FF_FFFF) << 12) | (i64::from(y) & 0xFFF)
}

fn unpack_pos(v: i64) -> (i32, i32, i32) {
    ((v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32)
}

/// Block coordinates of a step; `~` and `~N` are relative to the block the bot stands in.
fn block_arg(v: &[&str], cur: Option<Pos>) -> Option<(i32, i32, i32)> {
    let base = cur.map_or((0, 0, 0), |p| (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32));
    let one = |s: &str, b: i32| -> Option<i32> {
        match s.strip_prefix('~') {
            Some("") => Some(b),
            Some(d) => Some(b + d.parse::<i32>().ok()?),
            None => s.parse().ok(),
        }
    };
    Some((one(v.first()?, base.0)?, one(v.get(1)?, base.1)?, one(v.get(2)?, base.2)?))
}

/// Player action body: status, block, face (top), sequence.
fn action(status: i32, at: (i32, i32, i32), seq: i32) -> Vec<u8> {
    let mut b = Vec::new();
    put_varint(&mut b, status);
    b.extend_from_slice(&pack_pos(at).to_be_bytes());
    b.push(1);
    put_varint(&mut b, seq);
    b
}

/// The play packets a test looks for by name; the rest are shown by number.
/// The entries of an entity data packet as text: `<index>:<value>`. Reads the value types a
/// display entity uses (byte, int, float, bool, vector, quaternion) and stops at any other,
/// whose length it cannot know: `<index>:type<n>..`.
fn entity_data(r: &mut Rd) -> String {
    let f32s = |r: &mut Rd, n: usize| -> Option<String> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(format!("{:.3}", f32::from_be_bytes(r.take(4)?.try_into().ok()?)));
        }
        Some(out.join(","))
    };
    let mut out = Vec::new();
    loop {
        let Some(&[index]) = r.take(1) else { break };
        if index == 0xff {
            break;
        }
        let Some(kind) = r.varint() else { break };
        let value = match kind {
            0 => r.take(1).map(|b| b[0].to_string()),
            1 => r.varint().map(|v| v.to_string()),
            3 => f32s(r, 1),
            8 => r.take(1).map(|b| (b[0] != 0).to_string()),
            39 => f32s(r, 3).map(|v| format!("({v})")),
            40 => f32s(r, 4).map(|v| format!("[{v}]")),
            _ => None,
        };
        match value {
            Some(v) => out.push(format!("{index}:{v}")),
            None => {
                out.push(format!("{index}:type{kind}.."));
                break;
            }
        }
    }
    out.join(" ")
}

fn packet_name(id: i32) -> String {
    let known = [
        (cb::play::CONTAINER_SET_SLOT.0, "set_slot"),
        (cb::play::CONTAINER_SET_CONTENT.0, "set_content"),
        (cb::play::SET_PLAYER_INVENTORY.0, "set_player_inventory"),
        (cb::play::SET_ENTITY_DATA.0, "entity_data"),
        (cb::play::ADD_ENTITY.0, "add_entity"),
        (cb::play::REMOVE_ENTITIES.0, "remove_entities"),
        (cb::play::SET_PASSENGERS.0, "set_passengers"),
        (cb::play::MOVE_ENTITY_POS.0, "move_pos"),
        (cb::play::MOVE_ENTITY_POS_ROT.0, "move_pos_rot"),
        (cb::play::MOVE_ENTITY_ROT.0, "move_rot"),
        (cb::play::ROTATE_HEAD.0, "rotate_head"),
        (cb::play::ENTITY_POSITION_SYNC.0, "position_sync"),
        (cb::play::TELEPORT_ENTITY.0, "teleport_entity"),
        (cb::play::OPEN_SCREEN.0, "open_screen"),
        (cb::play::SOUND.0, "sound"),
        (cb::play::SYSTEM_CHAT.0, "chat"),
    ];
    known.iter().find(|(n, _)| *n == id).map_or_else(|| format!("packet_{id}"), |(_, name)| (*name).to_owned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: mcbot <host:port> <name> \"<script>\"");
        std::process::exit(2);
    }
    let (addr, name, script) = (&args[1], &args[2], &args[3]);
    let stream = TcpStream::connect(addr).expect("connect");
    stream.set_nodelay(true).ok();
    let mut conn = Conn { stream, threshold: None };

    // Handshake + login start
    let (host, port) = addr.rsplit_once(':').unwrap_or((addr, "25565"));
    let mut b = Vec::new();
    put_varint(&mut b, PROTOCOL);
    put_str(&mut b, host);
    b.extend_from_slice(&port.parse::<u16>().unwrap_or(25565).to_be_bytes());
    put_varint(&mut b, 2);
    conn.send(sb::handshake::INTENTION.0, &b).expect("handshake");
    let mut b = Vec::new();
    put_str(&mut b, name);
    b.extend_from_slice(&[0u8; 16]);
    conn.send(sb::login::HELLO.0, &b).expect("login start");

    // Login
    loop {
        let (id, body) = conn.recv().expect("login read");
        let mut r = Rd(&body);
        if id == cb::login::LOGIN_COMPRESSION.0 {
            conn.threshold = r.varint().map(|t| t.max(0) as usize);
        } else if id == cb::login::LOGIN_FINISHED.0 {
            conn.send(sb::login::LOGIN_ACKNOWLEDGED.0, &[]).expect("login ack");
            println!("LOGIN ok");
            break;
        } else if id == cb::login::LOGIN_DISCONNECT.0 {
            println!("KICK {}", r.string().unwrap_or_default());
            std::process::exit(1);
        } else if id == 1 {
            println!("KICK encryption requested: set encryption = false in pumpkin.toml");
            std::process::exit(1);
        }
    }

    // Configuration
    let mut info = Vec::new();
    put_str(&mut info, "th_th");
    info.push(8); // view distance
    put_varint(&mut info, 0);
    info.push(1);
    info.push(0x7f);
    put_varint(&mut info, 1);
    info.push(0);
    info.push(1);
    put_varint(&mut info, 0);
    conn.send(sb::config::CLIENT_INFORMATION.0, &info).ok();
    loop {
        let (id, body) = conn.recv().expect("config read");
        let mut r = Rd(&body);
        if id == cb::config::SELECT_KNOWN_PACKS.0 {
            conn.send(sb::config::SELECT_KNOWN_PACKS.0, &body).ok();
        } else if id == cb::config::KEEP_ALIVE.0 {
            conn.send(sb::config::KEEP_ALIVE.0, &body).ok();
        } else if id == cb::config::PING.0 {
            conn.send(sb::config::PONG.0, &body).ok();
        } else if id == cb::config::CODE_OF_CONDUCT.0 {
            conn.send(sb::config::ACCEPT_CODE_OF_CONDUCT.0, &[]).ok();
        } else if id == cb::config::RESOURCE_PACK_PUSH.0 {
            // uuid + url ...: answer "accepted" then "successfully loaded"
            let uuid = r.take(16).map(<[u8]>::to_vec).unwrap_or_default();
            for status in [3, 0] {
                let mut b = uuid.clone();
                put_varint(&mut b, status);
                conn.send(sb::config::RESOURCE_PACK.0, &b).ok();
            }
            println!("PACK {}", r.string().unwrap_or_default());
        } else if id == cb::config::FINISH_CONFIGURATION.0 {
            conn.send(sb::config::FINISH_CONFIGURATION.0, &[]).ok();
            println!("CONFIG ok");
            break;
        } else if id == cb::config::DISCONNECT.0 {
            println!("KICK {}", nbt_text(&body));
            std::process::exit(1);
        }
    }

    // Play: reader thread answers the server, main thread runs the script
    let conn = Arc::new(Mutex::new(conn));
    let reader = conn.lock().unwrap().stream.try_clone().expect("clone");
    let threshold = conn.lock().unwrap().threshold;
    let pos = Arc::new(Mutex::new(None::<Pos>));
    // The window that is open (0 = the player's own inventory) and its latest state id
    let window = Arc::new(Mutex::new((0i32, 0i32)));
    // The newest entity of each type the server showed us (type number -> entity id)
    let seen = Arc::new(Mutex::new(std::collections::HashMap::<i32, i32>::new()));
    // MCBOT_META=1: print what entities are told (`META`, `PASSENGERS`, `REMOVED`) and count
    // every play packet for the `counts` step
    let meta = std::env::var("MCBOT_META").is_ok_and(|v| !v.is_empty() && v != "0");
    let counts = Arc::new(Mutex::new(std::collections::BTreeMap::<i32, u64>::new()));
    let moves = Arc::new(Mutex::new(std::collections::BTreeMap::<i32, u64>::new()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    {
        let conn = conn.clone();
        let pos = pos.clone();
        let window = window.clone();
        let seen = seen.clone();
        let counts = counts.clone();
        let moves = moves.clone();
        let mut kinds = std::collections::HashMap::<i32, i32>::new();
        let find: Vec<String> = std::env::var("MCBOT_FIND").unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        std::thread::spawn(move || {
            let mut rconn = Conn { stream: reader, threshold };
            let mut loaded = false;
            loop {
                let Ok((id, body)) = rconn.recv() else {
                    println!("CLOSED");
                    let _ = done_tx.send(());
                    return;
                };
                let mut r = Rd(&body);
                // MCBOT_FIND=a,b: say which packets carry these strings (an item's model, a font)
                for mark in &find {
                    if body.windows(mark.len()).any(|w| w == mark.as_bytes()) {
                        println!("FOUND {mark} in {} bytes={}", packet_name(id), body.len());
                    }
                }
                if meta {
                    *counts.lock().unwrap().entry(id).or_insert(0) += 1;
                }
                if id == cb::play::ADD_ENTITY.0 {
                    // entity id, uuid, type
                    let eid = r.varint().unwrap_or(-1);
                    let _uuid = r.take(16);
                    let kind = r.varint().unwrap_or(-1);
                    seen.lock().unwrap().insert(kind, eid);
                    kinds.insert(eid, kind);
                    println!("ENTITY {eid} type={kind}");
                } else if meta
                    && [cb::play::MOVE_ENTITY_POS.0, cb::play::MOVE_ENTITY_POS_ROT.0, cb::play::MOVE_ENTITY_ROT.0, cb::play::ROTATE_HEAD.0, cb::play::ENTITY_POSITION_SYNC.0, cb::play::TELEPORT_ENTITY.0].contains(&id)
                {
                    // Movement by entity type: how much the server says about entities that only ride
                    let kind = r.varint().and_then(|eid| kinds.get(&eid).copied()).unwrap_or(-1);
                    *moves.lock().unwrap().entry(kind).or_insert(0) += 1;
                } else if meta && id == cb::play::SET_ENTITY_DATA.0 {
                    let eid = r.varint().unwrap_or(-1);
                    println!("META {eid} {}", entity_data(&mut r));
                } else if meta && id == cb::play::SET_PASSENGERS.0 {
                    let vehicle = r.varint().unwrap_or(-1);
                    let n = r.varint().unwrap_or(0);
                    let ids: Vec<String> = (0..n).filter_map(|_| r.varint()).map(|v| v.to_string()).collect();
                    println!("PASSENGERS {vehicle} [{}]", ids.join(","));
                } else if meta && id == cb::play::SET_ENTITY_LINK.0 {
                    let (attached, holder) = (r.i32().unwrap_or(-1), r.i32().unwrap_or(-1));
                    println!("LINK {attached} {holder}");
                } else if meta && id == cb::play::REMOVE_ENTITIES.0 {
                    let n = r.varint().unwrap_or(0);
                    let ids: Vec<String> = (0..n).filter_map(|_| r.varint()).map(|v| v.to_string()).collect();
                    println!("REMOVED [{}]", ids.join(","));
                } else if id == cb::play::KEEP_ALIVE.0 {
                    let _ = conn.lock().unwrap().send(sb::play::KEEP_ALIVE.0, &body);
                } else if id == cb::play::PING.0 {
                    let _ = conn.lock().unwrap().send(sb::play::PONG.0, &body);
                } else if id == cb::play::PLAYER_POSITION.0 {
                    let tid = r.varint().unwrap_or(0);
                    let (x, y, z) = (r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0));
                    let _delta = (r.f64(), r.f64(), r.f64());
                    let yaw = r.take(4).map_or(0.0, |b| f32::from_be_bytes(b.try_into().unwrap_or([0; 4])));
                    let pitch = r.take(4).map_or(0.0, |b| f32::from_be_bytes(b.try_into().unwrap_or([0; 4])));
                    let flags = r.i32().unwrap_or(0);
                    let old = pos.lock().unwrap().unwrap_or(Pos { x: 0.0, y: 0.0, z: 0.0 });
                    let p = Pos {
                        x: if flags & 0x01 != 0 { old.x + x } else { x },
                        y: if flags & 0x02 != 0 { old.y + y } else { y },
                        z: if flags & 0x04 != 0 { old.z + z } else { z },
                    };
                    // Since 26.3 the client echoes where it accepted the teleport
                    let mut b = Vec::new();
                    put_varint(&mut b, tid);
                    for v in [p.x, p.y, p.z] {
                        b.extend_from_slice(&v.to_be_bytes());
                    }
                    b.extend_from_slice(&yaw.to_be_bytes());
                    b.extend_from_slice(&pitch.to_be_bytes());
                    let _ = conn.lock().unwrap().send(sb::play::ACCEPT_TELEPORTATION.0, &b);
                    play_move(&conn, p);
                    *pos.lock().unwrap() = Some(p);
                    println!("POS {:.1} {:.1} {:.1}", p.x, p.y, p.z);
                    if !loaded {
                        loaded = true;
                        let _ = conn.lock().unwrap().send(sb::play::PLAYER_LOADED.0, &[]);
                    }
                } else if id == cb::play::CHUNK_BATCH_FINISHED.0 {
                    let _ = conn.lock().unwrap().send(sb::play::CHUNK_BATCH_RECEIVED.0, &64f32.to_be_bytes());
                } else if id == cb::play::SYSTEM_CHAT.0 {
                    let overlay = body.last() == Some(&1);
                    let t = nbt_text(&body[..body.len().saturating_sub(1)]);
                    println!("{} {t}", if overlay { "BAR" } else { "CHAT" });
                } else if id == cb::play::SET_ACTION_BAR_TEXT.0 {
                    println!("BAR {}", nbt_text(&body));
                } else if id == cb::play::TAB_LIST.0 {
                    println!("TAB {}", nbt_text(&body));
                } else if id == cb::play::SET_PLAYER_TEAM.0 || id == cb::play::SET_SCORE.0 || id == cb::play::SET_OBJECTIVE.0 {
                    // All three start with a string (team, score holder, objective); the rest is shown as text
                    let n = usize::from(body.first().copied().unwrap_or(0)).min(body.len().saturating_sub(1));
                    let first = String::from_utf8_lossy(&body[1..1 + n]).into_owned();
                    let kind = if id == cb::play::SET_PLAYER_TEAM.0 { "TEAM" } else if id == cb::play::SET_SCORE.0 { "SCORE" } else { "OBJ" };
                    println!("{kind} {first} | {}", nbt_text(&body[1 + n..]));
                } else if id == cb::play::BLOCK_UPDATE.0 {
                    if let Some(raw) = r.take(8) {
                        let (x, y, z) = unpack_pos(i64::from_be_bytes(raw.try_into().unwrap_or([0; 8])));
                        println!("BLOCK {x} {y} {z} {}", r.varint().unwrap_or(-1));
                    }
                } else if id == cb::play::OPEN_SCREEN.0 {
                    let (wid, kind) = (r.varint().unwrap_or(0), r.varint().unwrap_or(0));
                    *window.lock().unwrap() = (wid, 0);
                    println!("WINDOW {wid} {kind} {}", nbt_text(r.0));
                } else if id == cb::play::CONTAINER_SET_CONTENT.0 {
                    let (wid, state, slots) = (r.varint().unwrap_or(0), r.varint().unwrap_or(0), r.varint().unwrap_or(0));
                    let mut w = window.lock().unwrap();
                    if w.0 == wid {
                        w.1 = state;
                    }
                    println!("CONTENT {wid} {state} {slots}");
                } else if id == cb::play::CONTAINER_SET_SLOT.0 {
                    let (wid, state) = (r.varint().unwrap_or(0), r.varint().unwrap_or(0));
                    let slot = r.take(2).map_or(-1, |b| i16::from_be_bytes([b[0], b[1]]));
                    let count = r.varint().unwrap_or(0);
                    println!("SLOT {wid} {state} {slot} count={count}");
                } else if id == cb::play::CONTAINER_CLOSE.0 {
                    *window.lock().unwrap() = (0, 0);
                    println!("WINDOWCLOSED");
                } else if id == cb::play::SET_TITLE_TEXT.0 {
                    println!("TITLE {}", nbt_text(&body));
                } else if id == cb::play::DISGUISED_CHAT.0 {
                    println!("CHAT {}", nbt_text(&body));
                } else if id == cb::play::DISCONNECT.0 {
                    println!("KICK {}", nbt_text(&body));
                    let _ = done_tx.send(());
                    return;
                } else if id == cb::play::START_CONFIGURATION.0 {
                    let _ = conn.lock().unwrap().send(sb::play::CONFIGURATION_ACKNOWLEDGED.0, &[]);
                    println!("RECONFIGURE (not supported)");
                } else if id == cb::play::RESOURCE_PACK_PUSH.0 {
                    // uuid, url, hash, forced, optional prompt. MCBOT_PACK says how the "player" answers:
                    // accept (default): accepted then loaded · decline · fail: accepted then download failed · ignore
                    let uuid = r.take(16).map(<[u8]>::to_vec).unwrap_or_default();
                    let (url, hash) = (r.string().unwrap_or_default(), r.string().unwrap_or_default());
                    let forced = r.take(1).is_some_and(|b| b[0] != 0);
                    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
                    let how = std::env::var("MCBOT_PACK").unwrap_or_else(|_| "accept".into());
                    println!("PACKPUSH {hex} {url} hash={hash} forced={forced} answer={how}");
                    let statuses: &[i32] = match how.as_str() {
                        "decline" => &[1],
                        "fail" => &[3, 2],
                        "ignore" => &[],
                        _ => &[3, 0],
                    };
                    for status in statuses {
                        let mut b = uuid.clone();
                        put_varint(&mut b, *status);
                        let _ = conn.lock().unwrap().send(sb::play::RESOURCE_PACK.0, &b);
                    }
                } else if id == cb::play::RESOURCE_PACK_POP.0 {
                    let all = r.take(1).is_some_and(|b| b[0] == 0);
                    let hex: String = r.take(16).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect();
                    println!("PACKPOP {}", if all { "all".to_owned() } else { hex });
                } else if id == cb::play::PLAYER_INFO_UPDATE.0 {
                    // "Add player" entries: who, and whether a skin (the textures property) came with them.
                    // Entries are read as long as their other fields are the plain ones (no chat session, no display name).
                    let actions = r.take(1).map_or(0, |b| b[0]);
                    let count = r.varint().unwrap_or(0);
                    if actions & 1 != 0 {
                        for _ in 0..count.min(64) {
                            let _uuid = r.take(16);
                            let name = r.string().unwrap_or_default();
                            let props = r.varint().unwrap_or(0);
                            let mut skin = String::from("none");
                            for _ in 0..props.min(4) {
                                let (key, value) = (r.string().unwrap_or_default(), r.string().unwrap_or_default());
                                let signed = r.take(1).is_some_and(|b| b[0] != 0);
                                if signed {
                                    let _ = r.string();
                                }
                                if key == "textures" {
                                    // The value is base64 JSON; its length and tail tell two skins apart
                                    skin = format!("{}:{}:signed={signed}", value.len(), &value[value.len().saturating_sub(12)..]);
                                }
                            }
                            println!("INFO {name} skin={skin}");
                            if actions & 0x22 != 0 {
                                break;
                            }
                            if actions & 0x04 != 0 {
                                let _ = r.varint();
                            }
                            if actions & 0x08 != 0 {
                                let _ = r.take(1);
                            }
                            if actions & 0x10 != 0 {
                                let _ = r.varint();
                            }
                            if actions & 0x40 != 0 {
                                let _ = r.varint();
                            }
                            if actions & 0x80 != 0 {
                                let _ = r.take(1);
                            }
                        }
                    }
                } else if id == cb::play::PLAYER_INFO_REMOVE.0 {
                    println!("INFOREMOVE {}", r.varint().unwrap_or(0));
                } else if id == cb::play::RESPAWN.0 {
                    println!("RESPAWN");
                } else if id == cb::play::MAP_ITEM_DATA.0 {
                    // map id, scale, locked, optional icons, then the patch: columns, rows, x, z, data
                    let map = r.varint().unwrap_or(-1);
                    let _scale_locked = r.take(2);
                    let icons = if r.take(1).is_some_and(|b| b[0] != 0) { r.varint().unwrap_or(0) } else { 0 };
                    if icons > 0 {
                        println!("MAP {map} icons={icons} (patch not read)");
                    } else {
                        let cols = r.take(1).map_or(0, |b| b[0]);
                        let (rows, x, z) = if cols > 0 { (r.take(1).map_or(0, |b| b[0]), r.take(1).map_or(0, |b| b[0]), r.take(1).map_or(0, |b| b[0])) } else { (0, 0, 0) };
                        let len = if cols > 0 { r.varint().unwrap_or(0) } else { 0 };
                        let data = r.take(len.max(0) as usize).unwrap_or_default();
                        // A cheap fingerprint of the picture: the sum of its colour bytes and how many are not empty
                        let sum: u64 = data.iter().map(|b| u64::from(*b)).sum();
                        let filled = data.iter().filter(|b| **b >= 4).count();
                        println!("MAP {map} {cols}x{rows}+{x}+{z} bytes={len} filled={filled} sum={sum}");
                    }
                }
            }
        });
    }

    let start = Instant::now();
    while pos.lock().unwrap().is_none() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("SPAWNED");
    let mut seq = 0i32;
    for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (verb, rest) = step.split_once(' ').unwrap_or((step, ""));
        match verb {
            "wait" => std::thread::sleep(Duration::from_secs_f64(rest.parse().unwrap_or(1.0))),
            "cmd" => {
                let mut b = Vec::new();
                put_str(&mut b, rest);
                let _ = conn.lock().unwrap().send(sb::play::CHAT_COMMAND.0, &b);
                println!("SENT /{rest}");
            }
            "chat" => {
                let mut b = Vec::new();
                put_str(&mut b, rest);
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                b.extend_from_slice(&now.to_be_bytes());
                b.extend_from_slice(&0i64.to_be_bytes());
                b.push(0); // no signature
                put_varint(&mut b, 0);
                b.extend_from_slice(&[0, 0, 0]);
                b.push(0); // checksum
                let _ = conn.lock().unwrap().send(sb::play::CHAT.0, &b);
                println!("SENT {rest}");
            }
            "move" => {
                let d: Vec<f64> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                let cur = *pos.lock().unwrap();
                if let (Some(p), [dx, dy, dz]) = (cur, d.as_slice()) {
                    let np = Pos { x: p.x + dx, y: p.y + dy, z: p.z + dz };
                    play_move(&conn, np);
                    *pos.lock().unwrap() = Some(np);
                }
            }
            "dig" => {
                // dig <x> <y> <z> [seconds]: start, wait, finish (creative breaks on start)
                let v: Vec<&str> = rest.split_whitespace().collect();
                if let Some(at) = block_arg(&v, *pos.lock().unwrap()) {
                    let hold = v.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.4);
                    seq += 1;
                    let _ = conn.lock().unwrap().send(sb::play::PLAYER_ACTION.0, &action(0, at, seq));
                    let _ = conn.lock().unwrap().send(sb::play::SWING.0, &[0]);
                    std::thread::sleep(Duration::from_secs_f64(hold));
                    seq += 1;
                    // 26.3 numbering: 3 = finished digging
                    let _ = conn.lock().unwrap().send(sb::play::PLAYER_ACTION.0, &action(3, at, seq));
                    println!("DUG {} {} {}", at.0, at.1, at.2);
                }
            }
            "place" | "use" => {
                // place <x> <y> <z> [face 0-5]: use the held item on that block (face 1 = top)
                let v: Vec<&str> = rest.split_whitespace().collect();
                if let Some(at) = block_arg(&v, *pos.lock().unwrap()) {
                    let face: i32 = v.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
                    seq += 1;
                    let mut b = Vec::new();
                    put_varint(&mut b, 0);
                    b.extend_from_slice(&pack_pos(at).to_be_bytes());
                    put_varint(&mut b, face);
                    for c in [0.5f32, 1.0, 0.5] {
                        b.extend_from_slice(&c.to_be_bytes());
                    }
                    b.push(0);
                    b.push(0);
                    put_varint(&mut b, seq);
                    let _ = conn.lock().unwrap().send(sb::play::USE_ITEM_ON.0, &b);
                    let _ = conn.lock().unwrap().send(sb::play::SWING.0, &[0]);
                    println!("USED {} {} {}", at.0, at.1, at.2);
                }
            }
            "drop" => {
                // 26.3 numbering: 5 = drop one item of the held stack
                seq += 1;
                let _ = conn.lock().unwrap().send(sb::play::PLAYER_ACTION.0, &action(5, (0, 0, 0), seq));
            }
            "slot" => {
                let n: i16 = rest.trim().parse().unwrap_or(0);
                let _ = conn.lock().unwrap().send(sb::play::SET_CARRIED_ITEM.0, &n.to_be_bytes());
            }
            "useitem" => {
                // useitem [yaw] [pitch]: right click with the held item into the air (buckets, food);
                // pitch 90 looks straight down
                let v: Vec<f32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                seq += 1;
                let mut b = Vec::new();
                put_varint(&mut b, 0);
                put_varint(&mut b, seq);
                b.extend_from_slice(&v.first().copied().unwrap_or(0.0).to_be_bytes());
                b.extend_from_slice(&v.get(1).copied().unwrap_or(90.0).to_be_bytes());
                let _ = conn.lock().unwrap().send(sb::play::USE_ITEM.0, &b);
                println!("USEDITEM");
            }
            "click" => {
                // click <slot> [button 0=left 1=right] [mode 0=pickup 1=shift]: in the open window
                let v: Vec<i32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if let Some(slot) = v.first() {
                    let (wid, state) = *window.lock().unwrap();
                    let mut b = Vec::new();
                    put_varint(&mut b, wid);
                    put_varint(&mut b, state);
                    b.extend_from_slice(&(*slot as i16).to_be_bytes());
                    b.push(v.get(1).copied().unwrap_or(0) as u8);
                    put_varint(&mut b, v.get(2).copied().unwrap_or(0));
                    put_varint(&mut b, 0); // no predicted slot changes
                    b.push(0); // nothing predicted on the cursor
                    let _ = conn.lock().unwrap().send(sb::play::CONTAINER_CLICK.0, &b);
                    println!("CLICKED {wid} {slot}");
                }
            }
            "interact" => {
                // interact <entity id | type=N>: right click an entity (26.1+ layout: id, hand, where,
                // sneaking). `type=N` picks the newest entity of that type the server showed us.
                let target = match rest.trim().strip_prefix("type=") {
                    Some(kind) => kind.parse::<i32>().ok().and_then(|k| seen.lock().unwrap().get(&k).copied()),
                    None => rest.trim().parse::<i32>().ok(),
                };
                if let Some(eid) = target {
                    let mut b = Vec::new();
                    put_varint(&mut b, eid);
                    put_varint(&mut b, 0);
                    b.push(0); // a zero vector in the short form
                    b.push(0);
                    let _ = conn.lock().unwrap().send(sb::play::INTERACT.0, &b);
                    let _ = conn.lock().unwrap().send(sb::play::SWING.0, &[0]);
                    println!("INTERACTED {eid}");
                }
            }
            "attack" => {
                // attack <entity id | type=N>: left click an entity, then swing as the game does
                let target = match rest.trim().strip_prefix("type=") {
                    Some(kind) => kind.parse::<i32>().ok().and_then(|k| seen.lock().unwrap().get(&k).copied()),
                    None => rest.trim().parse::<i32>().ok(),
                };
                if let Some(eid) = target {
                    let mut b = Vec::new();
                    put_varint(&mut b, eid);
                    let _ = conn.lock().unwrap().send(sb::play::ATTACK.0, &b);
                    let _ = conn.lock().unwrap().send(sb::play::SWING.0, &[0]);
                    println!("ATTACKED {eid}");
                }
            }
            "closewin" => {
                let (wid, _) = *window.lock().unwrap();
                let mut b = Vec::new();
                put_varint(&mut b, wid);
                let _ = conn.lock().unwrap().send(sb::play::CONTAINER_CLOSE.0, &b);
                *window.lock().unwrap() = (0, 0);
            }
            "counts" => {
                // Packets since the last `counts` step, by name (needs MCBOT_META=1)
                let mut map = counts.lock().unwrap();
                let line: Vec<String> = map.iter().map(|(id, n)| format!("{}={n}", packet_name(*id))).collect();
                println!("COUNTS {}", line.join(" "));
                map.clear();
                let mut by_kind = moves.lock().unwrap();
                let line: Vec<String> = by_kind.iter().map(|(kind, n)| format!("type{kind}={n}")).collect();
                println!("MOVES {}", line.join(" "));
                by_kind.clear();
            }
            "quit" => break,
            other => println!("BADSTEP {other}"),
        }
        if done_rx.try_recv().is_ok() {
            break;
        }
    }
    let _ = conn.lock().unwrap().stream.shutdown(std::net::Shutdown::Both);
}
