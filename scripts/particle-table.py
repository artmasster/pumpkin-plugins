#!/usr/bin/env python3
"""Write common/src/particles.rs: the particle of a name like `flame` or `minecraft:flame`.
The names come from the WIT file of the Pumpkin tag pinned in Cargo.toml, so run this again
after changing the tag.

    scripts/particle-table.py <path to pumpkin-plugin-wit/v0.1/particles.wit>
"""
import os
import re
import sys


def main():
    wit = open(sys.argv[1], encoding="utf-8").read()
    body = wit[wit.index("enum particle {") :]
    body = body[body.index("{") + 1 : body.index("}")]
    names = [n.strip().rstrip(",") for n in body.splitlines() if re.fullmatch(r"\s*[a-z0-9-]+,?\s*", n)]
    out = [
        "//! Names → `Particle` (written by `scripts/particle-table.py` from the WIT file of the pinned Pumpkin tag).",
        "use pumpkin_plugin_api::wit::pumpkin::plugin::particles::Particle;",
        "",
        "/// `flame`, `minecraft:flame`, `FLAME`.",
        "pub fn particle(id: &str) -> Option<Particle> {",
        '    let lower = id.trim().to_ascii_lowercase();',
        '    Some(match lower.strip_prefix("minecraft:").unwrap_or(&lower) {',
    ]
    for name in names:
        variant = "".join(part.capitalize() for part in name.split("-"))
        out.append(f'        "{name.replace("-", "_")}" => Particle::{variant},')
    out += ["        _ => return None,", "    })", "}", ""]
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "..", "common", "src", "particles.rs"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))
    print(len(names), "particles")


main()
