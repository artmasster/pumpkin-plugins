# MCSV Pumpkin plugins

Eighteen plugins for [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin), the Minecraft Java server written in Rust. Paper and Spigot `.jar` plugins do not run on Pumpkin, so each plugin here covers the job of a plugin that server owners already know, with the same commands where that makes sense. All of them are written from scratch in Rust for Pumpkin's plugin API; none contains code from the plugin it stands in for.

They are made by [MCSV](https://mcsv.me), a game server host, and run on its Pumpkin servers. Everything here also works on a Pumpkin server you run yourself.

**Status:** pre-1.0, like Pumpkin itself. Built and tested against Pumpkin `0.2.2+26.3-26.60` (Minecraft 26.3). Every release passes unit tests, a headless test bot and a console test on the real server binary. Testing with real game clients has been limited so far, so expect rough edges and please report them.

## Plugins

| Plugin | Stands in for | What it does | Main commands |
|---|---|---|---|
| `basics` | CMI / EssentialsX | Homes, warps, spawn, tpa, random teleport, ranks, chat, mail, kits, AFK, economy and selling, moderation (mute, tempban, warn, jail, freeze, vanish) | `/home` `/warp` `/spawn` `/tpa` `/rtp` `/kit` `/rank` `/balance` `/pay` · `/basics` lists all |
| `perms` | LuckPerms | Groups, inheritance, weight, tracks, prefix and suffix, temporary and per-world permissions, wildcards, with the `/lp` syntax | `/lp user …` `/lp group …` `/lp creategroup` `/lp editor` |
| `login` | AuthMe | Register and log in with a password on offline-mode servers; imports AuthMe hashes | `/register` `/login` `/changepassword` · admin `/auth` |
| `claims` | GriefPrevention | Claim land with a golden shovel, three trust levels, claim blocks | `/claim` `/trust` `/untrust` `/claimslist` `/abandonclaim` |
| `blocklog` | CoreProtect | Logs blocks, containers, chat and commands; inspector, lookup, rollback, restore, undo | `/co inspect` `/co lookup` `/co rollback u:name t:1h r:20` |
| `scripts` | Skript | A script language with Skript-style syntax: commands, events, variables, loops, functions, chest menus | `/sk reload` `/sk list` `/sk enable` `/sk disable` |
| `discordlink` | DiscordSRV | Game chat to a Discord channel and back, through a webhook or a bot token | `/discord` · admin `/discordlink status` |
| `tablist` | TAB | Animated header and footer, prefix and order by group in the tab list and above heads, sidebar | `/tablist reload` |
| `worldtools` | WorldEdit | Select with a wooden axe, set, replace, walls, cylinders, spheres, copy, paste, schematics, brushes, undo | `//wand` `//set` `//replace` `//copy` `//paste` `//undo` |
| `monitor` | spark | TPS history, time per tick, ping, entity counts, slow tick reports | `/tps` `/spark health` `/spark entities` |
| `cleaner` | ClearLag | Clears dropped items on a schedule with a warning, limits mobs per chunk | `/cleaner now` `/cleaner check` (`/lagg`) |
| `motd` | MiniMOTD / Maintenance | Rotating server list messages and a maintenance mode | `/maintenance on [30m]` `/maintenance off` |
| `worldmap` | BlueMap / squaremap | A top-down 2D map drawn as players explore, served as a live web map by the plugin itself, or exported as one HTML file | `/worldmap render` `/worldmap web <port>` `/worldmap export` |
| `resourcepack` | Resource Pack Downloader / ForcePack | Sends resource packs on join, several packs, per world, optional enforcement, computes the SHA-1 itself | `/rp` `/rp list` `/rp send` |
| `skins` | SkinsRestorer | Skins on offline-mode servers, from a Mojang account name or an image link | `/skin <account>` `/skin url <link>` `/skin clear` |
| `placeholders` | PlaceholderAPI | `%player_name%` style placeholders for the other plugins, including values they provide | `/papi parse` `/papi list` |
| `imagemap` | ImageOnMap | Turns an image link into a map or a poster of several maps | `/tomap <link> [2x2]` `/imagemap list` |
| `pregen` | Chunky | Pre-generates the world; pause, continue, and resume after a restart | `/pregen start` `/pregen pause` `/pregen continue` (`/chunky`) |

Names such as LuckPerms, WorldEdit or CoreProtect belong to their own projects. They are named here only to say which job each plugin does. These plugins are not affiliated with or endorsed by those projects.

## Install

Seventeen plugins are WASM components (`<id>.wasm`) and run in Pumpkin's sandbox. `pregen` is a native library (`libpregen.so`, Linux x86-64) because Pumpkin's WASM API cannot load or generate chunks.

1. Put the file in the server's `plugins/` folder.
2. A WASM plugin needs its sandbox permissions granted in `pumpkin.toml`, or the server asks on the console at every start:

   ```toml
   [plugins.overrides.basics]
   allowed_permissions = ["fs.read.data", "fs.write.data"]
   ```

   Every WASM plugin needs `fs.read.data` and `fs.write.data`. On top of that:

   | Plugins | Extra permissions | Why |
   |---|---|---|
   | `perms`, `discordlink`, `resourcepack`, `skins`, `imagemap` | `http.outbound`, `network.dns` | Web editor, Discord, pack hashes, Mojang skins, image downloads |
   | `worldmap` | `network.tcp.bind` | The built-in web map. Without it the plugin is not loaded at all |

3. Restart the server. The console prints `Loaded <id> (<version>)` for each plugin.

Each plugin keeps its settings and data in `plugins/data/<id>/` and writes a `config.toml` there on first start. Messages are Thai by default; set `language = "en"` in a plugin's `config.toml` for English.

The native `pregen` build links one exact Pumpkin release. Use the build made for the Pumpkin tag you run, or build it yourself after changing the tag in `Cargo.toml`. WASM plugins keep loading on newer Pumpkin releases.

## Things to know

- **Argument tab-completion is off.** On Pumpkin 0.2.2 a WASM plugin's command suggestions crash the server when the console is a terminal, so these plugins register none.
- **Join and quit messages are left alone**, and the server list keeps real player counts. Tools that read `X joined the game` or the ping response keep working.
- **Perms web editor.** `/lp editor` needs an editor service. A plain build has none and says so; the `/lp` commands do everything the editor does. Set `editor-url` in `plugins/data/perms/config.toml` to use a service of your own: the plugin sends `POST <url>/api/perms-editor` with the JSON session and expects `{"code": "…"}`, then `GET <url>/api/perms-editor/<code>` returns what was stored. MCSV runs such a service for the servers it hosts.
- **Plugins talk to each other** when they are installed together: Basics chat reaches DiscordLink and BlockLog, Placeholders reads balances from Basics and prefixes from Perms, WorldTools edits are logged by BlockLog, and every plugin leaves players alone until Login has let them in.

## Build

```bash
rustup target add wasm32-wasip2
cargo test --workspace --exclude pregen && cargo test -p pregen --no-default-features
cargo build --release --target wasm32-wasip2 -p basics            # target/wasm32-wasip2/release/basics.wasm
RUST_MIN_STACK=33554432 cargo build --release -p pregen           # target/release/libpregen.so
```

The Pumpkin release is pinned in the workspace `Cargo.toml`. For a newer Pumpkin, change the tag on every Pumpkin line there and rebuild.

| Path | What |
|---|---|
| `<id>/` | One crate per plugin (`scripts-plugin/` holds `scripts`) |
| `common/` | Shared code: coloured text, config and state files, HTTP without blocking the tick, messages between plugins |
| `tools/mcbot/` | A headless Minecraft 26.3 client for tests: joins, runs a script of commands and movements, prints what the server sent |
| `scripts/console-smoke.py` | Types commands into a Pumpkin console that is a real terminal, the way a hosting panel runs it |
| `scripts/publish.sh` | The release gate MCSV runs for its own hosting: tests, release build, load on the real server binary, console test |

## License

[MIT](LICENSE)
