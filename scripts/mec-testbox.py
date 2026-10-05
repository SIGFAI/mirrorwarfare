#!/usr/bin/env python3
"""Write the `testbox` arena package (arena.glb, collision.glb, arena.json) for `map mec:testbox`.

"Catalyst: Parkour Playground": an 80 x 80 m runner arena built around Catalyst's move envelope
(context/artifacts/2026-10-04-mec-ant/README.md) that also plays as an FFA map with bots.

No dependencies, deterministic:  uv run scripts/mec-testbox.py [out_dir] [--preview top.png]
The default output is `$IW4L_MEC_ARENAS/testbox` (IW4L_MEC_ARENAS defaults to ../mec-arenas next
to the repo). glTF conventions: metres, +Y up, north = -Z. A spawn's `yaw_deg` turns about +Y,
counter-clockwise seen from above, 0 facing -Z. IW4 = (-z, -x, y) * 39.37 in.

Geometry is a list of closed boxes and wedges. Render faces are tessellated (~1 m) for a per-vertex
light bake (COLOR_0: rgb = sky visibility, a = sun visibility, ray traced against the same solids)
and cells hidden inside another solid are dropped; collision.glb holds every solid as a closed
volume, written with both windings, so render and collision are the same surfaces.

Levels: ground 0, west roofs 3.5 / east roofs 4.0, wall-kick block 4.75, deck 7.0, tower 12.0.
"""
import json
import math
import os
import struct
import sys
import zlib
from pathlib import Path

TITLE = "Catalyst: Parkour Playground"
HALF = 40.0           # interior is [-40, 40] on x and z
KICK_B_H = 4.75       # wall-kick block B: above the 4.49 m a wallclimb from the ground heaves to
KICK_GAP = 3.5        # wall A face -> block B face
WALL_H = 16.0         # boundary wall height: above any wallrun / ledge reach from the tower
SUN_DIR = (0.38, -0.80, -0.46)   # direction sunlight travels (glTF)

# ----------------------------------------------------------------------------- textures


def png_rgba(width, height, pixel):
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixel(x, y))

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")


def hash2(x, y, seed=0):
    h = (x * 374761393 + y * 668265263 + seed * 2147483647) & 0xFFFFFFFF
    h = ((h ^ (h >> 13)) * 1274126177) & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFF) / 65535.0


def clamp8(v):
    return max(0, min(255, int(round(v))))


def tex_tiles(base, grout, cells=4, size=128, seed=1, var=5.0, noise=3.0):
    """Square tiles with grout lines and a little per-tile tone variation."""
    step = size // cells

    def pixel(x, y):
        tx, ty = x // step, y // step
        if x % step == 0 or y % step == 0:
            return (*grout, 255)
        t = (hash2(tx, ty, seed) - 0.5) * 2 * var + (hash2(x, y, seed + 7) - 0.5) * 2 * noise
        return (*(clamp8(c + t) for c in base), 255)

    return png_rgba(size, size, pixel)


def tex_panels(base, seam, size=128, seed=2, var=4.0, noise=2.5, rows=2, cols=2, bolts=True):
    """Large wall panels: seams, faint per-panel tone, bolt dots near the corners."""
    pw, ph = size // cols, size // rows

    def pixel(x, y):
        px, py = x // pw, y // ph
        lx, ly = x % pw, y % ph
        if lx == 0 or ly == 0:
            return (*seam, 255)
        if lx == 1 or ly == 1:
            return (*(clamp8((a + b) / 2) for a, b in zip(base, seam)), 255)
        if bolts and (lx in (4, pw - 4)) and (ly in (4, ph - 4)):
            return (*(clamp8(c - 22) for c in base), 255)
        t = (hash2(px, py, seed) - 0.5) * 2 * var + (hash2(x, y, seed + 3) - 0.5) * 2 * noise
        # a soft vertical gradient per panel so big walls are never one flat value
        t += (ly / ph - 0.5) * -4.0
        return (*(clamp8(c + t) for c in base), 255)

    return png_rgba(size, size, pixel)


def tex_flat(base, size=32, seed=3, noise=4.0, stripe=None):
    def pixel(x, y):
        t = (hash2(x, y, seed) - 0.5) * 2 * noise
        if stripe and (x + y) % 16 < 2:
            t -= stripe
        return (*(clamp8(c + t) for c in base), 255)

    return png_rgba(size, size, pixel)


# (name, png, metres per texture repeat). Whites stay well under 255: the lit result must not clip.
MATERIALS = [
    ("pp_floor", lambda: tex_tiles((174, 176, 179), (144, 146, 150), cells=4, seed=11), 2.0),
    ("pp_white", lambda: tex_panels((198, 200, 203), (162, 164, 168), seed=12), 4.0),
    ("pp_grey", lambda: tex_panels((168, 171, 175), (136, 139, 144), seed=13, rows=4, cols=2, bolts=False), 4.0),
    ("pp_boundary", lambda: tex_panels((142, 146, 152), (112, 116, 122), seed=14, rows=1, cols=1), 8.0),
    ("pp_red", lambda: tex_flat((206, 42, 34), seed=15), 1.0),
    ("pp_orange", lambda: tex_flat((226, 128, 38), seed=16), 1.0),
    ("pp_blue", lambda: tex_flat((52, 118, 200), seed=17), 1.0),
    ("pp_green", lambda: tex_flat((70, 168, 98), seed=18), 1.0),
    ("pp_yellow", lambda: tex_flat((232, 186, 40), seed=19), 1.0),
    ("pp_purple", lambda: tex_flat((140, 78, 196), seed=20), 1.0),
]
M = {name: i for i, (name, _, _) in enumerate(MATERIALS)}
FLOOR, WHITE, GREY, BOUND, RED, ORANGE, BLUE, GREEN, YELLOW, PURPLE = range(10)

# ----------------------------------------------------------------------------- solids


class Solid:
    """A closed solid. kind 'box': lo/hi. kind 'ramp': a wedge over the box footprint rising
    from lo.y at the low edge to hi.y at the high edge along `rise` ('+x', '-x', '+z', '-z')."""

    def __init__(self, name, lo, hi, *, kind="box", rise=None, body=WHITE, top=None, skirt=None,
                 lips=(), stripe=None, red_top=False, red_all=False, step=1.0, render=True):
        self.name, self.lo, self.hi, self.kind, self.rise = name, tuple(lo), tuple(hi), kind, rise
        self.body, self.top = body, (body if top is None else top)
        self.skirt, self.lips, self.stripe = skirt, list(lips), stripe
        self.red_top, self.red_all, self.step, self.render = red_top, red_all, step, render
        if kind == "ramp":
            # solid side of the slope plane: n . p <= d, n pointing up and away from the rise
            x0, y0, z0 = lo
            x1, y1, z1 = hi
            h = y1 - y0
            axis = 0 if rise[1] == "x" else 2
            span = (x1 - x0) if axis == 0 else (z1 - z0)
            sgn = 1.0 if rise[0] == "+" else -1.0
            n = [0.0, span, 0.0]
            n[axis] = -sgn * h
            ln = math.sqrt(n[0] ** 2 + n[1] ** 2 + n[2] ** 2)
            self.plane_n = (n[0] / ln, n[1] / ln, n[2] / ln)
            low = lo[axis] if sgn > 0 else hi[axis]
            p = [0.0, y0, 0.0]
            p[axis] = low
            self.plane_d = self.plane_n[0] * p[0] + self.plane_n[1] * p[1] + self.plane_n[2] * p[2]
            self.axis, self.sgn = axis, sgn

    def height_at(self, x, z):
        if self.kind == "box":
            return self.hi[1]
        c = x if self.axis == 0 else z
        a0, a1 = (self.lo[self.axis], self.hi[self.axis])
        f = (c - a0) / (a1 - a0)
        if self.sgn < 0:
            f = 1.0 - f
        return self.lo[1] + f * (self.hi[1] - self.lo[1])

    def inside(self, p, eps=1e-3):
        lo, hi = self.lo, self.hi
        if not (lo[0] + eps < p[0] < hi[0] - eps and lo[1] + eps < p[1] < hi[1] - eps and lo[2] + eps < p[2] < hi[2] - eps):
            return False
        if self.kind == "ramp":
            n = self.plane_n
            return n[0] * p[0] + n[1] * p[1] + n[2] * p[2] < self.plane_d - eps
        return True


SOLIDS = []
FEATURES = []   # (label, description, (x, z)) for the preview and the report


def box(name, lo, hi, **kw):
    s = Solid(name, lo, hi, **kw)
    SOLIDS.append(s)
    return s


def ramp(name, x0, x1, z0, z1, y0, y1, rise, **kw):
    s = Solid(name, (x0, y0, z0), (x1, y1, z1), kind="ramp", rise=rise, **kw)
    SOLIDS.append(s)
    return s


def feature(label, text, x, z):
    FEATURES.append((label, text, (x, z)))


def parapet(name, lo, hi, **kw):
    """Thin 1.0 m safety rail on a roof edge (vaultable; too thin for a bot nav node)."""
    return box(name, lo, hi, body=GREY, lips=[("all", None)], **kw)


def build():
    """Levels: ground 0; E2 2.0; L1 2.6; W1 / balconies / R5 3.5; R1 / R3 / R4 4.0; deck 7.0;
    shoulder 9.5; tower 12.0. Five set-pieces, one accent each, chained by the showcase line:
    corridor (blue, west), descent (green, north-east), rooftop gaps (orange, east),
    wall kick (purple, south), ascent (yellow, centre-north). Red = runner vision on every
    traversal edge."""
    H = HALF
    # --- floor and boundary --------------------------------------------------------------
    box("floor", (-H - 1, -0.5, -H - 1), (H + 1, 0.0, H + 1), body=GREY, top=FLOOR, step=1.0)
    for name, lo, hi in [
        ("bound_n", (-H - 1, 0, -H - 1), (H + 1, WALL_H, -H)),
        ("bound_s", (-H - 1, 0, H), (H + 1, WALL_H, H + 1)),
        ("bound_w", (-H - 1, 0, -H), (-H, WALL_H, H)),
        ("bound_e", (H, 0, -H), (H + 1, WALL_H, H)),
    ]:
        box(name, lo, hi, body=BOUND, step=2.0, stripe=("in", 0.0, 0.25, RED))

    # --- NORTH: the deck (7.0 m) ---------------------------------------------------------
    deck_lips = [("+z", (-10.0, -4.0)), ("+z", (11.5, 17.5)), ("-x", (-32.0, -25.0)),
                 ("+x", (-30.5, -25.5)), ("+x", (-33.5, -31.0)), ("-z", (8.0, 12.0))]
    box("deck_w", (-18, 0, -34), (-2, 7.0, -14), lips=deck_lips, skirt=(GREY, 0.4))
    box("deck_e", (2, 0, -34), (18, 7.0, -14), lips=deck_lips, skirt=(GREY, 0.4))
    box("tunnel_roof", (-2, 3.0, -34), (2, 7.0, -14), lips=deck_lips)
    feature("1", "Deck 7.0 m (36 x 20 m) over a 4 x 3 m tunnel", -8, -18)
    parapet("par_n1", (-18, 7, -34), (8, 8, -33.7))
    parapet("par_n2", (12, 7, -34), (18, 8, -33.7))
    parapet("par_w1", (-18, 7, -33.7), (-17.7, 8, -32))
    parapet("par_w2", (-18, 7, -25), (-17.7, 8, -14.3))
    parapet("par_e1", (17.7, 7, -31), (18, 8, -30.5))
    parapet("par_e2", (17.7, 7, -25.5), (18, 8, -14.3))
    parapet("par_s1", (-18, 7, -14.3), (-10, 8, -14))
    parapet("par_s2", (-4, 7, -14.3), (4, 8, -14))
    parapet("par_s3", (4, 7, -14.3), (11.5, 8, -14))
    parapet("par_s4", (17.5, 7, -14.3), (18, 8, -14))
    feature("2", "High drop: deck -> north ring, 7.0 m (roll / hard landing)", 10, -36)
    # Deck lane (z -30.5..-25.5): short and long vault on the way east to the descent.
    box("dv1", (-12, 7.0, -30.5), (-11.5, 7.9, -25.5), body=GREY, lips=[("-x", None)])
    box("dv2", (-7, 7.0, -30.5), (-4, 8.0, -25.5), body=GREY, lips=[("-x", None)])
    feature("3", "Deck lane: vault 0.9 m x 0.5 m, long vault 1.0 m x 3.0 m", -8, -31)

    # --- ASCENT (yellow): plaza 0 -> balcony 3.5 -> deck 7.0 -> shoulder 9.5 -> tower 12.0 --
    y = dict(skirt=(YELLOW, 0.5))
    box("asc_wall", (13.4, 0, -9), (14.0, 6.0, -2), body=WHITE, stripe=("+x", 1.5, 2.3, YELLOW), lips=[("all", None)])
    box("balc_e", (11.5, 0, -14), (17.5, 3.5, -10), lips=[("+z", None), ("-x", None), ("+x", None)], **y)
    box("balc_w", (-10, 0, -14), (-4, 3.5, -10), lips=[("+z", None), ("-x", None), ("+x", None)], skirt=(GREY, 0.4))
    box("shoulder", (9.5, 7.0, -26.5), (11.5, 9.5, -21.5), lips=[("+x", None), ("-z", None), ("+z", None)], **y)
    box("tower", (4.5, 7.0, -26.5), (9.5, 12.0, -21.5), lips=[("-x", None), ("+z", None), ("+x", None), ("-z", None)],
        skirt=(YELLOW, 0.5))
    feature("4", "Ascent: wallrun 7 m -> wallclimb balcony 3.5 -> climb deck 7.0 -> ledge 9.5 -> tower 12.0", 14.5, -6)
    feature("5", "Tower 12.0 m (5 m above the deck: out of wallclimb reach without the shoulder)", 7, -24)
    # Second tower route: vault the 1.0 m launch block, wallrun the fin into the west face,
    # wallclimb, ledge (jump start 8.0 m: cap 12.49 m).
    box("tower_fin", (-3, 7.0, -21.5), (4.5, 11.0, -21.1), stripe=("-z", 7.9, 8.6, RED), lips=[("-z", None)])
    box("tower_launch", (-4, 7.0, -23.2), (1.0, 8.0, -21.5), lips=[("+x", None), ("-x", None)], body=GREY)
    feature("6", "Tower fin wallrun 7.5 m off a 1.0 m launch block -> wallclimb -> tower", -1, -22.5)

    # --- DESCENT (green): deck 7.0 -> R1 4.0 -> E2 2.0 -> ground -> springboard -----------
    gr = dict(skirt=(GREEN, 0.5))
    box("r1", (18, 0, -34), (34, 4.0, -20), lips=[("+z", None)], **gr)
    ramp("r1_ramp", 18, 27, -33.5, -31, 4.0, 7.0, "-x", body=GREY, top=WHITE)
    box("e2", (22, 0, -20), (34, 2.0, -16), lips=[("+z", None), ("-z", None)], **gr)
    ramp("e2_ramp", 18, 22, -19.5, -16.5, 0.0, 2.0, "+x", body=GREY, top=WHITE)
    box("spring_d", (24.5, 0, -0.25), (28.5, 1.2, 0.35), body=GREEN, red_top=True)
    feature("7", "Descent: drops 3.0 / 2.0 / 2.0 m with rolls -> springboard 1.2 m -> R3 ledge", 26, -16)

    # --- ROOFTOP GAPS (orange): R3 4.0 -> gap 3 m + coil bar -> R4 4.0 -> gap 4.5 m -> R5 3.5
    o = dict(skirt=(ORANGE, 0.5))
    box("r3", (22, 0, 3.5), (34, 4.0, 9), lips=[("-z", None), ("+z", None)], **o)
    box("coil_bar", (22, 4.9, 10.35), (34, 5.3, 10.65), body=ORANGE, lips=[("all", None)])
    box("r4", (22, 0, 12), (34, 4.0, 19), lips=[("-z", None), ("+z", None)], **o)
    box("r5", (22, 0, 23.5), (34, 3.5, 32), lips=[("-z", None), ("+z", None)], **o)
    box("slide_beam", (22, 4.9, 26), (34, 5.5, 27), red_all=True)
    box("vault_out", (22, 3.5, 28), (22.6, 4.5, 31.5), body=ORANGE, lips=[("+x", None)])
    feature("8", "Gap 3.0 m with a coil bar 0.9-1.3 m above the roofline", 28, 10.5)
    feature("9", "Gap 4.5 m (4.0 -> 3.5): fast jump only", 28, 21.2)
    feature("10", "R5: land into a slide under the 1.4 m beam, curve west, vault out off the edge (3.5 m drop)", 28, 27)

    # --- WALL KICK (purple, south): underpass under block B -> wall A -> back up onto B ------
    # On the R5 vault-out line (z 29.75), heading west: run through a 2.4 m underpass in B,
    # wallclimb wall A 3.5 m past it, quickturn on the wall, kick off backwards (7.2 m/s) and
    # grab B's 4.75 m lip - above the 4.49 m a wallclimb from the ground heaves to, so the turn
    # is the only way up. A (6.5 m) has nothing to grab.
    vo = solids_by_name()["vault_out"]
    # The line the runner leaves R5 on: the vault out drifts south of the vault box centre
    # (~0.2 m per m), so the 3 m underpass is centred where the landing roll passes through it.
    kz = 0.5 * (vo.lo[2] + vo.hi[2]) + 1.75
    p = dict(skirt=(PURPLE, 0.5))
    box("kick_b", (11.0, 2.4, kz - 2.5), (15.0, KICK_B_H, kz + 2.5), lips=[("-x", None), ("-z", None), ("+x", None)],
        stripe=("-x", KICK_B_H - 2.4 - 0.75, KICK_B_H - 2.4 - 0.15, PURPLE))
    box("kick_b_n", (11.0, 0, kz - 2.5), (15.0, 2.4, kz - 1.5), **p)
    box("kick_b_s", (11.0, 0, kz + 1.5), (15.0, 2.4, kz + 2.5), **p)
    box("kick_a", (11.0 - KICK_GAP - 0.6, 0, kz - 3.0), (11.0 - KICK_GAP, 6.5, kz + 3.0),
        stripe=("+x", 1.5, 2.3, PURPLE), skirt=(PURPLE, 0.5))
    feature("22", "Wall kick: 3 x 2.4 m underpass in B (4.75 m), wall A 3.5 m on: wallclimb -> quickturn -> kick -> ledge B",
            9.0, kz)

    # --- CORRIDOR (blue, west): wall B -> wall jump -> wall A -> ledge L1 -> W1 -> deck -----
    b = dict(skirt=(BLUE, 0.5))
    box("w1", (-34, 0, -34), (-18, 3.5, -22), lips=[("+z", None), ("-x", None), ("-z", None)], skirt=(GREY, 0.4))
    feature("11", "W1 roof 3.5 m: 1.9 m jump-up from L1, 3.5 m climb face up to the deck", -27, -28)
    ramp("w1_ramp", -22, -18, -22, -12, 0.0, 3.5, "-z", body=GREY, top=WHITE)
    box("wr_b", (-26.0, 0, -6), (-25.4, 6.0, 2), body=WHITE, stripe=("-x", 1.5, 2.3, BLUE), lips=[("all", None)])
    box("wr_a", (-29.2, 0, -16), (-28.6, 6.0, -4), body=WHITE, stripe=("+x", 1.5, 2.3, BLUE), lips=[("all", None)])
    feature("12", "Corridor: walls B (8 m) and A (12 m), 6 m tall, faces 2.6 m apart", -27.6, -5)
    box("l1", (-30, 0, -22), (-25.4, 1.6, -17), lips=[("+z", None), ("-z", None)], **b)
    feature("13", "L1 ledge 1.6 m past the end of wall A (wallrun -> ledge), 1.9 m jump-up to W1", -27.7, -19.5)
    box("bastion", (-34, 0, 14), (-24, 4.8, 24), lips=[("-z", None), ("+x", None)], skirt=(GREY, 0.4))
    box("bastion_step", (-33, 0, 11), (-29, 1.2, 14), lips=[("-z", None), ("-x", None), ("+x", None)], body=GREY)
    feature("14", "Bastion 4.8 m: out of reach from the ground; 1.2 m step -> wallclimb -> ledge", -29, 19)
    box("rail_w1", (-37, 0, -10.15), (-34, 1.0, -9.85), body=GREY, lips=[("all", None)])
    box("rail_w2", (-37, 0, 5.85), (-34, 1.0, 6.15), body=GREY, lips=[("all", None)])
    box("spring_w", (-40, 0, 26), (-37, 1.1, 26.8), body=GREY, red_top=True)

    # --- PLAZA (centre, ground) ----------------------------------------------------------
    box("c_plat", (-3, 0, -3), (3, 1.2, 3), lips=[("-x", None), ("+x", None), ("-z", None), ("+z", None)], skirt=(GREY, 0.3))
    box("c_pillar", (-1, 1.2, -1), (1, 3.8, 1), lips=[("-x", None), ("+x", None), ("-z", None), ("+z", None)])
    feature("15", "Centre: 1.2 m platform (vault onto) + 2.6 m climb pillar", 0, 0)
    for nm, lo, hi in (("k1", (-13, 0, -8), (-9, 2.5, -6)), ("k2", (9, 0, 6), (13, 2.5, 8)),
                       ("k3", (-13, 0, 4), (-11, 2.5, 9)), ("k4", (11, 0, -6), (13, 2.5, -1)),
                       ("k5", (-4, 0, 24), (0, 2.5, 27)), ("k6", (8, 0, 25), (11, 2.5, 27))):
        box(nm, lo, hi, body=WHITE, skirt=(GREY, 0.3))
    for nm, lo, hi in (("h1", (-7, 0, 7), (-3, 1.2, 7.4)), ("h2", (1, 0, -7.4), (5, 1.2, -7)),
                       ("h3", (3, 0, 3), (3.4, 1.2, 7)), ("h4", (-6.4, 0, -7), (-6, 1.2, -3)),
                       ("h5", (-12, 0, 30), (-6, 1.0, 30.3))):
        box(nm, lo, hi, body=GREY, lips=[("all", None)])
    feature("16", "Cover: 2.5 m blocks, 1.0-1.2 m half walls (vault)", -11, -7)

    # --- SOUTH: vault garden (the showcase runs north up x = 14.5) ---------------------------
    box("v1", (-18, 0, 16), (-15, 0.9, 16.4), body=WHITE, lips=[("-z", None), ("+z", None)])
    box("v2", (-13, 0, 17), (-10, 1.2, 18), body=WHITE, lips=[("-z", None), ("+z", None)])
    box("v3", (18.5, 0, 8), (21.5, 1.5, 9.2), body=WHITE, lips=[("-z", None), ("+z", None)])
    box("v4", (-1, 0, 15), (2, 1.0, 18), body=WHITE, lips=[("-z", None), ("+z", None)])
    # v5 sits ~10 m past the wall-kick drop: a 1.5-2 s sprint out of the roll before the vault,
    # and ~1 s of running after it before the ascent wallrun.
    box("v5", (13, 0, 8.5), (16, 1.4, 11), body=WHITE, lips=[("-z", None), ("+z", None)])
    feature("17", "Vaults (h x depth): 0.9 x 0.4, 1.2 x 1.0, 1.5 x 1.2; long 1.0 x 3.0, 1.4 x 2.5", 14.5, 12.5)
    box("s1", (-16, 0, 22), (-14, 1.0, 22.8), body=WHITE, red_top=True)
    box("s2", (17, 0, 15), (17.8, 1.3, 17), body=WHITE, red_top=True)
    box("s3", (-18, 0, 28), (-16, 1.5, 28.8), body=WHITE, red_top=True)
    feature("18", "Springboards 1.0 / 1.3 / 1.5 m", 17.4, 18.5)
    for i, x in enumerate((-20, -6, 8)):
        box(f"slide_{i}", (x, 1.4, 34), (x + 1.0, 2.2, 37), red_all=True)
    feature("19", "Slide lane (south ring, 74 m) with 1.4 m slide-under bars", -6, 37)
    box("hurdle_1", (-24, 0, -37), (-23.6, 0.9, -34), body=GREY, lips=[("all", None)])
    box("hurdle_2", (22, 0, -37), (22.4, 1.2, -34), body=GREY, lips=[("all", None)])
    feature("20", "North ring vault lane: hurdles 0.9 m, 1.2 m", -23.8, -37)
    feature("21", "Speed loop: the 6 m ring around everything (296 m)", 37, 24)


def solids_by_name():
    return {s.name: s for s in SOLIDS}


def showcase_targets():
    """Aim points for the showcase line (tools/harness/showcase_route.py), glTF metres, derived
    from the solids: where a runner looks before each move - the middle of the wall segment
    about to be run, the midpoint of a lip about to be climbed, the centre of a vault top, the
    centre of a landing zone. Call after build()."""
    S = solids_by_name()

    def pt(name, fx, fy, fz):
        s = S[name]
        return tuple(round(s.lo[k] + (s.hi[k] - s.lo[k]) * f, 3) for k, f in enumerate((fx, fy, fz)))

    lane_z = 0.5 * (S["dv1"].lo[2] + S["dv1"].hi[2])
    east_x = 0.5 * (S["spring_d"].lo[0] + S["spring_d"].hi[0])
    north_x = 0.5 * (S["v5"].lo[0] + S["v5"].hi[0])
    return {
        # corridor (wall B on the right, wall A on the left, L1 / W1 lips ahead)
        "wr_b_far": (S["wr_b"].lo[0] - 0.6, 1.6, S["wr_b"].lo[2] - 6.0),
        "wr_b_mid": (S["wr_b"].lo[0], 1.8, 0.5 * (S["wr_b"].lo[2] + S["wr_b"].hi[2])),
        "wr_a_mid": (S["wr_a"].hi[0], 1.8, 0.5 * (S["wr_a"].lo[2] + S["wr_a"].hi[2])),
        "l1_lip": pt("l1", 0.6, 1.0, 1.0),
        "w1_lip": (-27.7, S["w1"].hi[1], S["w1"].hi[2]),
        "deck_west_lip": (S["deck_w"].lo[0], S["deck_w"].hi[1], -28.5),
        # deck lane
        "dv1_top": (0.5 * (S["dv1"].lo[0] + S["dv1"].hi[0]), S["dv1"].hi[1], lane_z),
        "dv2_top": (0.5 * (S["dv2"].lo[0] + S["dv2"].hi[0]), S["dv2"].hi[1], lane_z),
        "deck_east_edge": (S["deck_e"].hi[0], S["deck_e"].hi[1], lane_z),
        # descent
        "r1_landing": (S["r1"].lo[0] + 5.0, S["r1"].hi[1], lane_z),
        "r1_south_edge": (east_x, S["r1"].hi[1], S["r1"].hi[2]),
        "e2_south_edge": (east_x, S["e2"].hi[1], S["e2"].hi[2]),
        "spring_top": (east_x, S["spring_d"].hi[1], 0.5 * (S["spring_d"].lo[2] + S["spring_d"].hi[2])),
        "r3_lip": (east_x, S["r3"].hi[1], S["r3"].lo[2]),
        # gaps
        "coil_bar": (east_x, S["coil_bar"].hi[1], 0.5 * (S["coil_bar"].lo[2] + S["coil_bar"].hi[2])),
        "r4_landing": (east_x, S["r4"].hi[1], S["r4"].lo[2] + 2.5),
        "r5_landing": (east_x, S["r5"].hi[1], S["r5"].lo[2] + 1.5),
        "slide_beam": (east_x, S["slide_beam"].lo[1] - 0.4, 0.5 * (S["slide_beam"].lo[2] + S["slide_beam"].hi[2])),
        "vault_out_top": (0.5 * (S["vault_out"].lo[0] + S["vault_out"].hi[0]), S["vault_out"].hi[1],
                          0.5 * (S["vault_out"].lo[2] + S["vault_out"].hi[2])),
        "ground_west_of_r5": (S["r5"].lo[0] - 6.0, 0.0, 0.5 * (S["vault_out"].lo[2] + S["vault_out"].hi[2])),
        # ascent
        "v5_top": (north_x, S["v5"].hi[1], 0.5 * (S["v5"].lo[2] + S["v5"].hi[2])),
        "v3_top": (0.5 * (S["v3"].lo[0] + S["v3"].hi[0]), S["v3"].hi[1], 0.5 * (S["v3"].lo[2] + S["v3"].hi[2])),
        "asc_wall_mid": (S["asc_wall"].hi[0], 1.8, 0.5 * (S["asc_wall"].lo[2] + S["asc_wall"].hi[2])),
        "balc_lip": (north_x, S["balc_e"].hi[1], S["balc_e"].hi[2]),
        "deck_south_lip": (north_x, S["deck_e"].hi[1], S["deck_e"].hi[2]),
        "shoulder_lip": (S["shoulder"].hi[0], S["shoulder"].hi[1], 0.5 * (S["shoulder"].lo[2] + S["shoulder"].hi[2])),
        "tower_lip": (S["tower"].hi[0], S["tower"].hi[1], 0.5 * (S["shoulder"].lo[2] + S["shoulder"].hi[2])),
        "tower_west_drop": (S["tower"].lo[0] - 3.0, S["deck_e"].hi[1], 0.5 * (S["shoulder"].lo[2] + S["shoulder"].hi[2])),
        "deck_west_run": (-6.0, S["deck_e"].hi[1] + 1.0, 0.5 * (S["shoulder"].lo[2] + S["shoulder"].hi[2])),
        # wall kick
        "kick_a_mid": (S["kick_a"].hi[0], 3.0, 0.5 * (S["kick_a"].lo[2] + S["kick_a"].hi[2])),
        "kick_b_lip": (S["kick_b"].lo[0], S["kick_b"].hi[1], 0.5 * (S["kick_b"].lo[2] + S["kick_b"].hi[2])),
        "kick_b_north": (S["kick_b"].hi[0] - 0.5, S["kick_b"].hi[1], S["kick_b"].lo[2]),
    }


# The springboard must launch (movement_mec buffers the jump ~0.5 s before the obstacle).
SPRING_EXPECT = "Air"
# Wall kick timing (autopilot ticks, 50 ms): quickturn this long into the wallclimb, jump this
# long into WallClimb180 (its jump window opens at 20 t = 0.333 s).
KICK_TURN_TICKS = 8
KICK_JUMP_TICKS = 7


def showcase_route():
    """The showcase line as autopilot data (crates/console/src/autopilot.rs): a polyline of
    waypoints through every set-piece with actions on state triggers and the mec mode each
    action must produce. Built from the solids, so moving a set-piece moves its route.
    Call after build(). glTF metres; the start pose is for one `move` before `autopilot`."""
    S = solids_by_name()
    T = showcase_targets()

    def c(name, k):
        return 0.5 * (S[name].lo[k] + S[name].hi[k])

    run_b = S["wr_b"].lo[0] - 0.6           # runner axis beside wall B (0.22 m from the hull)
    run_a = S["wr_a"].hi[0] + 0.6           # ... beside wall A
    lane = c("dv1", 2)                      # deck lane
    ex = c("spring_d", 0)                   # east line (descent + gaps)
    nx = c("v5", 0)                         # north line (vault garden + ascent)
    run_asc = S["asc_wall"].hi[0] + 0.6
    tz = c("shoulder", 2)                   # shoulder / tower line
    deck = S["deck_e"].hi[1]
    seg = []

    def add(id_, to, **kw):
        d = {"id": id_, "to": [round(v, 3) for v in to]}
        d.update(kw)
        seg.append(d)

    def act(cond=None, **kw):
        a = {"if": cond or {}}
        a.update(kw)
        return a

    F, J, C, Q = "+forward", "+gostand", "+movedown", "+attack"
    # --- corridor (blue) ------------------------------------------------------------------
    add("sprint", (run_b, 0, S["wr_b"].hi[2] - 2.0), caption="Corridor", hold_line=True,
        actions=[act(hold=[F])])
    add("wallrun_b", (run_b, 1, S["wr_b"].hi[2] - 5.5), reach=0.6, hold_line=True,
        actions=[act(hold=[J], expect="WallRun", within_ticks=4, caption="Wallrun"),
                 act({"mode": "WallRun", "mode_ticks_ge": 2}, release=[J])])
    add("wall_jump", (run_a, 1.5, S["wr_a"].hi[2] - 2.5), until={"mode": "WallRun", "seg_ticks_ge": 2},
        actions=[act({"mode": "WallRun"}, hold=[J], expect="Air", within_ticks=3, caption="Wall jump"),
                 act({"mode": "Air"}, expect="WallRun", within_ticks=12)])
    add("wallrun_a", (run_a, 1.5, S["wr_a"].lo[2]), until={"not_mode": "WallRun"},
        actions=[act({"mode": "WallRun", "mode_ticks_ge": 3}, hold=[Q], caption="Wallrun + hip fire"),
                 act({"seg_ticks_ge": 12}, release=[Q])])
    add("ledge_l1", (run_a, S["l1"].hi[1], S["l1"].hi[2] - 0.6), glance=list(T["l1_lip"]),
        until={"mode": "Ground", "y_gt": S["l1"].hi[1] - 0.2},
        actions=[act(release=[Q, J]), act({"mode": "LedgeClimb"}, caption="Wallrun to ledge")])
    add("jump_w1", (run_a, S["l1"].hi[1], S["w1"].hi[2] + 1.5), reach=0.4, glance=list(T["w1_lip"]))
    add("ledge_w1", (run_a, S["w1"].hi[1], S["w1"].hi[2] - 0.6), glance=list(T["w1_lip"]),
        until={"mode": "Ground", "y_gt": S["w1"].hi[1] - 0.2},
        actions=[act(hold=[J], expect="LedgeClimb", within_ticks=14, caption="Jump-up to ledge"),
                 act({"mode": "LedgeClimb"}, release=[J])])
    add("w1_run", (-22.5, S["w1"].hi[1], -26.6))
    add("deck_climb", (S["deck_w"].lo[0] - 0.4, S["w1"].hi[1], -28.5), glance=list(T["deck_west_lip"]),
        until={"mode": "Ground", "y_gt": deck - 0.2},
        actions=[act({"dist_lt": 2.6}, hold=[J], expect="WallClimb", within_ticks=16, caption="Wallclimb + ledge"),
                 act({"mode": "LedgeClimb"}, release=[J])])
    # --- deck lane: short and long vault ------------------------------------------------
    add("vault", (S["dv1"].hi[0] + 1.0, deck, lane), glance=list(T["dv1_top"]), glance_dist=4.0,
        actions=[act({"dist_lt": 5.5}, expect="Vault", within_ticks=20, caption="Vault")])
    add("long_vault", (S["dv2"].hi[0] + 1.0, deck, lane), glance=list(T["dv2_top"]), glance_dist=4.0,
        actions=[act({"dist_lt": 6.5}, expect="Vault", within_ticks=20, caption="Long vault")])
    add("deck_run", (S["deck_e"].hi[0] - 2.0, deck, lane))
    # --- descent (green) ----------------------------------------------------------------
    add("drop_r1", (S["deck_e"].hi[0] + 1.5, deck, lane), glance=list(T["r1_landing"]), glance_dist=5.0,
        until={"mode": "Roll"},
        actions=[act({"falling": True, "y_lt": deck - 0.3}, hold=[C], expect="Roll", within_ticks=20,
                     caption="Drop 3 m + roll")])
    add("r1_roll", (ex - 1.5, S["r1"].hi[1], lane + 0.5), until={"not_mode": "Roll"},
        actions=[act({"mode_ticks_ge": 6}, release=[C])])
    add("r1_run", (ex, S["r1"].hi[1], S["r1"].hi[2] - 2.0))
    add("drop_e2", (ex, S["r1"].hi[1], S["r1"].hi[2] + 1.0), glance=list(T["e2_south_edge"]), glance_dist=4.0,
        until={"mode": "Roll"},
        actions=[act({"falling": True, "y_lt": S["r1"].hi[1] - 0.3}, hold=[C], expect="Roll", within_ticks=20,
                     caption="Drop 2 m + roll")])
    add("roll_chain", (ex, 0, S["e2"].hi[2] + 2.0), until={"mode": "Roll", "y_lt": 0.3},
        actions=[act({"falling": True}, caption="Roll chain")])
    add("ground_roll", (ex, 0, S["e2"].hi[2] + 5.0), until={"not_mode": "Roll"},
        actions=[act({"mode_ticks_ge": 8}, release=[C])])
    add("spring_runup", (ex, 0, S["spring_d"].lo[2] - 1.25), reach=0.3, glance=list(T["spring_top"]),
        glance_dist=5.0)
    add("springboard", (ex, S["r3"].hi[1], S["r3"].lo[2] - 0.5), glance=list(T["r3_lip"]),
        until={"mode": "LedgeClimb"},
        actions=[act(tap=[J], expect=SPRING_EXPECT, within_ticks=3, caption="Springboard"),
                 act({"dist_lt": 1.2, "mode": "Ground"}, hold=[J]), act({"mode": "LedgeClimb"}, release=[J])])
    add("r3_top", (ex, S["r3"].hi[1], S["r3"].lo[2] + 0.6), until={"mode": "Ground", "y_gt": S["r3"].hi[1] - 0.2},
        actions=[act(caption="Ledge onto R3")])
    # --- rooftop gaps (orange) ----------------------------------------------------------
    add("r3_run", (ex, S["r3"].hi[1], S["r3"].hi[2] - 1.2), reach=0.4)
    add("coil_gap", (ex, S["r4"].hi[1], S["r4"].lo[2] + 2.0), glance=list(T["r4_landing"]), glance_dist=6.0,
        until={"mode": "Ground", "y_gt": S["r4"].hi[1] - 0.2, "seg_ticks_ge": 4},
        actions=[act(tap=[J], expect="Air", within_ticks=2, caption="Gap 3 m + coil over the bar"),
                 act({"mode": "Air", "mode_ticks_ge": 1}, hold=[C]),
                 act({"mode": "Air", "falling": True, "y_lt": S["r4"].hi[1] + 1.0}, release=[C])])
    add("r4_run", (ex, S["r4"].hi[1], S["r4"].hi[2] - 0.9), reach=0.4)
    add("gap_45", (ex, S["r5"].hi[1], S["r5"].lo[2] + 1.0), glance=list(T["r5_landing"]), glance_dist=6.0,
        until={"mode": "Slide"},
        actions=[act(tap=[J], expect="Air", within_ticks=2, caption="Gap 4.5 m"),
                 act({"mode": "Air", "falling": True, "y_lt": S["r4"].hi[1]}, hold=[C], expect="Slide",
                     within_ticks=20, caption="Land into a slide")])
    add("slide_beam", (ex, S["r5"].hi[1], S["slide_beam"].hi[2] + 0.8),
        actions=[act({"mode": "Slide", "mode_ticks_ge": 2}, hold=[Q], caption="Slide under the beam + fire"),
                 act({"seg_ticks_ge": 12}, release=[Q])])
    vz = c("vault_out", 2)
    add("curve_west", (ex - 2.2, S["r5"].hi[1], vz - 0.6), actions=[act(release=[C, Q])])
    add("vault_out", (S["vault_out"].lo[0] - 1.5, S["r5"].hi[1], vz), glance=list(T["vault_out_top"]),
        until={"mode": "Vault"}, actions=[act(expect="Vault", within_ticks=40, caption="Vault out")])
    add("drop_roll", (S["r5"].lo[0] - 3.0, 0, vz), until={"mode": "Roll"},
        actions=[act({"falling": True, "y_lt": S["r5"].hi[1] - 0.5}, hold=[C], expect="Roll", within_ticks=25,
                     caption="Drop 3.5 m + roll")])
    # --- wall kick (purple): underpass, wallclimb A, quickturn on the wall, kick back onto B ---
    kb, ka = S["kick_b"], S["kick_a"]
    kz, kh = c("kick_b", 2), kb.hi[1]
    add("roll_west", (kb.hi[0], 0, kz), until={"not_mode": "Roll"},
        actions=[act({"mode_ticks_ge": 6}, release=[C])])
    add("underpass", (kb.lo[0], 0, kz), hold_line=True, caption="Underpass")
    # The quickturn fires on entry to wall_kick (same tick), so the view target is already B.
    add("wallclimb_a", (ka.hi[0] + 0.4, 0, kz), hold_line=True, glance=list(T["kick_a_mid"]),
        until={"mode": "WallClimb", "mode_ticks_ge": KICK_TURN_TICKS},
        actions=[act({"dist_lt": 2.6}, hold=[J], expect="WallClimb", within_ticks=16, caption="Wallclimb")])
    add("wall_kick", (kb.lo[0] + 0.6, kh, kz), hold_line=True, glance=list(T["kick_b_lip"]), glance_dist=5.0,
        until={"mode": "LedgeClimb"},
        actions=[act(quickturn=True, release=[J], expect="WallClimb180", within_ticks=2,
                     caption="Quickturn on the wall"),
                 act({"mode": "WallClimb180", "mode_ticks_ge": KICK_JUMP_TICKS}, hold=[J], expect="Air",
                     within_ticks=2, caption="Wall kick"),
                 act({"mode": "Air"}, expect="LedgeClimb", within_ticks=14)])
    add("ledge_b", (kb.lo[0] + 1.0, kh, kz), until={"mode": "Ground", "y_gt": kh - 0.2},
        actions=[act(release=[J], caption="Ledge")])
    # Run across B's top and jump out off its north edge: a visible arc, not a step off.
    add("kick_b_top", (nx - 0.8, kh, kb.lo[2] + 1.6))
    add("kick_b_edge", (nx - 0.4, kh, kb.lo[2] + 0.5), reach=0.4)
    add("drop_kick_b", (nx, 0, kb.lo[2] - 4.0), until={"mode": "Roll"},
        actions=[act(tap=[J], expect="Air", within_ticks=2, caption="Jump off B"),
                 act({"falling": True, "y_lt": kh - 0.4}, hold=[C], expect="Roll", within_ticks=30,
                     caption="Drop 4.75 m + roll")])
    add("roll_turn", (nx, 0, kb.lo[2] - 7.0), until={"not_mode": "Roll"},
        actions=[act({"mode_ticks_ge": 6}, release=[C])])
    # --- north line through the vault garden, then the ascent (yellow) --------------------
    add("v5", (nx, 0, S["v5"].lo[2] - 1.0), glance=list(T["v5_top"]), glance_dist=5.0,
        actions=[act({"dist_lt": 7.0}, expect="Vault", within_ticks=30, caption="Long vault 1.4 m")])
    add("asc_line", (run_asc, 0, S["asc_wall"].hi[2] + 0.8))
    add("asc_wallrun", (run_asc, 1, S["asc_wall"].lo[2] - 0.2), until={"not_mode": "WallRun", "seg_ticks_ge": 6},
        actions=[act(hold=[J], expect="WallRun", within_ticks=6, caption="Ascent: wallrun")])
    add("balcony", (run_asc, S["balc_e"].hi[1], S["balc_e"].hi[2] - 0.6), glance=list(T["balc_lip"]),
        until={"mode": "Ground", "y_gt": S["balc_e"].hi[1] - 0.2},
        actions=[act({"mode": "LedgeClimb"}, release=[J], caption="Wallclimb + ledge 3.5 m")])
    add("deck_climb_s", (run_asc, S["balc_e"].hi[1], S["deck_e"].hi[2] + 0.4), glance=list(T["deck_south_lip"]),
        until={"mode": "Ground", "y_gt": deck - 0.2},
        actions=[act({"dist_lt": 2.4}, hold=[J], expect="WallClimb", within_ticks=16, caption="Wallclimb to the deck"),
                 act({"mode": "LedgeClimb"}, release=[J])])
    add("deck_north", (run_asc, deck, tz + 4.0))
    add("deck_curve", (S["shoulder"].hi[0] + 1.6, deck, tz + 0.4))
    add("shoulder", (S["shoulder"].hi[0] + 0.5, deck, tz), glance=list(T["shoulder_lip"]),
        until={"mode": "Ground", "y_gt": S["shoulder"].hi[1] - 0.2},
        actions=[act({"dist_lt": 1.4}, hold=[J], expect="LedgeClimb", within_ticks=16, caption="Ledge 9.5 m"),
                 act({"mode": "LedgeClimb"}, release=[J])])
    add("tower", (S["tower"].hi[0] - 0.5, S["shoulder"].hi[1], tz), glance=list(T["tower_lip"]),
        until={"mode": "Ground", "y_gt": S["tower"].hi[1] - 0.2},
        actions=[act({"mode": "Ground", "mode_ticks_ge": 2}, hold=[J], expect="LedgeClimb", within_ticks=16,
                     caption="Tower 12 m"),
                 act({"mode": "LedgeClimb"}, release=[J])])
    add("tower_run", (S["tower"].lo[0] + 1.0, S["tower"].hi[1], tz))
    add("tower_drop", (S["tower"].lo[0] - 1.5, S["tower"].hi[1], tz), glance=list(T["tower_west_drop"]),
        glance_dist=6.0, until={"mode": "Roll"},
        actions=[act({"falling": True, "y_lt": S["tower"].hi[1] - 0.4}, hold=[C], expect="Roll", within_ticks=25,
                     caption="Tower drop 5 m + roll")])
    add("sprint_out", (S["tower"].lo[0] - 6.0, deck, tz), until={"not_mode": "Roll"},
        actions=[act({"mode_ticks_ge": 6}, release=[C])])
    add("finale", (-6.0, deck, tz))
    # quickturn on entry; from then on the view keeps facing back east (no turning back)
    add("quickturn", (-8.0, deck, tz), face=[6.0, deck + 1.5, tz], until={"seg_ticks_ge": 20},
        actions=[act(quickturn=True, caption="Quickturn"), act({"seg_ticks_ge": 14}, release=[F])])
    start = (run_b, 0.0, 10.0)
    return {
        "name": "showcase",
        "start": {"pos": list(start), "yaw_deg": 0.0,
                  "move": f"move {-start[2] * 39.37:.1f} {-start[0] * 39.37:.1f} {start[1] * 39.37:.1f} 0 0"},
        "segments": seg,
    }


def test_routes():
    """Movement test scenarios for tools/harness (one autopilot route each, written to
    routes/<name>.json): fixed start pose, inputs on state/tick triggers, so two runs are
    bit-identical. Each act carries a `caption` that the harness uses as a window marker."""
    S = solids_by_name()
    deck = S["deck_e"].hi[1]
    dlane = 0.5 * (S["dv1"].lo[2] + S["dv1"].hi[2])
    kz, kh = 0.5 * (S["kick_b"].lo[2] + S["kick_b"].hi[2]), S["kick_b"].hi[1]
    IN = 39.37

    def route(name, start, facing, segs):
        fx, fz = facing
        yaw = math.degrees(math.atan2(-fx, -fz))
        return name, {
            "name": name,
            "start": {"pos": list(start), "yaw_deg": round(yaw, 2),
                      "move": f"move {-start[2] * IN:.1f} {-start[0] * IN:.1f} {start[1] * IN:.1f} {yaw:.2f} 0"},
            "segments": segs,
        }

    def seg(id_, to, **kw):
        d = {"id": id_, "to": [round(v, 3) for v in to], "max_ticks": 600}
        d.update(kw)
        return d

    def act(cond=None, **kw):
        a = {"if": cond or {}}
        a.update(kw)
        return a

    F, J, C, Q, ADS = "+forward", "+gostand", "+movedown", "+attack", "+speed_throw"
    lane_z = 38.5                        # south ring, outer half: clear 78 m straight
    out = dict([
        # ADS on the ground, then sprint 4 s, slide, stop, standing quickturn.
        route("sprint", (-36.0, 0.0, lane_z), (1, 0), [
            seg("ads", (-35.0, 0, lane_z), hold_line=True, until={"seg_ticks_ge": 30},
                actions=[act(hold=[ADS], caption="ads_go"), act({"seg_ticks_ge": 24}, release=[ADS], caption="ads_done")]),
            seg("run", (36.0, 0, lane_z), hold_line=True, until={"seg_ticks_ge": 150},
                actions=[act(hold=[F], caption="sprint_go"),
                         act({"seg_ticks_ge": 80}, hold=[C], expect="Slide", within_ticks=3, caption="slide_go"),
                         act({"seg_ticks_ge": 108}, release=[C], caption="slide_done"),
                         act({"seg_ticks_ge": 120}, release=[F])]),
            seg("settle", (36.0, 0, lane_z), hold_line=True, until={"seg_ticks_ge": 10}),
            # quickturn on entry; from then on the view keeps facing back down the lane
            seg("quickturn", (36.0, 0, lane_z), face=[-36.0, 1.5, lane_z], until={"seg_ticks_ge": 24},
                actions=[act(quickturn=True, caption="qt_go"), act({"seg_ticks_ge": 22}, caption="qt_done")]),
        ]),
        route("jumps", (-36.0, 0.0, lane_z), (1, 0), [
            seg("stand_jump", (-35.0, 0, lane_z), hold_line=True, until={"seg_ticks_ge": 34},
                actions=[act({"seg_ticks_ge": 2}, tap=[J], expect="Air", within_ticks=3, caption="sjump_go"),
                         act({"seg_ticks_ge": 32}, caption="sjump_done")]),
            seg("run_jump", (30.0, 0, lane_z), hold_line=True, until={"seg_ticks_ge": 100},
                actions=[act(hold=[F]),
                         act({"seg_ticks_ge": 60}, tap=[J], expect="Air", within_ticks=3, caption="rjump_go"),
                         act({"seg_ticks_ge": 92}, caption="rjump_done"), act({"seg_ticks_ge": 94}, release=[F])]),
        ]),
    ])
    # Wallrun along wall A (face +x), northwards, three times: plain, holding ADS, hip fire.
    ax = S["wr_a"].hi[0] + 0.6
    a_south = S["wr_a"].hi[2]
    for name, extra in (("wallrun", []),
                        ("wallrun_ads", [act({"mode": "WallRun", "mode_ticks_ge": 5}, hold=[ADS])]),
                        ("wallrun_fire", [act({"mode": "WallRun", "mode_ticks_ge": 7}, tap=[Q])])):
        n, r = route(name, (ax, 0.0, a_south + 7.0), (0, -1), [
            seg("approach", (ax, 0, a_south - 1.0), hold_line=True, actions=[act(hold=[F])]),
            seg("wallrun", (ax, 1, S["wr_a"].lo[2]), hold_line=True, until={"not_mode": "WallRun", "seg_ticks_ge": 6},
                actions=[act(hold=[J], expect="WallRun", within_ticks=3, caption="wr_go")] + extra),
            seg("after", (ax, 0, S["wr_a"].lo[2] - 0.5), until={"seg_ticks_ge": 4},
                actions=[act(release=[J, ADS, F], caption="wr_done")]),
        ])
        out[n] = r
    # Wallclimb + ledge onto W1 (3.5 m) from the ground.
    wx = -32.0
    out.update(dict([
        route("wallclimb", (wx, 0.0, S["w1"].hi[2] + 1.65), (0, -1), [
            seg("climb", (wx, S["w1"].hi[1], S["w1"].hi[2] - 0.6), hold_line=True,
                until={"mode": "Ground", "y_gt": S["w1"].hi[1] - 0.2, "seg_ticks_ge": 10},
                actions=[act(hold=[F], caption="wc_go"),
                         act({"seg_ticks_ge": 8}, hold=[J], expect="WallClimb", within_ticks=4)]),
            seg("top", (wx, S["w1"].hi[1], S["w1"].hi[2] - 1.0), until={"seg_ticks_ge": 12},
                actions=[act(release=[J, F]), act({"seg_ticks_ge": 10}, caption="wc_done")]),
        ]),
        # Vault over the 0.9 m north-ring hurdle at speed.
        route("vault", (-36.0, 0.0, -35.5), (1, 0), [
            seg("run", (S["hurdle_1"].hi[0] + 6.0, 0, -35.5), hold_line=True,
                actions=[act(hold=[F], caption="v_go"),
                         act({"dist_lt": 9.5}, expect="Vault", within_ticks=30)]),
            seg("after", (S["hurdle_1"].hi[0] + 8.0, 0, -35.5), until={"seg_ticks_ge": 8},
                actions=[act(release=[F]), act({"seg_ticks_ge": 6}, caption="v_done")]),
        ]),
        # Roll: off the deck's east lane edge (3.0 m onto R1), crouch held -> roll, room to run out.
        route("roll", (S["deck_e"].hi[0] - 2.5, deck, dlane), (1, 0), [
            seg("drop", (S["deck_e"].hi[0] + 3.0, 0, dlane), hold_line=True, until={"mode": "Roll"},
                actions=[act(hold=[F], caption="roll_go"),
                         act({"falling": True, "y_lt": deck - 0.5}, hold=[C], expect="Roll", within_ticks=25)]),
            seg("out", (S["deck_e"].hi[0] + 9.0, 0, dlane), hold_line=True, until={"not_mode": "Roll", "seg_ticks_ge": 4},
                actions=[act({"mode_ticks_ge": 8}, release=[C])]),
            seg("rest", (S["deck_e"].hi[0] + 12.0, 0, dlane), hold_line=True, until={"seg_ticks_ge": 16},
                actions=[act({"seg_ticks_ge": 12}, release=[F]), act({"seg_ticks_ge": 14}, caption="roll_done")]),
        ]),
        # Wall kick: west through B's underpass, wallclimb A, quickturn on the wall, jump off
        # backwards, ledge onto B (4.75 m).
        route("wall_turn", (S["kick_b"].hi[0] + 6.0, 0.0, kz), (-1, 0), [
            seg("run", (S["kick_b"].lo[0], 0, kz), hold_line=True, actions=[act(hold=[F], caption="wt_go")]),
            seg("climb", (S["kick_a"].hi[0] + 0.4, 0, kz), hold_line=True,
                until={"mode": "WallClimb", "mode_ticks_ge": KICK_TURN_TICKS},
                actions=[act({"dist_lt": 2.6}, hold=[J], expect="WallClimb", within_ticks=16)]),
            seg("kick", (S["kick_b"].lo[0] + 0.6, kh, kz), hold_line=True, until={"mode": "LedgeClimb"},
                actions=[act(quickturn=True, release=[J], expect="WallClimb180", within_ticks=2, caption="wt_turn"),
                         act({"mode": "WallClimb180", "mode_ticks_ge": KICK_JUMP_TICKS}, hold=[J], expect="Air",
                             within_ticks=2, caption="wt_kick"),
                         act({"mode": "Air"}, expect="LedgeClimb", within_ticks=14)]),
            seg("top", (S["kick_b"].lo[0] + 1.0, kh, kz), until={"mode": "Ground", "y_gt": kh - 0.2},
                actions=[act(release=[J])]),
            seg("rest", (S["kick_b"].lo[0] + 1.5, kh, kz), until={"seg_ticks_ge": 8},
                actions=[act(release=[F]), act({"seg_ticks_ge": 6}, caption="wt_done")]),
        ]),
        # Hard landing: off the deck's north drop gap (7.0 m) without crouch.
        route("hard", (10.0, deck, -33.65), (0, -1), [
            seg("drop", (10.0, 0, -38.0), hold_line=True, until={"mode": "HardLanding"},
                actions=[act(hold=[F], caption="hard_go"),
                         act({"falling": True, "y_lt": deck - 0.5}, expect="HardLanding", within_ticks=25)]),
            seg("stun", (10.0, 0, -39.0), until={"seg_ticks_ge": 14},
                actions=[act(release=[F]), act({"seg_ticks_ge": 12}, caption="hard_done")]),
        ]),
    ]))
    return out


# ----------------------------------------------------------------------------- tessellation

AXES = {"x": 0, "y": 1, "z": 2}


def face_list(s):
    """(dir, axis k, coord c, outward sign, (a, a0, a1), (b, b0, b1)) for the solid's planar faces,
    plus ramp extras returned separately."""
    lo, hi = s.lo, s.hi
    faces = []
    for k, ax in enumerate("xyz"):
        others = [i for i in range(3) if i != k]
        a, b = others
        for sign, c in ((-1, lo[k]), (1, hi[k])):
            d = ("+" if sign > 0 else "-") + ax
            if s.kind == "ramp":
                # the top (+y) is the slope; the low end has no face; sides are triangles
                if d == "+y":
                    continue
                if k == s.axis and ((sign > 0) != (s.sgn > 0)):
                    continue
                if k != 1 and k != s.axis:
                    continue  # side triangles handled separately
            faces.append((d, k, c, sign, (a, lo[a], hi[a]), (b, lo[b], hi[b])))
    return faces


def cuts(a0, a1, step, extra=()):
    n = max(1, int(math.ceil((a1 - a0) / step - 1e-6)))
    vals = {round(a0 + (a1 - a0) * i / n, 4) for i in range(n + 1)}
    for e in extra:
        if a0 + 1e-3 < e < a1 - 1e-3:
            vals.add(round(e, 4))
    return sorted(vals)


def lip_covers(s, d, along):
    for ld, rng in s.lips:
        if ld == "all" or ld == d:
            if rng is None or rng[0] - 1e-3 <= along <= rng[1] + 1e-3:
                return True
    return False


def paint(s, d, p):
    """Material of a face cell centred at p on face direction d."""
    if s.red_all:
        return RED
    lo, hi = s.lo, s.hi
    if d == "+y" or d == "slope":
        if s.red_top:
            return RED
        for ld, rng in s.lips:
            for dd in (("-x", "+x", "-z", "+z") if ld == "all" else (ld,)):
                k = 0 if dd[1] == "x" else 2
                edge = hi[k] if dd[0] == "+" else lo[k]
                along = p[2] if k == 0 else p[0]
                if abs(p[k] - edge) < 0.3 and (rng is None or rng[0] <= along <= rng[1]) and ld != "all":
                    return RED
                if ld == "all" and abs(p[k] - edge) < 0.3:
                    return RED
        return s.top
    if d == "-y":
        return s.body
    k = 0 if d[1] == "x" else 2
    along = p[2] if k == 0 else p[0]
    if lip_covers(s, d, along) and p[1] > hi[1] - 0.15:
        return RED
    if s.stripe:
        sd, y0, y1, mat = s.stripe
        if (sd == d or sd == "in") and y0 <= p[1] - lo[1] <= y1:
            if sd != "in" or s.name.startswith("bound"):
                return mat
    if s.skirt and p[1] - lo[1] < s.skirt[1]:
        return s.skirt[0]
    return s.body


def band_cuts(s, d, k_axis_b_is_y):
    """Extra cut heights (absolute y) for vertical faces."""
    out = [s.hi[1] - 0.15]
    if s.skirt:
        out.append(s.lo[1] + s.skirt[1])
    if s.stripe:
        out += [s.lo[1] + s.stripe[1], s.lo[1] + s.stripe[2]]
    return out


class Builder:
    def __init__(self):
        self.verts = []      # [pos, normal, uv(mat scale applied later), mat]
        self.tris = {}       # mat -> list of (i, j, k)
        self.vkey = {}

    def vertex(self, p, n, mat, key):
        kk = (key, mat)
        if kk in self.vkey:
            return self.vkey[kk]
        self.verts.append((p, n, mat))
        self.vkey[kk] = len(self.verts) - 1
        return len(self.verts) - 1

    def add_tri(self, mat, i, j, k, n):
        a, b, c = self.verts[i][0], self.verts[j][0], self.verts[k][0]
        u = (b[0] - a[0], b[1] - a[1], b[2] - a[2])
        v = (c[0] - a[0], c[1] - a[1], c[2] - a[2])
        cr = (u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0])
        area2 = cr[0] ** 2 + cr[1] ** 2 + cr[2] ** 2
        if area2 < 1e-10:
            return
        if cr[0] * n[0] + cr[1] * n[1] + cr[2] * n[2] < 0:
            j, k = k, j
        self.tris.setdefault(mat, []).append((i, j, k))


def hidden(p, n, owner, near):
    q = (p[0] + n[0] * 0.02, p[1] + n[1] * 0.02, p[2] + n[2] * 0.02)
    for s in near:
        if s is not owner and s.inside(q, eps=1e-4):
            return True
    return False


def aabb_hit(s, lo, hi, pad=0.05):
    return (s.lo[0] - pad <= hi[0] and s.hi[0] + pad >= lo[0] and s.lo[1] - pad <= hi[1] and s.hi[1] + pad >= lo[1]
            and s.lo[2] - pad <= hi[2] and s.hi[2] + pad >= lo[2])


def tessellate(solids):
    bld = Builder()
    face_id = 0
    for s in solids:
        if not s.render:
            continue
        for (d, k, c, sign, (a, a0, a1), (bb, b0, b1)) in face_list(s):
            if s.name == "floor" and d != "+y":
                continue
            if d == "-y" and c <= 0.0:
                continue  # resting on the floor: never visible
            n = [0.0, 0.0, 0.0]
            n[k] = float(sign)
            face_id += 1
            ea, eb = [], []
            if k != 1:  # vertical face: b or a is y
                ys = band_cuts(s, d, True)
                if a == 1:
                    ea = ys
                else:
                    eb = ys
            else:
                for ld, rng in s.lips:
                    for dd in (("-x", "+x", "-z", "+z") if ld == "all" else (ld,)):
                        kk = 0 if dd[1] == "x" else 2
                        edge = s.hi[kk] if dd[0] == "+" else s.lo[kk]
                        val = edge - 0.3 if dd[0] == "+" else edge + 0.3
                        if kk == a:
                            ea.append(val)
                        else:
                            eb.append(val)
                        if rng is not None:
                            if kk == a:
                                eb += list(rng)
                            else:
                                ea += list(rng)
                if s.name == "floor":
                    ea += [-H for H in (HALF,)] + [HALF]
                    eb += [-HALF, HALF]
            ca = cuts(a0, a1, s.step, ea)
            cb = cuts(b0, b1, s.step, eb)
            flo = [0, 0, 0]
            fhi = [0, 0, 0]
            flo[k] = fhi[k] = c
            flo[a], fhi[a], flo[bb], fhi[bb] = a0, a1, b0, b1
            near = [o for o in solids if o is not s and aabb_hit(o, flo, fhi)]

            def P(u, v):
                p = [0.0, 0.0, 0.0]
                p[k], p[a], p[bb] = c, u, v
                return tuple(p)

            for i in range(len(ca) - 1):
                for j in range(len(cb) - 1):
                    u0, u1, v0, v1 = ca[i], ca[i + 1], cb[j], cb[j + 1]
                    # Refine the cell along the outlines of solids that touch it, so hidden-cell
                    # culling never opens a hole next to a thin obstacle.
                    su, sv = {u0, u1}, {v0, v1}
                    for o in near:
                        if o.lo[a] < u1 and o.hi[a] > u0 and o.lo[bb] < v1 and o.hi[bb] > v0:
                            for e in (o.lo[a], o.hi[a]):
                                if u0 + 1e-3 < e < u1 - 1e-3:
                                    su.add(round(e, 4))
                            for e in (o.lo[bb], o.hi[bb]):
                                if v0 + 1e-3 < e < v1 - 1e-3:
                                    sv.add(round(e, 4))
                    su, sv = sorted(su), sorted(sv)
                    for ii in range(len(su) - 1):
                        for jj in range(len(sv) - 1):
                            cen = P((su[ii] + su[ii + 1]) / 2, (sv[jj] + sv[jj + 1]) / 2)
                            if hidden(cen, n, s, near):
                                continue
                            mat = paint(s, d, cen)
                            q = [bld.vertex(P(su[ii + di], sv[jj + dj]), tuple(n), mat,
                                            (face_id, round(su[ii + di], 4), round(sv[jj + dj], 4)))
                                 for di, dj in ((0, 0), (1, 0), (1, 1), (0, 1))]
                            bld.add_tri(mat, q[0], q[1], q[2], n)
                            bld.add_tri(mat, q[0], q[2], q[3], n)
        if s.kind == "ramp":
            face_id = tess_ramp(bld, s, solids, face_id)
    return bld


def tess_ramp(bld, s, solids, face_id):
    ax = s.axis
    other = 2 if ax == 0 else 0
    lo, hi = s.lo, s.hi
    near = [o for o in solids if o is not s and aabb_hit(o, lo, hi)]
    # slope: grid over the footprint
    ca = cuts(lo[ax], hi[ax], s.step)
    cb = cuts(lo[other], hi[other], s.step, [lo[other] + 0.3, hi[other] - 0.3])
    n = s.plane_n
    face_id += 1

    def P(u, v):
        p = [0.0, 0.0, 0.0]
        p[ax], p[other] = u, v
        p[1] = s.height_at(p[0], p[2])
        return tuple(p)

    for i in range(len(ca) - 1):
        for j in range(len(cb) - 1):
            cen = P((ca[i] + ca[i + 1]) / 2, (cb[j] + cb[j + 1]) / 2)
            if hidden(cen, n, s, near):
                continue
            mat = RED if (cen[other] < lo[other] + 0.3 or cen[other] > hi[other] - 0.3) else s.top
            q = [bld.vertex(P(ca[i + di], cb[j + dj]), n, mat, (face_id, i + di, j + dj))
                 for di, dj in ((0, 0), (1, 0), (1, 1), (0, 1))]
            bld.add_tri(mat, q[0], q[1], q[2], n)
            bld.add_tri(mat, q[0], q[2], q[3], n)
    # side triangles: (t along the rise, s up to the slope)
    for sign, cside in ((-1, lo[other]), (1, hi[other])):
        face_id += 1
        nn = [0.0, 0.0, 0.0]
        nn[other] = float(sign)
        nn = tuple(nn)
        ts = cuts(lo[ax], hi[ax], s.step)
        ns = 4

        def Q(u, f):
            p = [0.0, 0.0, 0.0]
            p[ax], p[other] = u, cside
            top = s.height_at(p[0], p[2]) if ax == 0 else s.height_at(p[0], p[2])
            p[1] = lo[1] + f * (top - lo[1])
            return tuple(p)

        for i in range(len(ts) - 1):
            for j in range(ns):
                cen = Q((ts[i] + ts[i + 1]) / 2, (j + 0.5) / ns)
                if hidden(cen, nn, s, near):
                    continue
                mat = s.body
                q = [bld.vertex(Q(ts[i + di], (j + dj) / ns), nn, mat, (face_id, i + di, j + dj))
                     for di, dj in ((0, 0), (1, 0), (1, 1), (0, 1))]
                bld.add_tri(mat, q[0], q[1], q[2], nn)
                bld.add_tri(mat, q[0], q[2], q[3], nn)
    return face_id


# ----------------------------------------------------------------------------- light bake


def ray_solid(s, o, d, tmax):
    t0, t1 = 1e-4, tmax
    for k in range(3):
        if abs(d[k]) < 1e-12:
            if o[k] <= s.lo[k] or o[k] >= s.hi[k]:
                return False
        else:
            inv = 1.0 / d[k]
            ta, tb = (s.lo[k] - o[k]) * inv, (s.hi[k] - o[k]) * inv
            if ta > tb:
                ta, tb = tb, ta
            if ta > t0:
                t0 = ta
            if tb < t1:
                t1 = tb
            if t0 > t1:
                return False
    if s.kind == "ramp":
        n = s.plane_n
        den = n[0] * d[0] + n[1] * d[1] + n[2] * d[2]
        num = s.plane_d - (n[0] * o[0] + n[1] * o[1] + n[2] * o[2])
        if abs(den) < 1e-12:
            return num > 0
        tp = num / den
        if den < 0:
            t0 = max(t0, tp)
        else:
            t1 = min(t1, tp)
        return t0 <= t1
    return True


def hemisphere(n_dirs=20):
    """Cosine-weighted directions about +Y (Hammersley), deterministic."""
    out = []
    for i in range(n_dirs):
        u = (i + 0.5) / n_dirs
        bits, v, f = i, 0.0, 0.5
        while bits:
            v += f * (bits & 1)
            bits >>= 1
            f *= 0.5
        v = (v + 0.37) % 1.0
        r = math.sqrt(u)
        phi = 2 * math.pi * v
        out.append((r * math.cos(phi), math.sqrt(max(0.0, 1 - u)), r * math.sin(phi)))
    return out


def frame(n):
    t = (1.0, 0.0, 0.0) if abs(n[0]) < 0.9 else (0.0, 0.0, 1.0)
    bx = (n[1] * t[2] - n[2] * t[1], n[2] * t[0] - n[0] * t[2], n[0] * t[1] - n[1] * t[0])
    ln = math.sqrt(sum(c * c for c in bx))
    bx = tuple(c / ln for c in bx)
    bz = (n[1] * bx[2] - n[2] * bx[1], n[2] * bx[0] - n[0] * bx[2], n[0] * bx[1] - n[1] * bx[0])
    return bx, bz


class Grid:
    def __init__(self, solids, cell=4.0):
        self.cell, self.map = cell, {}
        for s in solids:
            if s.name == "floor":
                continue  # nothing is below the floor top
            for i in range(int(math.floor(s.lo[0] / cell)), int(math.floor(s.hi[0] / cell)) + 1):
                for j in range(int(math.floor(s.lo[2] / cell)), int(math.floor(s.hi[2] / cell)) + 1):
                    self.map.setdefault((i, j), []).append(s)

    def query(self, lo, hi):
        c = self.cell
        out, seen = [], set()
        for i in range(int(math.floor(lo[0] / c)), int(math.floor(hi[0] / c)) + 1):
            for j in range(int(math.floor(lo[2] / c)), int(math.floor(hi[2] / c)) + 1):
                for s in self.map.get((i, j), ()):
                    if id(s) not in seen and s.hi[1] >= lo[1] and s.lo[1] <= hi[1]:
                        seen.add(id(s))
                        out.append(s)
        return out


AO_RANGE = 9.0
SKY_MAX = 0.85   # open sky stays below the lightmap peak: white walls keep their texture


def bake(bld, solids):
    grid = Grid(solids)
    dirs = hemisphere(20)
    sun = tuple(-c for c in SUN_DIR)
    ls = math.sqrt(sum(c * c for c in sun))
    sun = tuple(c / ls for c in sun)
    top = max(s.hi[1] for s in solids) + 0.5
    colors = []
    for p, n, _mat in bld.verts:
        o = (p[0] + n[0] * 0.03, p[1] + n[1] * 0.03, p[2] + n[2] * 0.03)
        lo = (o[0] - AO_RANGE, o[1] - AO_RANGE, o[2] - AO_RANGE)
        hi = (o[0] + AO_RANGE, o[1] + AO_RANGE, o[2] + AO_RANGE)
        cand = grid.query(lo, hi)
        bx, bz = frame(n)
        free = 0
        for (dx, dy, dz) in dirs:
            d = (bx[0] * dx + n[0] * dy + bz[0] * dz, bx[1] * dx + n[1] * dy + bz[1] * dz, bx[2] * dx + n[2] * dy + bz[2] * dz)
            if d[1] < -1e-3 and o[1] / -d[1] < AO_RANGE:
                continue  # the floor (not in the grid) is closer than the AO range
            blocked = False
            for s in cand:
                if ray_solid(s, o, d, AO_RANGE):
                    blocked = True
                    break
            if not blocked:
                free += 1
        sky = free / len(dirs)
        ndl = n[0] * sun[0] + n[1] * sun[1] + n[2] * sun[2]
        lit = 0.0
        if ndl > 0.02:
            L = (top - o[1]) / sun[1]
            end = (o[0] + sun[0] * L, top, o[2] + sun[2] * L)
            slo = (min(o[0], end[0]), o[1], min(o[2], end[2]))
            shi = (max(o[0], end[0]), top, max(o[2], end[2]))
            lit = 1.0
            for s in grid.query(slo, shi):
                if ray_solid(s, o, sun, L):
                    lit = 0.0
                    break
        colors.append((sky * SKY_MAX, lit))
    return colors


# ----------------------------------------------------------------------------- glb


def write_glb(path, prims, textures):
    """prims: list of dicts {pos, nrm, uv?, col?, idx, material?}. textures: [(name, png)]."""
    blob = bytearray()
    views, accessors = [], []

    def push(data, target=None):
        while len(blob) % 4:
            blob.append(0)
        view = {"buffer": 0, "byteOffset": len(blob), "byteLength": len(data)}
        if target:
            view["target"] = target
        blob.extend(data)
        views.append(view)
        return len(views) - 1

    meshes, nodes = [], []
    for i, pr in enumerate(prims):
        pos = pr["pos"]
        lo = [min(p[a] for p in pos) for a in range(3)]
        hi = [max(p[a] for p in pos) for a in range(3)]
        attrs = {}
        accessors.append({"bufferView": push(b"".join(struct.pack("<3f", *p) for p in pos), 34962),
                          "componentType": 5126, "count": len(pos), "type": "VEC3", "min": lo, "max": hi})
        attrs["POSITION"] = len(accessors) - 1
        if "nrm" in pr:
            accessors.append({"bufferView": push(b"".join(struct.pack("<3f", *n) for n in pr["nrm"]), 34962),
                              "componentType": 5126, "count": len(pos), "type": "VEC3"})
            attrs["NORMAL"] = len(accessors) - 1
        if "uv" in pr:
            accessors.append({"bufferView": push(b"".join(struct.pack("<2f", *t) for t in pr["uv"]), 34962),
                              "componentType": 5126, "count": len(pos), "type": "VEC2"})
            attrs["TEXCOORD_0"] = len(accessors) - 1
        if "col" in pr:
            accessors.append({"bufferView": push(b"".join(struct.pack("<4B", *c) for c in pr["col"]), 34962),
                              "componentType": 5121, "normalized": True, "count": len(pos), "type": "VEC4"})
            attrs["COLOR_0"] = len(accessors) - 1
        idx = pr["idx"]
        big = len(pos) > 65535
        accessors.append({"bufferView": push(b"".join(struct.pack("<I" if big else "<H", k) for k in idx), 34963),
                          "componentType": 5125 if big else 5123, "count": len(idx), "type": "SCALAR"})
        prim = {"attributes": attrs, "indices": len(accessors) - 1}
        if pr.get("material") is not None:
            prim["material"] = pr["material"]
        meshes.append({"name": pr.get("name", f"m{i}"), "primitives": [prim]})
        nodes.append({"mesh": i, "name": pr.get("name", f"m{i}")})

    images, materials, gltf_textures = [], [], []
    for name, png in textures:
        images.append({"bufferView": push(png), "mimeType": "image/png", "name": name})
        gltf_textures.append({"source": len(images) - 1, "sampler": 0})
        materials.append({"name": name, "pbrMetallicRoughness": {"baseColorTexture": {"index": len(gltf_textures) - 1},
                                                                  "baseColorFactor": [1, 1, 1, 1], "metallicFactor": 0.0,
                                                                  "roughnessFactor": 0.9}})
    doc = {
        "asset": {"version": "2.0", "generator": "iw4L mec-testbox.py (Parkour Playground)"},
        "scene": 0,
        "scenes": [{"nodes": list(range(len(nodes)))}],
        "nodes": nodes,
        "meshes": meshes,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": len(blob)}],
    }
    if materials:
        doc.update(images=images, textures=gltf_textures, materials=materials,
                   samplers=[{"magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497}])
    text = json.dumps(doc, separators=(",", ":")).encode()
    while len(text) % 4:
        text += b" "
    while len(blob) % 4:
        blob.append(0)
    out = struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(text) + 8 + len(blob))
    out += struct.pack("<II", len(text), 0x4E4F534A) + text
    out += struct.pack("<II", len(blob), 0x004E4942) + bytes(blob)
    path.write_bytes(out)


def solid_tris(s):
    """Closed outward triangles of a solid (glTF CCW from outside)."""
    x0, y0, z0 = s.lo
    x1, y1, z1 = s.hi
    if s.kind == "box":
        quads = [
            [(x0, y1, z0), (x0, y1, z1), (x1, y1, z1), (x1, y1, z0)],
            [(x0, y0, z0), (x1, y0, z0), (x1, y0, z1), (x0, y0, z1)],
            [(x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1)],
            [(x1, y0, z0), (x0, y0, z0), (x0, y1, z0), (x1, y1, z0)],
            [(x1, y0, z1), (x1, y0, z0), (x1, y1, z0), (x1, y1, z1)],
            [(x0, y0, z0), (x0, y0, z1), (x0, y1, z1), (x0, y1, z0)],
        ]
        tris = []
        for q in quads:
            tris += [(q[0], q[1], q[2]), (q[0], q[2], q[3])]
        return tris
    # wedge: 6 corners
    corners = [(x, y0, z) for x in (x0, x1) for z in (z0, z1)]
    tops = [(x, s.height_at(x, z), z) for x in (x0, x1) for z in (z0, z1)]
    pts = corners + [t for t in tops if t[1] > y0 + 1e-6]
    cx = sum(p[0] for p in pts) / len(pts)
    cy = sum(p[1] for p in pts) / len(pts)
    cz = sum(p[2] for p in pts) / len(pts)
    faces = [corners, [t for t in tops]]  # bottom, slope
    # high end
    ax = s.axis
    hv = s.hi[ax] if s.sgn > 0 else s.lo[ax]
    faces.append([p for p in corners + tops if abs(p[ax] - hv) < 1e-6 and p not in faces[0][:0]])
    other = 2 if ax == 0 else 0
    for ov in (s.lo[other], s.hi[other]):
        faces.append([p for p in corners + tops if abs(p[other] - ov) < 1e-6])
    tris = []
    for f in faces:
        uniq = []
        for p in f:
            if p not in uniq:
                uniq.append(p)
        if len(uniq) < 3:
            continue
        # order around the face centroid
        fc = [sum(p[i] for p in uniq) / len(uniq) for i in range(3)]
        # face normal ~ outward from solid centroid
        nrm = [fc[0] - cx, fc[1] - cy, fc[2] - cz]
        bx, bz = frame(tuple(c / (math.sqrt(sum(v * v for v in nrm)) or 1) for c in nrm))
        uniq.sort(key=lambda p: math.atan2(sum((p[i] - fc[i]) * bz[i] for i in range(3)),
                                           sum((p[i] - fc[i]) * bx[i] for i in range(3))))
        for i in range(1, len(uniq) - 1):
            a, b, c = uniq[0], uniq[i], uniq[i + 1]
            u = [b[k] - a[k] for k in range(3)]
            v = [c[k] - a[k] for k in range(3)]
            cr = (u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0])
            if sum(cr[k] * nrm[k] for k in range(3)) < 0:
                b, c = c, b
            tris.append((a, b, c))
    return tris


# ----------------------------------------------------------------------------- preview

FONT = {
    "0": ["111", "101", "101", "101", "111"], "1": ["010", "110", "010", "010", "111"],
    "2": ["111", "001", "111", "100", "111"], "3": ["111", "001", "111", "001", "111"],
    "4": ["101", "101", "111", "001", "001"], "5": ["111", "100", "111", "001", "111"],
    "6": ["111", "100", "111", "101", "111"], "7": ["111", "001", "010", "010", "010"],
    "8": ["111", "101", "111", "101", "111"], "9": ["111", "101", "111", "001", "111"],
    "N": ["101", "111", "111", "111", "101"],
}
MAT_RGB = {FLOOR: (182, 184, 186), WHITE: (214, 216, 218), GREY: (168, 171, 175), BOUND: (110, 114, 120),
           RED: (206, 42, 34), ORANGE: (226, 128, 38), BLUE: (52, 118, 200), GREEN: (70, 168, 98),
           YELLOW: (232, 186, 40), PURPLE: (140, 78, 196)}


def preview(path, solids, spawns, ppm=6):
    size = int(2 * (HALF + 1) * ppm)
    grid = Grid(solids)
    sun = tuple(-c for c in SUN_DIR)
    ls = math.sqrt(sum(c * c for c in sun))
    sun = tuple(c / ls for c in sun)
    hmap = [[0.0] * size for _ in range(size)]
    img = [[(0, 0, 0)] * size for _ in range(size)]
    for py in range(size):
        z = -HALF - 1 + (py + 0.5) / ppm
        for px in range(size):
            x = -HALF - 1 + (px + 0.5) / ppm
            best, bs = 0.0, None
            for s in grid.query((x, -1, z), (x, 99, z)):
                if s.lo[0] <= x <= s.hi[0] and s.lo[2] <= z <= s.hi[2]:
                    h = s.height_at(x, z)
                    if h > best:
                        best, bs = h, s
            hmap[py][px] = best
            mat = paint(bs, "+y", (x, best, z)) if bs else FLOOR
            if bs is not None and bs.kind == "ramp":
                mat = bs.top
            r, g, b = MAT_RGB[mat]
            o = (x, best + 0.05, z)
            L = (20 - o[1]) / sun[1]
            shade = 1.0
            for s in grid.query((min(x, x + sun[0] * L), 0, min(z, z + sun[2] * L)), (max(x, x + sun[0] * L), 20, max(z, z + sun[2] * L))):
                if ray_solid(s, o, sun, L):
                    shade = 0.62
                    break
            k = shade * (0.78 + 0.022 * best)
            img[py][px] = (clamp8(r * k), clamp8(g * k), clamp8(b * k))
    # height-change outlines
    for py in range(1, size - 1):
        for px in range(1, size - 1):
            h = hmap[py][px]
            if max(abs(h - hmap[py][px + 1]), abs(h - hmap[py + 1][px])) > 0.2:
                img[py][px] = tuple(clamp8(c * 0.35) for c in img[py][px])

    def dot(cx, cy, rad, col):
        for yy in range(int(cy - rad), int(cy + rad) + 1):
            for xx in range(int(cx - rad), int(cx + rad) + 1):
                if 0 <= xx < size and 0 <= yy < size and (xx - cx) ** 2 + (yy - cy) ** 2 <= rad * rad:
                    img[yy][xx] = col

    def text(cx, cy, s, col=(20, 20, 20), scale=2):
        w = len(s) * 4 * scale
        x0, y0 = int(cx - w / 2), int(cy - 5 * scale / 2)
        for yy in range(-1, 5 * scale + 1):
            for xx in range(-1, w):
                if 0 <= x0 + xx < size and 0 <= y0 + yy < size:
                    img[y0 + yy][x0 + xx] = (250, 250, 250)
        for ci, ch in enumerate(s):
            for ry, row in enumerate(FONT[ch]):
                for rx, bit in enumerate(row):
                    if bit == "1":
                        for sy in range(scale):
                            for sx in range(scale):
                                xx, yy = x0 + ci * 4 * scale + rx * scale + sx, y0 + ry * scale + sy
                                if 0 <= xx < size and 0 <= yy < size:
                                    img[yy][xx] = col

    to_px = lambda x, z: ((x + HALF + 1) * ppm, (z + HALF + 1) * ppm)
    for sp in spawns:
        x, _y, z = sp["origin"]
        cx, cy = to_px(x, z)
        yaw = math.radians(sp["yaw_deg"])
        dx, dz = -math.sin(yaw), -math.cos(yaw)
        for t in range(0, 4 * ppm):
            xx, yy = int(cx + dx * t * 0.5), int(cy + dz * t * 0.5)
            if 0 <= xx < size and 0 <= yy < size:
                img[yy][xx] = (255, 255, 255)
        dot(cx, cy, ppm * 0.7, (255, 255, 255))
        dot(cx, cy, ppm * 0.45, (230, 30, 160))
    for label, _t, (x, z) in FEATURES:
        cx, cy = to_px(x, z)
        text(cx, cy, label)
    text(size / 2, 4 * ppm / 2 + 4, "N", scale=3)
    rows = [bytes(c for px in row for c in (*px, 255)) for row in img]
    Path(path).write_bytes(png_rgba(size, size, lambda x, y: rows[y][4 * x:4 * x + 4]))


# ----------------------------------------------------------------------------- spawns


def spawn(x, y, z, face_x, face_z):
    yaw = math.degrees(math.atan2(-face_x, -face_z))
    return {"origin": [x, round(y + 0.05, 3), z], "yaw_deg": round(yaw, 1)}


SPAWNS = [
    spawn(-14, 0, 2, 1, 0),        # plaza west, facing east
    spawn(15.5, 0, 2, -1, 0),      # plaza east, facing west
    spawn(0, 0, 11, 0, -1),        # plaza south, facing north
    spawn(-12, 0, 33, 0, -1),      # garden south, facing north
    spawn(18, 0, 30, -0.4, -1),    # south-east ground
    spawn(-37, 0, -2, 1, 0),       # west ring, facing east
    spawn(37, 0, -4.75, -1, 0),    # east ring, looking down the R2/R3 alley
    spawn(-15, 7.0, -32, 0.6, 1),  # deck west
    spawn(12, 7.0, -18, -0.2, 1),  # deck east
    spawn(28, 4.0, -28, -1, 1),    # R1 roof
    spawn(-28, 3.5, -24, 1, 1),    # W1 roof
    spawn(28, 4.0, 6.0, -1, 0),    # R3 roof
    spawn(-27.7, 1.6, -19.5, 0, 1),  # L1 ledge block, looking down the alley
    spawn(9.5, 0, 34.2, -1, 0),    # south, behind the wall-kick block
]


def main():
    args = [a for a in sys.argv[1:]]
    prev = None
    if "--preview" in args:
        i = args.index("--preview")
        prev = args[i + 1]
        del args[i:i + 2]
    root = Path(os.environ.get("IW4L_MEC_ARENAS", Path(__file__).resolve().parents[2] / "mec-arenas"))
    out = Path(args[0]) if args else root / "testbox"
    out.mkdir(parents=True, exist_ok=True)

    SOLIDS.clear()
    FEATURES.clear()
    build()
    for sp in SPAWNS:
        p = (sp["origin"][0], sp["origin"][1] + 0.9, sp["origin"][2])
        clash = [s.name for s in SOLIDS if s.inside(p)]
        if clash:
            raise SystemExit(f"spawn {sp} inside {clash}")

    bld = tessellate(SOLIDS)
    colors = bake(bld, SOLIDS)

    textures = [(name, make()) for name, make, _ in MATERIALS]
    prims = []
    for mat in sorted(bld.tris):
        scale = 1.0 / MATERIALS[mat][2]
        remap, pos, nrm, uv, col, idx = {}, [], [], [], [], []
        for tri in bld.tris[mat]:
            for v in tri:
                if v not in remap:
                    remap[v] = len(pos)
                    p, n, _ = bld.verts[v]
                    pos.append(p)
                    nrm.append(n)
                    # planar UVs on the two axes the face spans (slopes: the footprint)
                    k = max(range(3), key=lambda a: abs(n[a]))
                    ua, ub = [a for a in range(3) if a != k]
                    uv.append((p[ua] * scale, -p[ub] * scale if ub == 1 else p[ub] * scale))
                    sky, sun = colors[v]
                    col.append((clamp8(sky * 255), clamp8(sky * 255), clamp8(sky * 255), clamp8(sun * 255)))
                idx.append(remap[v])
        prims.append({"name": MATERIALS[mat][0], "pos": pos, "nrm": nrm, "uv": uv, "col": col, "idx": idx, "material": mat})
    write_glb(out / "arena.glb", prims, textures)

    cpos, cidx = [], []
    for s in SOLIDS:
        for a, b, c in solid_tris(s):
            base = len(cpos)
            cpos += [a, b, c]
            cidx += [base, base + 1, base + 2, base, base + 2, base + 1]  # both windings
    write_glb(out / "collision.glb", [{"name": "collision", "pos": cpos, "idx": cidx}], [])

    meta = {
        "name": out.name,
        "title": TITLE,
        "spawns": SPAWNS,
        "bounds_min": [-HALF - 1, -0.5, -HALF - 1],
        "bounds_max": [HALF + 1, WALL_H, HALF + 1],
        "play_min": [-HALF, -3.0, -HALF],
        "play_max": [HALF, 24.0, HALF],
        "sun_dir": list(SUN_DIR),
    }
    (out / "arena.json").write_text(json.dumps(meta, indent=2))
    (out / "route.json").write_text(json.dumps(showcase_route(), indent=1))
    (out / "routes").mkdir(exist_ok=True)
    for name, r in test_routes().items():
        (out / "routes" / f"{name}.json").write_text(json.dumps(r, indent=1))
    ntri = sum(len(t) for t in bld.tris.values())
    print(f"wrote {out}: {len(SOLIDS)} solids, {len(bld.verts)} vertices, {ntri} render tris, "
          f"{len(cidx) // 3} collision tris, {len(SPAWNS)} spawns")
    if prev:
        preview(prev, SOLIDS, SPAWNS)
        print(f"preview {prev}")


if __name__ == "__main__":
    main()
