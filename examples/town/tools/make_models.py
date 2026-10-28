#!/usr/bin/env python3
"""Writes the courier truck's models: models/truck.glb and models/wheel.glb.

The truck is a pickup with an open bed for the parcels: a red cab with
windows, a flatbed with low sides and a tailgate, bumpers, lights and a sign
on the roof. It faces +Z, and its origin is the middle of its collision box
(1.0 m by 0.96 m by 2.62 m from the origin): the chassis rails reach 0.96 m
below it, the cab roof 0.96 m above, and the bed's floor is at -0.54 m.

The wheel is a tyre with a hub on both faces, 0.42 m in radius and 0.3 m
wide, turning about X. Colours are vertex colours, so one white material
draws each.

Standard library only. Run from the project directory:

    python3 tools/make_models.py

The output is byte-for-byte the same on every run, so the committed files can
be checked with `git diff --exit-code` after running it.
"""

import json
import math
import struct
import sys
from pathlib import Path

PAINT = (0.62, 0.06, 0.03, 1.0)
DARK = (0.03, 0.03, 0.035, 1.0)
CHROME = (0.55, 0.56, 0.58, 1.0)
GLASS = (0.02, 0.035, 0.05, 1.0)
WOOD = (0.32, 0.2, 0.09, 1.0)
LAMP = (1.0, 0.95, 0.8, 1.0)
TAIL = (0.7, 0.02, 0.01, 1.0)
SIGN = (0.95, 0.62, 0.02, 1.0)
TYRE = (0.025, 0.025, 0.025, 1.0)
HUB = (0.45, 0.46, 0.48, 1.0)


class Mesh:
    def __init__(self, lift=0.0):
        self.pos, self.nrm, self.col, self.idx = [], [], [], []
        self.lift = lift

    def quad(self, a, b, c, d, colour):
        """A flat quad, counter-clockwise seen from its front."""
        u = [b[i] - a[i] for i in range(3)]
        v = [c[i] - a[i] for i in range(3)]
        n = (u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0])
        length = math.sqrt(sum(x * x for x in n)) or 1.0
        n = tuple(x / length for x in n)
        base = len(self.pos)
        for p in (a, b, c, d):
            self.pos.append((p[0], p[1] + self.lift, p[2]))
            self.nrm.append(n)
            self.col.append(colour)
        self.idx += [base, base + 1, base + 2, base, base + 2, base + 3]

    def tri(self, a, b, c, colour):
        self.quad(a, b, c, c, colour)

    def box(self, lo, hi, colour):
        x0, y0, z0 = lo
        x1, y1, z1 = hi
        self.quad((x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1), colour)  # +Z
        self.quad((x1, y0, z0), (x0, y0, z0), (x0, y1, z0), (x1, y1, z0), colour)  # -Z
        self.quad((x1, y0, z1), (x1, y0, z0), (x1, y1, z0), (x1, y1, z1), colour)  # +X
        self.quad((x0, y0, z0), (x0, y0, z1), (x0, y1, z1), (x0, y1, z0), colour)  # -X
        self.quad((x0, y1, z1), (x1, y1, z1), (x1, y1, z0), (x0, y1, z0), colour)  # +Y
        self.quad((x0, y0, z0), (x1, y0, z0), (x1, y0, z1), (x0, y0, z1), colour)  # -Y


def truck():
    m = Mesh(lift=-0.34)
    # Chassis rails and bumpers.
    m.box((-0.9, -0.62, -2.55), (0.9, -0.32, 2.45), DARK)
    m.box((-1.02, -0.5, 2.45), (1.02, -0.2, 2.62), CHROME)
    m.box((-1.02, -0.5, -2.66), (1.02, -0.25, -2.5), DARK)
    # The cab: a box, the bonnet in front, a sloping windscreen.
    m.box((-1.0, -0.32, 0.55), (1.0, 0.55, 2.5), PAINT)  # lower cab and bonnet
    x0, x1 = -0.98, 0.98
    # Upper cab: back at z 0.6, roof to z 1.55, windscreen down to z 2.0 at y 0.55.
    m.box((-0.98, 0.55, 0.6), (0.98, 1.3, 1.55), PAINT)
    m.quad((x0, 0.55, 2.0), (x1, 0.55, 2.0), (x1, 1.3, 1.55), (x0, 1.3, 1.55), GLASS)  # windscreen
    m.tri((x1, 0.55, 2.0), (x1, 0.55, 1.55), (x1, 1.3, 1.55), PAINT)  # right of the screen
    m.tri((x0, 0.55, 1.55), (x0, 0.55, 2.0), (x0, 1.3, 1.55), PAINT)  # left of the screen
    # Side windows and the back window, a little proud of the cab.
    m.quad((1.0, 0.68, 1.5), (1.0, 0.68, 0.75), (1.0, 1.2, 0.75), (1.0, 1.2, 1.5), GLASS)
    m.quad((-1.0, 0.68, 0.75), (-1.0, 0.68, 1.5), (-1.0, 1.2, 1.5), (-1.0, 1.2, 0.75), GLASS)
    m.quad((0.7, 0.72, 0.58), (-0.7, 0.72, 0.58), (-0.7, 1.15, 0.58), (0.7, 1.15, 0.58), GLASS)
    # Headlights and grille on the front, tail lights at the back.
    for x in (-0.7, 0.7):
        m.quad((x - 0.2, 0.1, 2.51), (x + 0.2, 0.1, 2.51), (x + 0.2, 0.3, 2.51), (x - 0.2, 0.3, 2.51), LAMP)
    m.quad((-0.4, -0.15, 2.51), (0.4, -0.15, 2.51), (0.4, 0.3, 2.51), (-0.4, 0.3, 2.51), DARK)
    for x in (-0.85, 0.85):
        m.quad((x + 0.1, -0.2, -2.67), (x - 0.1, -0.2, -2.67), (x - 0.1, 0.05, -2.67), (x + 0.1, 0.05, -2.67), TAIL)
    # The courier's sign on the roof.
    m.box((-0.55, 1.3, 0.9), (0.55, 1.55, 1.25), SIGN)
    # The bed: a wooden floor, low sides, a board behind the cab and a tailgate.
    m.box((-0.95, -0.32, -2.55), (0.95, -0.2, 0.55), WOOD)
    m.box((0.88, -0.2, -2.55), (1.0, 0.3, 0.55), PAINT)
    m.box((-1.0, -0.2, -2.55), (-0.88, 0.3, 0.55), PAINT)
    m.box((-1.0, -0.2, -2.62), (1.0, 0.3, -2.5), PAINT)
    # Mudguards over the wheels.
    for z in (1.6, -1.5):
        for x0, x1 in ((0.92, 1.08), (-1.08, -0.92)):
            m.box((x0, 0.0, z - 0.55), (x1, 0.08, z + 0.55), DARK)
    return m


def wheel():
    m = Mesh()
    r, hub, w, n = 0.42, 0.24, 0.3, 20
    ring = [(math.cos(2 * math.pi * i / n), math.sin(2 * math.pi * i / n)) for i in range(n)]
    for i in range(n):
        (c0, s0), (c1, s1) = ring[i], ring[(i + 1) % n]
        # The tread, facing out.
        m.quad((w / 2, r * c0, r * s0), (-w / 2, r * c0, r * s0), (-w / 2, r * c1, r * s1), (w / 2, r * c1, r * s1), TYRE)
        for side in (1, -1):
            x = side * w / 2
            # Sidewall (tyre) and hub, facing out along X.
            a = (x, r * c0, r * s0)
            b = (x, r * c1, r * s1)
            ha = (x + side * 0.01, hub * c0, hub * s0)
            hb = (x + side * 0.01, hub * c1, hub * s1)
            centre = (x + side * 0.03, 0.0, 0.0)
            if side > 0:
                m.quad(a, b, (x, hub * c1, hub * s1), (x, hub * c0, hub * s0), TYRE)
                m.tri(ha, hb, centre, HUB)
            else:
                m.quad(b, a, (x, hub * c0, hub * s0), (x, hub * c1, hub * s1), TYRE)
                m.tri(hb, ha, centre, HUB)
    return m


def glb(mesh, generator):
    data = bytearray()
    views, accessors = [], []

    def push(raw, component, count, ty, target, **extra):
        while len(data) % 4:
            data.append(0)
        views.append({"buffer": 0, "byteOffset": len(data), "byteLength": len(raw), "target": target})
        data.extend(raw)
        acc = {"bufferView": len(views) - 1, "componentType": component, "count": count, "type": ty}
        acc.update(extra)
        accessors.append(acc)
        return len(accessors) - 1

    def floats(rows, ty, minmax=False):
        flat = [round(v, 6) for row in rows for v in row]
        extra = {}
        if minmax:
            width = len(rows[0])
            cols = [flat[k::width] for k in range(width)]
            extra = {"min": [min(c) for c in cols], "max": [max(c) for c in cols]}
        return push(struct.pack(f"<{len(flat)}f", *flat), 5126, len(rows), ty, 34962, **extra)

    a_pos = floats(mesh.pos, "VEC3", minmax=True)
    a_nrm = floats(mesh.nrm, "VEC3")
    a_col = floats(mesh.col, "VEC4")
    a_idx = push(struct.pack(f"<{len(mesh.idx)}H", *mesh.idx), 5123, len(mesh.idx), "SCALAR", 34963)
    while len(data) % 4:
        data.append(0)
    doc = {
        "asset": {"version": "2.0", "generator": generator},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"name": "Body", "mesh": 0}],
        "meshes": [{"name": "Body", "primitives": [{
            "attributes": {"POSITION": a_pos, "NORMAL": a_nrm, "COLOR_0": a_col},
            "indices": a_idx,
        }]}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": len(data)}],
    }
    js = json.dumps(doc, separators=(",", ":"), sort_keys=True).encode()
    while len(js) % 4:
        js += b" "
    out = bytearray(b"glTF")
    out += struct.pack("<II", 2, 12 + 8 + len(js) + 8 + len(data))
    out += struct.pack("<I", len(js)) + b"JSON" + js
    out += struct.pack("<I", len(data)) + b"BIN\0" + data
    return bytes(out)


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
    (root / "models").mkdir(exist_ok=True)
    for name, mesh in (("truck", truck()), ("wheel", wheel())):
        target = root / "models" / f"{name}.glb"
        target.write_bytes(glb(mesh, "examples/town/tools/make_models.py"))
        print(f"wrote {target} ({target.stat().st_size} bytes)")
