#!/usr/bin/env python3
"""Write the example model of the Models plugin as a Blockbench project: a small creature with
a body, a head that looks around, legs, a tail whose cube is turned in a way a model file
cannot hold (so it becomes a part of its own), a hitbox, a place for a name, and the
animations the plugin starts by itself (idle, walk, hurt, death) plus one to play (attack).

    scripts/example-model.py                      writes models/src/example.bbmodel
    scripts/example-model.py <out.bbmodel> [legs] another file; more legs = more parts (load tests)

The picture is made here too, so the file needs nothing but this script.
"""
import base64
import json
import os
import struct
import sys
import zlib

# One 8x8 tile per body part, at (column, row) of a 32x32 picture
TILES = {(0, 0): (232, 134, 46), (1, 0): (242, 161, 74), (2, 0): (122, 72, 30), (0, 1): (176, 98, 36), (1, 1): (104, 62, 28)}


def png(width, height):
    rows = b""
    for y in range(height):
        rows += b"\x00"
        for x in range(width):
            colour = TILES.get((x // 8, y // 8))
            if colour is None:
                rows += bytes((0, 0, 0, 0))
                continue
            # A darker rim makes the cubes readable
            rim = x % 8 in (0, 7) or y % 8 in (0, 7)
            rows += bytes(int(c * (0.78 if rim else 1.0)) for c in colour) + b"\xff"

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


def faces(column, row):
    u, v = column * 8, row * 8
    return {side: {"uv": [u, v, u + 8, v + 8], "texture": 0} for side in ("north", "east", "south", "west", "up", "down")}


def key(channel, time, x, y, z, interpolation="linear"):
    return {"channel": channel, "time": time, "interpolation": interpolation, "data_points": [{"x": x, "y": y, "z": z}]}


def project(legs):
    elements = [
        {"name": "body", "uuid": "c-body", "from": [-4, 8, -6], "to": [4, 16, 6], "origin": [0, 8, 0], "faces": faces(0, 0)},
        {"name": "skull", "uuid": "c-head", "from": [-3, 14, -11], "to": [3, 20, -5], "origin": [0, 16, -6], "faces": faces(1, 0)},
        {"name": "ear", "uuid": "c-ear", "from": [-1, 20, -9], "to": [1, 23, -8], "origin": [0, 20, -8.5], "rotation": [22.5, 0, 0], "faces": faces(2, 0)},
        {"name": "tail", "uuid": "c-tail", "from": [-1, 12, 6], "to": [1, 14, 12], "origin": [0, 13, 6], "rotation": [30, 10, 0], "faces": faces(0, 1)},
        {"name": "hitbox", "uuid": "c-hit", "from": [-6, 0, -6], "to": [6, 22, 6], "origin": [0, 0, 0], "faces": {}},
    ]
    leg_groups = []
    walk = {}
    rows = max(1, (legs + 1) // 2)
    for i in range(legs):
        x = -3 if i % 2 == 0 else 3
        z = -5 + (i // 2) * (10 / (rows - 1)) if rows > 1 else 0
        elements.append({"name": f"leg{i}", "uuid": f"c-leg{i}", "from": [x - 1, 0, z - 1], "to": [x + 1, 8, z + 1], "origin": [x, 8, z], "faces": faces(1, 1)})
        leg_groups.append({"name": f"leg{i}", "uuid": f"g-leg{i}", "origin": [x, 8, z], "children": [f"c-leg{i}"]})
        # Opposite corners swing together
        swing = 30 if (i % 2 == 0) == ((i // 2) % 2 == 0) else -30
        walk[f"g-leg{i}"] = {"name": f"leg{i}", "type": "bone", "keyframes": [key("rotation", 0, swing, 0, 0), key("rotation", 0.5, -swing, 0, 0), key("rotation", 1.0, swing, 0, 0)]}
    outliner = [
        {"name": "root", "uuid": "g-root", "origin": [0, 0, 0], "children": [
            {"name": "body", "uuid": "g-body", "origin": [0, 8, 0], "children": [
                "c-body",
                {"name": "h_head", "uuid": "g-head", "origin": [0, 16, -5], "children": ["c-head", "c-ear"]},
                {"name": "tail", "uuid": "g-tail", "origin": [0, 13, 6], "children": ["c-tail"]},
                {"name": "tag_name", "uuid": "g-tag", "origin": [0, 26, 0], "children": []},
            ]},
            *leg_groups,
            {"name": "hitbox", "uuid": "g-hit", "origin": [0, 0, 0], "children": ["c-hit"]},
        ]}
    ]
    animations = [
        {"name": "idle", "loop": "loop", "length": 2.0, "animators": {
            "g-body": {"name": "body", "type": "bone", "keyframes": [key("position", 0, 0, "math.sin(query.anim_time * 180) * 0.5", 0)]},
            "g-tail": {"name": "tail", "type": "bone", "keyframes": [key("rotation", 0, 0, "math.sin(query.anim_time * 360) * 20", 0)]},
        }},
        {"name": "walk", "loop": "loop", "length": 1.0, "animators": walk},
        {"name": "attack", "loop": "once", "length": 0.5, "animators": {
            "g-head": {"name": "h_head", "type": "bone", "keyframes": [key("rotation", 0, 0, 0, 0), key("rotation", 0.25, 40, 0, 0, "catmullrom"), key("rotation", 0.5, 0, 0, 0)]},
        }},
        {"name": "hurt", "loop": "once", "length": 0.3, "animators": {
            "g-body": {"name": "body", "type": "bone", "keyframes": [key("scale", 0, 1, 1, 1), key("scale", 0.15, 1.2, 0.8, 1.2), key("scale", 0.3, 1, 1, 1)]},
        }},
        {"name": "death", "loop": "hold", "length": 1.0, "animators": {
            "g-root": {"name": "root", "type": "bone", "keyframes": [key("rotation", 0, 0, 0, 0), key("rotation", 1.0, 0, 0, 90)]},
        }},
    ]
    return {
        "meta": {"format_version": "4.10", "model_format": "free", "box_uv": False},
        "name": "example",
        "resolution": {"width": 32, "height": 32},
        "elements": elements,
        "outliner": outliner,
        "textures": [{"name": "skin.png", "uuid": "t-skin", "uv_width": 32, "uv_height": 32, "source": "data:image/png;base64," + base64.b64encode(png(32, 32)).decode()}],
        "animations": animations,
    }


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "models", "src", "example.bbmodel")
    legs = int(sys.argv[2]) if len(sys.argv) > 2 else 4
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(project(legs), fh, separators=(",", ":"))
        fh.write("\n")


main()
