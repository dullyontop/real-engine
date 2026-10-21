#!/usr/bin/env python3
"""Writes models/walker.glb: the valley's walking character.

A low-poly figure of boxes, each box skinned wholly to one joint of an
eleven-joint skeleton (skin `Walker`), with two looping clips: `idle` (2 s)
and `walk` (one stride pair per second at about 1.6 m/s). It faces +Z, stands
on y = 0 and is 1.8 m tall. Colours are vertex colours, so one white material
draws it.

Standard library only. Run from the project directory:

    python3 tools/make_walker.py

The output is byte-for-byte the same on every run, so the committed file can be
checked with `git diff --exit-code` after running it.
"""

import json
import math
import struct
import sys
from pathlib import Path

# name, parent, rest translation relative to the parent
JOINTS = [
    ("Hips", None, (0.0, 0.95, 0.0)),
    ("Spine", "Hips", (0.0, 0.10, 0.0)),
    ("Head", "Spine", (0.0, 0.50, 0.0)),
    ("UpperLeg.L", "Hips", (0.10, -0.05, 0.0)),
    ("LowerLeg.L", "UpperLeg.L", (0.0, -0.42, 0.0)),
    ("UpperLeg.R", "Hips", (-0.10, -0.05, 0.0)),
    ("LowerLeg.R", "UpperLeg.R", (0.0, -0.42, 0.0)),
    ("UpperArm.L", "Spine", (0.27, 0.40, 0.0)),
    ("LowerArm.L", "UpperArm.L", (0.0, -0.32, 0.0)),
    ("UpperArm.R", "Spine", (-0.27, 0.40, 0.0)),
    ("LowerArm.R", "UpperArm.R", (0.0, -0.32, 0.0)),
]
INDEX = {name: i for i, (name, _, _) in enumerate(JOINTS)}

SKIN = (0.93, 0.74, 0.60, 1.0)
SHIRT = (0.85, 0.33, 0.18, 1.0)
TROUSERS = (0.20, 0.28, 0.45, 1.0)
BOOTS = (0.18, 0.13, 0.10, 1.0)

# joint, centre (model space), size, colour
BOXES = [
    ("Hips", (0.0, 0.93, 0.0), (0.34, 0.18, 0.21), TROUSERS),
    ("Spine", (0.0, 1.29, 0.0), (0.40, 0.52, 0.23), SHIRT),
    ("Head", (0.0, 1.67, 0.01), (0.22, 0.25, 0.24), SKIN),
    ("Head", (0.0, 1.63, 0.13), (0.05, 0.06, 0.04), SKIN),  # nose, so the front shows
]
for side, x in (("L", 1.0), ("R", -1.0)):
    BOXES += [
        (f"UpperLeg.{side}", (0.10 * x, 0.69, 0.0), (0.15, 0.44, 0.15), TROUSERS),
        (f"LowerLeg.{side}", (0.10 * x, 0.28, 0.0), (0.13, 0.42, 0.13), TROUSERS),
        (f"LowerLeg.{side}", (0.10 * x, 0.04, 0.04), (0.13, 0.08, 0.25), BOOTS),
        (f"UpperArm.{side}", (0.27 * x, 1.34, 0.0), (0.11, 0.34, 0.11), SHIRT),
        (f"LowerArm.{side}", (0.27 * x, 1.02, 0.0), (0.09, 0.30, 0.09), SKIN),
    ]


def global_rest():
    out = []
    for _, parent, t in JOINTS:
        if parent is None:
            out.append(t)
        else:
            p = out[INDEX[parent]]
            out.append((p[0] + t[0], p[1] + t[1], p[2] + t[2]))
    return out


def box(centre, size):
    """24 vertices (4 per face, flat normals) and 36 indices, outward winding."""
    cx, cy, cz = centre
    hx, hy, hz = (s / 2 for s in size)
    faces = [
        ((1, 0, 0), (0, 0, -1), (0, 1, 0)),
        ((-1, 0, 0), (0, 0, 1), (0, 1, 0)),
        ((0, 1, 0), (1, 0, 0), (0, 0, -1)),
        ((0, -1, 0), (1, 0, 0), (0, 0, 1)),
        ((0, 0, 1), (1, 0, 0), (0, 1, 0)),
        ((0, 0, -1), (-1, 0, 0), (0, 1, 0)),
    ]
    pos, nrm, idx = [], [], []
    for n, u, v in faces:
        base = len(pos)
        for su, sv in ((-1, -1), (1, -1), (1, 1), (-1, 1)):
            p = [n[k] + su * u[k] + sv * v[k] for k in range(3)]
            pos.append((cx + p[0] * hx, cy + p[1] * hy, cz + p[2] * hz))
            nrm.append(n)
        idx += [base, base + 1, base + 2, base, base + 2, base + 3]
    return pos, nrm, idx


def quat_x(deg):
    a = math.radians(deg) / 2
    return (math.sin(a), 0.0, 0.0, math.cos(a))


def quat_z(deg):
    a = math.radians(deg) / 2
    return (0.0, 0.0, math.sin(a), math.cos(a))


def walk_clip(n=16):
    """One second: each leg swings ±28° about X, the knee bends while the leg
    swings forward, the arms swing against the legs, and the hips drop by as
    much as the swung legs shorten, so the feet stay on the ground."""
    times = [i / n for i in range(n + 1)]
    tracks = {}

    def phase(t, offset=0.0):
        return math.sin(2 * math.pi * (t + offset))

    for side, off in (("L", 0.0), ("R", 0.5)):
        # A negative X rotation swings the foot forward (+Z).
        tracks[(f"UpperLeg.{side}", "rotation")] = [quat_x(-28 * phase(t, off)) for t in times]
        tracks[(f"LowerLeg.{side}", "rotation")] = [
            quat_x(35 * max(0.0, phase(t, off + 0.25))) for t in times
        ]
        tracks[(f"UpperArm.{side}", "rotation")] = [quat_x(22 * phase(t, off)) for t in times]
        tracks[(f"LowerArm.{side}", "rotation")] = [quat_x(-18) for _ in times]
    rest = JOINTS[INDEX["Hips"]][2]
    leg = 0.89  # hip joint to sole
    tracks[("Hips", "translation")] = [
        (rest[0], rest[1] - leg * (1 - math.cos(math.radians(28 * phase(t)))), rest[2]) for t in times
    ]
    tracks[("Spine", "rotation")] = [quat_x(4.0) for _ in times]
    return times, tracks


def idle_clip(n=8):
    """Two seconds of breathing: the spine and arms sway slightly."""
    times = [2.0 * i / n for i in range(n + 1)]
    tracks = {}
    tracks[("Spine", "rotation")] = [quat_x(1.5 * math.sin(math.pi * t)) for t in times]
    for side, sign in (("L", 1.0), ("R", -1.0)):
        tracks[(f"UpperArm.{side}", "rotation")] = [
            quat_z(sign * (4 + 1.5 * math.sin(math.pi * t))) for t in times
        ]
    return times, tracks


class Bin:
    def __init__(self):
        self.data = bytearray()
        self.views = []
        self.accessors = []

    def push(self, raw, component, count, ty, target=None, **extra):
        while len(self.data) % 4:
            self.data.append(0)
        view = {"buffer": 0, "byteOffset": len(self.data), "byteLength": len(raw)}
        if target is not None:
            view["target"] = target
        self.views.append(view)
        self.data += raw
        acc = {"bufferView": len(self.views) - 1, "componentType": component, "count": count, "type": ty}
        acc.update(extra)
        self.accessors.append(acc)
        return len(self.accessors) - 1

    def floats(self, rows, ty, target=None, minmax=False):
        flat = [round(v, 6) for row in rows for v in (row if isinstance(row, (tuple, list)) else (row,))]
        extra = {}
        if minmax:
            width = len(rows[0]) if isinstance(rows[0], (tuple, list)) else 1
            cols = [flat[k::width] for k in range(width)]
            extra = {"min": [min(c) for c in cols], "max": [max(c) for c in cols]}
        return self.push(struct.pack(f"<{len(flat)}f", *flat), 5126, len(rows), ty, target, **extra)


def build():
    rest = global_rest()
    pos, nrm, col, jnt, wgt, idx = [], [], [], [], [], []
    for joint, centre, size, colour in BOXES:
        p, n, i = box(centre, size)
        base = len(pos)
        pos += p
        nrm += n
        col += [colour] * len(p)
        jnt += [(INDEX[joint], 0, 0, 0)] * len(p)
        wgt += [(1.0, 0.0, 0.0, 0.0)] * len(p)
        idx += [base + k for k in i]

    b = Bin()
    a_pos = b.floats(pos, "VEC3", 34962, minmax=True)
    a_nrm = b.floats(nrm, "VEC3", 34962)
    a_col = b.floats(col, "VEC4", 34962)
    flat_j = [v for row in jnt for v in row]
    a_jnt = b.push(struct.pack(f"<{len(flat_j)}H", *flat_j), 5123, len(jnt), "VEC4", 34962)
    a_wgt = b.floats(wgt, "VEC4", 34962)
    a_idx = b.push(struct.pack(f"<{len(idx)}H", *idx), 5123, len(idx), "SCALAR", 34963)
    ibm = []
    for gx, gy, gz in rest:
        # Column-major inverse of a pure translation.
        ibm.append((1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, -gx, -gy, -gz, 1))
    a_ibm = b.floats(ibm, "MAT4")

    nodes = []
    for name, parent, t in JOINTS:
        children = [INDEX[c] for c, p, _ in JOINTS if p == name]
        node = {"name": name, "translation": list(t)}
        if children:
            node["children"] = children
        nodes.append(node)
    mesh_node = len(nodes)
    nodes.append({"name": "Body", "mesh": 0, "skin": 0})

    animations = []
    for clip_name, (times, tracks) in (("idle", idle_clip()), ("walk", walk_clip())):
        a_time = b.floats(times, "SCALAR", minmax=True)
        samplers, channels = [], []
        for (joint, path), values in sorted(tracks.items()):
            ty = "VEC4" if path == "rotation" else "VEC3"
            samplers.append({"input": a_time, "output": b.floats(values, ty), "interpolation": "LINEAR"})
            channels.append({"sampler": len(samplers) - 1, "target": {"node": INDEX[joint], "path": path}})
        animations.append({"name": clip_name, "samplers": samplers, "channels": channels})

    while len(b.data) % 4:
        b.data.append(0)
    doc = {
        "asset": {"version": "2.0", "generator": "examples/valley/tools/make_walker.py"},
        "scene": 0,
        "scenes": [{"nodes": [0, mesh_node]}],
        "nodes": nodes,
        "meshes": [{"name": "Body", "primitives": [{
            "attributes": {"POSITION": a_pos, "NORMAL": a_nrm, "COLOR_0": a_col,
                           "JOINTS_0": a_jnt, "WEIGHTS_0": a_wgt},
            "indices": a_idx,
        }]}],
        "skins": [{"name": "Walker", "joints": list(range(len(JOINTS))), "inverseBindMatrices": a_ibm,
                   "skeleton": 0}],
        "animations": animations,
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [{"byteLength": len(b.data)}],
    }
    js = json.dumps(doc, separators=(",", ":"), sort_keys=True).encode()
    while len(js) % 4:
        js += b" "
    out = bytearray(b"glTF")
    out += struct.pack("<II", 2, 12 + 8 + len(js) + 8 + len(b.data))
    out += struct.pack("<I", len(js)) + b"JSON" + js
    out += struct.pack("<I", len(b.data)) + b"BIN\0" + b.data
    return bytes(out)


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    target = root / "models" / "walker.glb"
    target.write_bytes(build())
    print(f"wrote {target} ({target.stat().st_size} bytes)")
