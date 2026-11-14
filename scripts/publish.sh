#!/usr/bin/env bash
# Build MCSV's own Pumpkin plugins for the Pumpkin tag pinned in Cargo.toml, check that the matching
# Pumpkin release loads them, then publish them to /opt/mcsv/pumpkin-plugins where the API installs
# them from (services::pumpkin_plugins reads manifest.json).
#
# Native plugins (.so) must be built for the exact Pumpkin tag (plugin-API version check), so every
# Pumpkin tag MCSV sells needs one run of this script. WASM plugins keep loading on newer Pumpkin, but
# they are rebuilt and tested with the same tag so one manifest entry covers both.
# This is the release gate MCSV runs for its own hosting; README.md covers a plain build.
#
# Usage: scripts/publish.sh          (heavy steps are CPU-capped: the build host is shared)
# New Pumpkin tag: change the tag on every Pumpkin line of Cargo.toml first, fix the build, then run.
# New plugin: add one line to WASM_PLUGINS below (and the catalog entry in the API).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST_ROOT="${PUMPKIN_PLUGINS_DIST:-/opt/mcsv/pumpkin-plugins}"
# Perms: the web editor this build uses when config.toml names none. A plain `cargo build` has
# none (the editor is off); MCSV's builds point at its own service.
export PERMS_EDITOR_URL="${PERMS_EDITOR_URL-https://perms.mcsv.me}"
# The image Pumpkin servers run on (version_fetcher::fetch_pumpkin), without a JVM
IMAGE="ghcr.io/parkervcp/yolks:debian"

# WASM plugins: id | name its `version` command prints | sandbox permissions | console commands (`;` between).
# The console commands are typed into a TTY console during the test: every plugin must answer its own
# commands, with arguments, without taking the server down. The first one must print "<Name> <version>".
WASM_PLUGINS=(
  "basics|Basics|fs.read.data fs.write.data|basics version;rank list;setrank Nobody vip;warp;tempban Nobody 1h test;warn Nobody test;lag;sun;shop;trash;repair all Nobody;sudo Nobody spawn;holo list"
  "tablist|TabList|fs.read.data fs.write.data|tablist version;tablist reload"
  "worldtools|WorldTools|fs.read.data fs.write.data|worldtools version;worldtools help;worldtools schem list;worldtools set oak_stairs[facing=north]"
  "login|Login|fs.read.data fs.write.data|auth version;auth status;auth info Nobody;authme status;login x"
  "monitor|Monitor|fs.read.data fs.write.data|spark version;tps;spark health;spark ping Nobody;spark entities;monitor tps"
  "cleaner|Cleaner|fs.read.data fs.write.data|cleaner version;cleaner check;lagg clear"
  "discordlink|DiscordLink|fs.read.data fs.write.data http.outbound network.dns|discordlink version;discordlink status;discord"
  "claims|Claims|fs.read.data fs.write.data|claims version;claimslist;adjustclaimblocks Nobody 5"
  "motd|Motd|fs.read.data fs.write.data|maintenance version;maintenance status;maintenance add Nobody;maintenance remove Nobody"
  "scripts|Scripts|fs.read.data fs.write.data|sk version;sk list;sk info;sk reload all;scripts list"
  "perms|Perms|fs.read.data fs.write.data http.outbound network.dns|lp info;lp listgroups;lp creategroup smoketest 5;lp group smoketest permission set basics:command.fly true;lp user Nobody info;lp group smoketest meta settempprefix 5 X 1m;lp log recent;lp deletegroup smoketest;perms listgroups;lp editor;lp applyedits Ab3dEf7hJk;lp applyedits x"
  "worldmap|WorldMap|fs.read.data fs.write.data network.tcp.bind|worldmap version;worldmap status;worldmap render 2;worldmap export overworld;worldmap pause;worldmap resume;worldmap web;worldmap web 25599;worldmap web off"
  "placeholders|Placeholders|fs.read.data fs.write.data|papi version;papi list;papi parse --null %server_online%/%server_max_players% %custom_discord% 100%;papi parse Nobody x;papi reload"
  "skins|Skins|fs.read.data fs.write.data http.outbound network.dns|skin version;skin info Nobody;skin set bad!name Nobody;skin clear Nobody;skin reload;skin"
  "resourcepack|ResourcePack|fs.read.data fs.write.data http.outbound network.dns|rp version;rp list;rp status Nobody;rp send Nobody;rp hash;rp reload"
  "imagemap|ImageMap|fs.read.data fs.write.data http.outbound network.dns|imagemap version;imagemap list all;imagemap new http://127.0.0.1/x.png;imagemap file nope.png;imagemap delete nope;tomap"
  "blocklog|BlockLog|fs.read.data fs.write.data|co version;co status;co lookup u:Nobody t:1h r:#global;coreprotect near;co purge t:1h;blocklog status"
)

cd "$ROOT"
TAG="$(sed -nE 's/^pumpkin-plugin-api = .*tag = "([^"]+)".*/\1/p' Cargo.toml)"
[[ "$TAG" =~ ^[0-9]+\.[0-9]+\.[0-9]+\+[0-9.]+-[0-9.]+$ ]] || { echo "unexpected Pumpkin tag: '$TAG'" >&2; exit 2; }
PINS="$(grep -c 'github.com/Pumpkin-MC/Pumpkin' Cargo.toml)"
[ "$(grep -c "tag = \"$TAG\"" Cargo.toml)" = "$PINS" ] || { echo "Cargo.toml pins more than one Pumpkin tag" >&2; exit 2; }
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml)"
echo "MCSV Pumpkin plugins $VERSION for Pumpkin $TAG (${#WASM_PLUGINS[@]} WASM + pregen)"

IDS=()
PACKAGES=()
for line in "${WASM_PLUGINS[@]}"; do
  id="${line%%|*}"
  IDS+=("$id")
  PACKAGES+=(-p "$id")
done

cap() { systemd-run --scope -q -p CPUQuota=800% nice -n 19 "$@"; }
cap cargo test -q -p pregen --no-default-features
cap cargo test -q "${PACKAGES[@]}"
cap cargo build --release -q --target wasm32-wasip2 "${PACKAGES[@]}"
RUST_MIN_STACK=33554432 cap cargo build --release -q -p pregen
SO="$ROOT/target/release/libpregen.so"
test -s "$SO"
for id in "${IDS[@]}"; do
  test -s "$ROOT/target/wasm32-wasip2/release/$id.wasm" || { echo "$id.wasm was not built" >&2; exit 1; }
done

# The Pumpkin release this tag ships, checked against its own checksums
WORK="/var/tmp/mcsv-pumpkin-plugins-smoke/$TAG"
if [ -d "$WORK" ]; then
  # the container writes as another uid
  docker run --rm --network none -v "$WORK:/w" --entrypoint /bin/sh "$IMAGE" -c 'rm -rf /w/* /w/.[!.]*' </dev/null >/dev/null 2>&1 || true
  rm -rf "$WORK"
fi
mkdir -p "$WORK/plugins"
ENC_TAG="${TAG//+/%2B}"
curl -fsSL -o "$WORK/pumpkin" "https://github.com/Pumpkin-MC/Pumpkin/releases/download/${ENC_TAG}/pumpkin-X64-Linux"
curl -fsSL "https://github.com/Pumpkin-MC/Pumpkin/releases/download/${ENC_TAG}/checksums.sha256" \
  | awk -v f="$WORK/pumpkin" '$2 ~ /pumpkin-X64-Linux$/ {print $1"  "f}' | sha256sum -c -
chmod 755 "$WORK/pumpkin"

# The plugin must not need a newer glibc than Pumpkin itself (the server image only has to run Pumpkin)
glibc_max() { objdump -T "$1" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1; }
NEED="$(glibc_max "$SO")"
HAVE="$(glibc_max "$WORK/pumpkin")"
[ "$(printf '%s\n%s\n' "$NEED" "$HAVE" | sort -V | tail -1)" = "$HAVE" ] \
  || { echo "libpregen.so needs glibc $NEED, Pumpkin only $HAVE" >&2; exit 1; }

# Smoke test on the server image: every plugin loads and gets exactly the permissions the panel
# will approve for it, without a prompt in the console
cp "$SO" "$WORK/plugins/"
: > "$WORK/pumpkin.toml"
CONSOLE=()
for line in "${WASM_PLUGINS[@]}"; do
  IFS='|' read -r id _name perms commands <<< "$line"
  cp "$ROOT/target/wasm32-wasip2/release/$id.wasm" "$WORK/plugins/"
  quoted="$(printf '"%s", ' $perms)"
  printf '[plugins.overrides.%s]\nallowed_permissions = [%s]\n' "$id" "${quoted%, }" >> "$WORK/pumpkin.toml"
  IFS=';' read -ra list <<< "$commands"
  CONSOLE+=("${list[@]}")
done
CONSOLE+=("pregen version" "chunky version" "pregen progress")
chmod -R a+rwX "$WORK"
docker run --rm --network none --cpus 2 --memory 1g -v "$WORK:/home/container" \
  -e STARTUP='timeout -s INT 40 ./pumpkin' "$IMAGE" > "$WORK/smoke.log" 2>&1 || true
sed -i 's/\x1b\[[0-9;]*m//g' "$WORK/smoke.log"
for p in pregen "${IDS[@]}"; do
  if ! grep -q "Loaded $p ($VERSION)" "$WORK/smoke.log"; then
    grep -i -E 'error|mismatch|panic|permission' "$WORK/smoke.log" | head -20 >&2
    echo "smoke test: $p did not load, nothing published (log: $WORK/smoke.log)" >&2
    exit 1
  fi
done
echo "smoke test passed (${#IDS[@]} WASM + pregen loaded)"

# Console test with a TTY, like on a node: rustyline asks the command tree for suggestions on every
# key, a path the piped run above never takes. Commands with arguments from every plugin must answer
# and the server must still be up for our `stop` (Basics 0.2.0, then named Nexus, crashed Pumpkin 0.2.2 here).
chmod -R a+rwX "$WORK"
if ! SMOKE_LIMIT=$(( ${#CONSOLE[@]} * 3 + 90 )) python3 -I "$ROOT/scripts/console-smoke.py" "$WORK" "${CONSOLE[@]}" > "$WORK/console.log" 2>&1; then
  tail -30 "$WORK/console.log" >&2
  echo "console test failed, nothing published (log: $WORK/console.log)" >&2
  exit 1
fi
for line in "${WASM_PLUGINS[@]}" "pregen|Pregen||"; do
  IFS='|' read -r _id name _rest <<< "$line"
  if ! grep -q "$name $VERSION" "$WORK/console.log"; then
    echo "console test: no \"$name $VERSION\" answer, nothing published (log: $WORK/console.log)" >&2
    exit 1
  fi
done
echo "console test passed (${#CONSOLE[@]} commands)"

# Publish: one directory per Pumpkin tag, then the manifest entry the API reads
DEST="$DEST_ROOT/$TAG"
mkdir -p "$DEST"
FILES=("pregen=libpregen.so")
install -m 644 "$SO" "$DEST/libpregen.so.new" && mv -f "$DEST/libpregen.so.new" "$DEST/libpregen.so"
for id in "${IDS[@]}"; do
  install -m 644 "$ROOT/target/wasm32-wasip2/release/$id.wasm" "$DEST/$id.wasm.new" && mv -f "$DEST/$id.wasm.new" "$DEST/$id.wasm"
  FILES+=("$id=$id.wasm")
done
python3 -I - "$DEST_ROOT/manifest.json" "$TAG" "$VERSION" "$DEST" "${FILES[@]}" <<'PY'
import hashlib, json, os, sys, time
path, tag, version, dest, *files = sys.argv[1:]
data = json.load(open(path)) if os.path.exists(path) else {}
entry = {"built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
for pair in files:
    plugin, name = pair.split("=", 1)
    raw = open(os.path.join(dest, name), "rb").read()
    entry[plugin] = {
        "file": f"{tag}/{name}",
        "size": len(raw),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "version": version,
    }
data.setdefault("builds", {})[tag] = entry
tmp = path + ".new"
json.dump(data, open(tmp, "w"), indent=2, sort_keys=True)
os.replace(tmp, path)
print(json.dumps({k: (v["size"] if isinstance(v, dict) else v) for k, v in entry.items()}, indent=2))
PY
chmod 644 "$DEST_ROOT/manifest.json"
echo "published $DEST"
