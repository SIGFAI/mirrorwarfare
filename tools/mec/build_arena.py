# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Build an iw4L "arena package" from a Mirror's Edge Catalyst data dump.

    uv run --python 3.13 tools/mec/build_arena.py --name mec_anchor_1 --center 430 330 --size 160

--center is in *Frostbite* world coordinates (x, z) of SP_MainCity.  Output
(<out>/<name>/): arena.glb, collision.glb, arena.json, preview.png, stats.json.

Coordinate conversion.  MEC mesh + world data behave as RIGHT-handed, +Y up, metres, with
counter-clockwise front faces (cross(b-a, c-a) . vertexNormal > 0 for ~100% of triangles, and the
UV-orientation test -- non-mirrored texturing gives det(dUV) < 0 for CCW faces -- holds for ~67-80% of
textured area only WITHOUT a Z mirror; with a Z mirror shop signs read backwards).  So the default is
    gl = (x - cx, y, z - cz)          (recentre XZ on the region centre, no mirror)
and winding is reversed only for mirrored instances (det(M3x3) < 0).  --mirror-z restores the
"Frostbite is left-handed" conversion gl = (x - cx, y, -(z - cz)) with every winding reversed.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
import time
from collections import defaultdict

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from fbdata import DEFAULT_ARENAS, DEFAULT_CACHE, DEFAULT_DUMP, Dump  # noqa: E402
from fbmaterial import MaterialResolver  # noqa: E402
from fbmesh import CAT_DECAL, MeshSet, decode_lod  # noqa: E402
from fbtex import TexHeader, decode, encode_dds, passthrough_dds  # noqa: E402
from fbworld import scan_world  # noqa: E402
from bake import Voxels, bake, subdivide  # noqa: E402
from glbwrite import GlbBuilder  # noqa: E402

SKIP_PATH = ("electricbox_cable", "construction/wire", "objects/crowds", "gameplay/rvo", "/fx/", "fx/", "objects/lighting/volumetric",
             "/lightshafts", "/godray", "/occluder", "_shadowproxy", "/proxy/")
NO_COLLIDE_PATH = ("foliage", "vegetation", "/tree", "/plant", "grass", "/ivy", "cable", "wire", "/flag", "cloth",
                   "objects/lighting", "/lights/", "/lamp", "/sign", "billboard", "/glassshard", "/debris", "/trash",
                   "/litter", "/paper", "/leaves")
NO_COLLIDE_SHADER = ("foliage", "decal", "leaf", "leaves", "grass", "hair", "cable")


def log(*a):
    print(*a, flush=True)


# ----------------------------------------------------------------------------- geometry helpers
def transform_part(part, M):
    """Frostbite local -> Frostbite world (row-vector convention)."""
    A = M[:3, :3]
    pos = part["pos"].astype(np.float64) @ A + M[3, :3]
    nrm = None
    if part["nrm"] is not None:
        try:
            Ainv = np.linalg.inv(A)
        except np.linalg.LinAlgError:
            Ainv = A
        nrm = part["nrm"].astype(np.float64) @ Ainv.T  # n' = n @ inv(A)^T  (row vectors)
        nrm /= np.maximum(np.linalg.norm(nrm, axis=1, keepdims=True), 1e-9)
    return pos, nrm


def bbox_world(bmin, bmax, M):
    c = np.array([[x, y, z] for x in (bmin[0], bmax[0]) for y in (bmin[1], bmax[1]) for z in (bmin[2], bmax[2])])
    w = c @ M[:3, :3] + M[3, :3]
    return w.min(0), w.max(0)


# ----------------------------------------------------------------------------- vertical-ray queries
class VGrid:
    """2D grid over XZ (glTF space) for vertical ray casts against a triangle soup."""

    def __init__(self, tris, cell=2.0):
        self.t = tris  # (N,3,3)
        self.cell = cell
        lo = tris[:, :, [0, 2]].min(1)
        hi = tris[:, :, [0, 2]].max(1)
        self.o = lo.min(0) - 1
        i0 = np.floor((lo - self.o) / cell).astype(int)
        i1 = np.floor((hi - self.o) / cell).astype(int)
        self.cells = defaultdict(list)
        for ti in range(len(tris)):
            for gx in range(i0[ti, 0], i1[ti, 0] + 1):
                for gz in range(i0[ti, 1], i1[ti, 1] + 1):
                    self.cells[(gx, gz)].append(ti)
        self.cells = {k: np.array(v, np.int64) for k, v in self.cells.items()}

    def heights(self, x, z):
        """All surface heights (y) of triangles above/below the vertical line through (x,z)."""
        k = (int(math.floor((x - self.o[0]) / self.cell)), int(math.floor((z - self.o[1]) / self.cell)))
        ids = self.cells.get(k)
        if ids is None:
            return np.zeros(0)
        T = self.t[ids]
        a, b, c = T[:, 0], T[:, 1], T[:, 2]
        v0 = b[:, [0, 2]] - a[:, [0, 2]]
        v1 = c[:, [0, 2]] - a[:, [0, 2]]
        v2 = np.array([x, z]) - a[:, [0, 2]]
        den = v0[:, 0] * v1[:, 1] - v1[:, 0] * v0[:, 1]
        ok = np.abs(den) > 1e-10
        den = np.where(ok, den, 1)
        u = (v2[:, 0] * v1[:, 1] - v1[:, 0] * v2[:, 1]) / den
        v = (v0[:, 0] * v2[:, 1] - v2[:, 0] * v0[:, 1]) / den
        inside = ok & (u >= -1e-6) & (v >= -1e-6) & (u + v <= 1 + 1e-6)
        y = a[:, 1] + u * (b[:, 1] - a[:, 1]) + v * (c[:, 1] - a[:, 1])
        return y[inside]


# ----------------------------------------------------------------------------- collision
def simplify_collision(pieces, cell):
    """Vertex clustering over every piece at once (anisotropic cell: x/z coarse, y fine so steps and
    ledges keep their height); degenerate and duplicate triangles go.  -> (tris (N,3,3), piece id (N,))"""
    cell = max(cell, 1e-3)
    T = np.concatenate(pieces).astype(np.float64)
    piece = np.repeat(np.arange(len(pieces)), [len(p) for p in pieces])
    V = T.reshape(-1, 3)
    q = np.floor(V / np.array([cell, cell * 0.4, cell])).astype(np.int64)
    _, cid, inv = np.unique(q, axis=0, return_index=True, return_inverse=True)
    inv = inv.reshape(-1)
    sums = np.zeros((len(cid), 3))
    np.add.at(sums, inv, V)
    rep = sums / np.bincount(inv, minlength=len(cid))[:, None]
    I = inv.reshape(-1, 3)
    ok = (I[:, 0] != I[:, 1]) & (I[:, 1] != I[:, 2]) & (I[:, 0] != I[:, 2])
    I, piece = I[ok], piece[ok]
    key = np.sort(I, axis=1)
    _, first = np.unique(key, axis=0, return_index=True)
    first = np.sort(first)
    I, piece = I[first], piece[first]
    out = rep[I]
    area = np.linalg.norm(np.cross(out[:, 1] - out[:, 0], out[:, 2] - out[:, 0]), axis=1)
    ok = area > 1e-4
    return out[ok].astype(np.float32), piece[ok]


# ----------------------------------------------------------------------------- spawns
def body_clear(vox, p):
    """No render geometry in a 0.35 m radius column from 0.4 m to 1.8 m above the feet."""
    pts = np.array([(p[0] + dx, p[1] + dy, p[2] + dz) for dy in (0.4, 1.0, 1.6, 1.8)
                    for dx, dz in [(0, 0), (0.35, 0), (-0.35, 0), (0, 0.35), (0, -0.35)]])
    ok, flat = vox._flat(pts)
    return not np.any(vox.occ[flat[ok]])


def open_yaw(vox, p, toward_centre, half):
    """Yaw of the longest clear sightline at eye height that stays inside the arena square,
    preferring the arena centre on ties."""
    yaws = np.arange(0, 360, 15.0)
    r = np.radians(yaws)
    D = np.stack([-np.sin(r), np.zeros_like(r), -np.cos(r)], 1)
    O = np.repeat(np.array([[p[0], p[1] + 1.6, p[2]]]), len(yaws), 0)
    reach = np.full(len(yaws), 80.0)
    for t in np.arange(1.0, 80.0, vox.cell):
        ok, flat = vox._flat(O + D * t)
        Q = O + D * t
        outside = (np.abs(Q[:, 0]) > half) | (np.abs(Q[:, 2]) > half)
        hit = ((ok & vox.occ[flat]) | outside) & (reach >= 80.0)
        reach[hit] = t
    diff = np.abs((yaws - toward_centre + 180) % 360 - 180)
    score = np.minimum(reach, 60.0) - diff * 0.05
    return float(yaws[int(np.argmax(score))])

def pick_spawns(tris, half, count=20, edge_margin=12.0, seed=1, vox=None, max_y=None, ground_share=0.25, y_weight=0.75,
                min_y=None):
    rng = np.random.default_rng(seed)
    a, b, c = tris[:, 0], tris[:, 1], tris[:, 2]
    n = np.cross(b - a, c - a)
    area2 = np.linalg.norm(n, axis=1)
    ny = n[:, 1] / np.maximum(area2, 1e-12)
    walk = (ny > 0.8) & (area2 > 1e-6)
    if not walk.any():
        return []
    wi = np.nonzero(walk)[0]
    w = area2[wi] / area2[wi].sum()
    N = 40000
    pick = rng.choice(wi, size=N, p=w)
    r1, r2 = rng.random(N), rng.random(N)
    s = np.sqrt(r1)
    P = (1 - s)[:, None] * a[pick] + (s * (1 - r2))[:, None] * b[pick] + (s * r2)[:, None] * c[pick]
    inner = (np.abs(P[:, 0]) < half - edge_margin) & (np.abs(P[:, 2]) < half - edge_margin)
    P = P[inner]
    grid = VGrid(tris)
    good = []
    ring = [(0.45 * math.cos(t), 0.45 * math.sin(t)) for t in np.linspace(0, 2 * math.pi, 8, endpoint=False)]
    for p in P[:6000]:
        x, y, z = p
        if vox is not None and not body_clear(vox, p):
            continue
        ok = True
        for dx, dz in [(0.0, 0.0)] + ring:
            hs = grid.heights(x + dx, z + dz)
            # support: a surface within +-0.3 m of the feet under every probe
            if not np.any(np.abs(hs - y) < 0.3):
                ok = False
                break
            # clearance: nothing between 0.3 m and 2.0 m above the feet (cylinder, 0.45 m radius)
            if np.any((hs > y + 0.3) & (hs < y + 2.0)):
                ok = False
                break
        if not ok:
            continue
        hs = grid.heights(x, z)
        sky = not np.any(hs > y + 2.0)
        good.append((x, y + 0.02, z, sky))
        if len(good) >= 2500:
            break
    if not good:
        return []
    G = np.array([g[:3] for g in good])
    sky = np.array([g[3] for g in good])
    if max_y is not None or min_y is not None:
        keep = (G[:, 1] <= (max_y if max_y is not None else 1e9)) & (G[:, 1] >= (min_y if min_y is not None else -1e9))
        if keep.sum() >= count:
            G, sky = G[keep], sky[keep]
    if sky.sum() >= count:
        G = G[sky]
    # drop the highest 3% (isolated tower tops) and limit street-level spawns to a share
    if len(G) > count * 3 and max_y is None:
        G = G[G[:, 1] <= np.percentile(G[:, 1], 97)]
    ground = np.percentile(G[:, 1], 5)
    is_ground = G[:, 1] < ground + 3.0
    n_up = int(round(count * (1 - ground_share)))
    max_ground = max(1, count - n_up) if (~is_ground).sum() >= n_up else count
    # farthest point sampling (3D; vertical distance weighted so spawns spread over levels too)
    W = G * np.array([1.0, y_weight, 1.0])
    first = int(np.argmax(np.where(is_ground, -1e9, G[:, 1] - np.abs(G[:, 0]) - np.abs(G[:, 2]))))
    chosen = [first]
    d = np.linalg.norm(W - W[first], axis=1)
    n_ground = int(is_ground[first])
    while len(chosen) < min(count, len(G)):
        dd = d.copy()
        if n_ground >= max_ground:
            dd[is_ground] = -1
        i = int(np.argmax(dd))
        if dd[i] <= 0:
            break
        chosen.append(i)
        n_ground += int(is_ground[i])
        d = np.minimum(d, np.linalg.norm(W - W[i], axis=1))
    out = []
    for i in chosen:
        x, y, z = G[i]
        # face the arena centre: forward(yaw) = (-sin yaw, 0, -cos yaw) in glTF (== IW4 +X at yaw 0)
        fx, fz = -x, -z
        yaw = math.degrees(math.atan2(-fx, -fz)) if (fx * fx + fz * fz) > 1e-6 else 0.0
        if vox is not None:
            yaw = open_yaw(vox, (x, y, z), yaw, half)
        out.append({"origin": [round(float(x), 3), round(float(y), 3), round(float(z), 3)], "yaw_deg": round(yaw, 1)})
    return out


# ----------------------------------------------------------------------------- preview
def render_preview(prims, spawns, half, path, px=1024):
    """Top-down orthographic preview: splat area-sampled points, max-height z-buffer, hillshade."""
    res = (2 * half) / px
    H = np.full((px, px), -1e9, np.float32)
    C = np.zeros((px, px, 3), np.float32)
    rng = np.random.default_rng(0)
    for pr in prims:
        P, I, col = pr["pos"], pr["idx"], pr["color"]
        a, b, c = P[I[:, 0]], P[I[:, 1]], P[I[:, 2]]
        area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
        # projected area matters for top-down; sample by full area but cap
        ns = np.clip(np.ceil(area / (res * res) * 4.0), 1, 20000).astype(np.int64)
        tri = np.repeat(np.arange(len(I)), ns)
        if len(tri) > 20_000_000:
            tri = rng.choice(tri, 20_000_000, replace=False)
        r1, r2 = rng.random(len(tri)), rng.random(len(tri))
        s = np.sqrt(r1)
        Q = (1 - s)[:, None] * a[tri] + (s * (1 - r2))[:, None] * b[tri] + (s * r2)[:, None] * c[tri]
        ix = ((Q[:, 0] + half) / res).astype(np.int64)
        iz = ((Q[:, 2] + half) / res).astype(np.int64)
        ok = (ix >= 0) & (iz >= 0) & (ix < px) & (iz < px)
        ix, iz, y = ix[ok], iz[ok], Q[ok, 1].astype(np.float32)
        flat = iz * px + ix
        order = np.argsort(y)
        flat, y = flat[order], y[order]
        Hf = H.reshape(-1)
        upd = y >= Hf[flat]
        # last write wins in sorted order -> highest
        Hf[flat[upd]] = y[upd]
        Cf = C.reshape(-1, 3)
        Cf[flat[upd]] = np.array(col[:3], np.float32)
    # fill single-pixel sampling holes from the highest 3x3 neighbour (twice)
    for _ in range(2):
        e = H < -1e8
        if not e.any():
            break
        Hp = np.pad(H, 1, constant_values=-1e9)
        Cp = np.pad(C, ((1, 1), (1, 1), (0, 0)))
        best = Hp[1:-1, 1:-1].copy()
        bestc = C.copy()
        for dy in (-1, 0, 1):
            for dx in (-1, 0, 1):
                nh = Hp[1 + dy:1 + dy + px, 1 + dx:1 + dx + px]
                upd = e & (nh > best)
                best[upd] = nh[upd]
                bestc[upd] = Cp[1 + dy:1 + dy + px, 1 + dx:1 + dx + px][upd]
        H, C = best, bestc
    empty = H < -1e8
    Hn = np.where(empty, np.nan, H)
    lo, hi = np.nanpercentile(Hn, 2), np.nanpercentile(Hn, 99.5)
    Hc = np.where(empty, lo, H)
    gz, gx = np.gradient(Hc, res)
    shade = np.clip(1.0 - 0.35 * (gx * 0.6 + gz * 0.8), 0.35, 1.3)
    hnorm = np.clip((Hc - lo) / max(hi - lo, 1e-3), 0, 1)
    tint = 0.45 + 0.55 * hnorm
    img = C * (shade * tint)[:, :, None]
    img[empty] = (0.05, 0.05, 0.08)
    img = (np.clip(img, 0, 1) * 255).astype(np.uint8)
    im = Image.fromarray(img)
    from PIL import ImageDraw
    d = ImageDraw.Draw(im)
    for s in spawns:
        x, _, z = s["origin"]
        cx, cz = (x + half) / res, (z + half) / res
        d.ellipse([cx - 5, cz - 5, cx + 5, cz + 5], outline=(255, 30, 30), width=2)
        yaw = math.radians(s["yaw_deg"])
        d.line([cx, cz, cx - 12 * math.sin(yaw), cz - 12 * math.cos(yaw)], fill=(255, 220, 0), width=2)
    im.save(path)
    return lo, hi


# ----------------------------------------------------------------------------- textures
class Textures:
    """Texture requests (ref, role) -> one DDS each, sized under per-request caps and a total
    budget.  Roles: 'albedo' (BC1/BC3 passthrough, BC7 re-encoded), 'normal' (IW4 DXT5nm:
    x in alpha and red, y in green), 'spec' (IW4 specular map from MEC RSM: rgb = F0 / metal
    tint, a = smoothness as gloss)."""

    def __init__(self, dump, gb, log):
        self.dump, self.gb, self.log = dump, gb, log
        self.req = {}  # (role, file, inst) -> dict(cap, hdr, chunk, levels...)
        self.out = {}  # key -> (gltf texture index | None, info)
        self.fail = defaultdict(int)

    def _load(self, ref):
        e = self.dump.load(ref.file)
        to = e.by_guid.get(ref.inst) if e else None
        if to is None or not to.isa("TextureBaseAsset") or not to.get("Resource"):
            return None, None, "no TextureAsset"
        hb, _ = self.dump.res_bytes(to["Resource"])
        if not hb or len(hb) < 0x6C:
            return None, None, "res missing"
        th = TexHeader(hb)
        ch = self.dump.chunk(th.chunk_id)
        if ch is None:
            return None, None, "chunk missing"
        return hb, ch, to.get("Name") or "tex"

    def want(self, role, ref, cap):
        if ref is None:
            return None
        key = (role, ref.file, ref.inst)
        if key not in self.req:
            hb, ch, name = self._load(ref)
            if hb is None:
                self.fail[f"{role}: {name}"] += 1
                self.req[key] = None
                return None
            th = TexHeader(hb)
            self.req[key] = dict(hb=hb, ch=ch, name=name, th=th, cap=cap)
        elif self.req[key] is not None:
            self.req[key]["cap"] = max(self.req[key]["cap"], cap)
        return key if self.req.get(key) is not None else None

    @staticmethod
    def _bytes(edge_w, edge_h, block):
        total, w, h = 0, edge_w, edge_h
        while True:
            total += max(1, (w + 3) // 4) * max(1, (h + 3) // 4) * block
            if w <= 4 and h <= 4:
                return total
            w, h = max(1, w // 2), max(1, h // 2)

    def plan(self, budget_mb):
        """Pick each texture's top edge: its cap, then halve the largest until the estimate fits."""
        live = {k: r for k, r in self.req.items() if r is not None}
        for k, r in live.items():
            th = r["th"]
            e = max(th.width, th.height)
            s = 1
            while e / s > r["cap"]:
                s *= 2
            r["scale"] = s
        def est(k, r):
            th = r["th"]
            w, h = max(1, th.width // r["scale"]), max(1, th.height // r["scale"])
            block = 8 if (k[0] == "albedo" and th.format in (54, 55, 56, 57)) else 16
            return self._bytes(w, h, block)
        total = sum(est(k, r) for k, r in live.items())
        while total > budget_mb * 1048576:
            k, r = max(live.items(), key=lambda kv: est(*kv))
            if max(r["th"].width, r["th"].height) // r["scale"] <= 64:
                break
            total -= est(k, r)
            r["scale"] *= 2
            total += est(k, r)
        self.planned_mb = total / 1048576
        return self.planned_mb

    def get(self, key, albedo_avg=None):
        """-> (texture index | None, info dict)."""
        if key is None:
            return None, {}
        if key in self.out:
            return self.out[key]
        r = self.req[key]
        role = key[0]
        th = r["th"]
        edge = max(th.width, th.height) // r["scale"]
        info = {}
        idx = None
        try:
            if role == "albedo":
                small = decode(r["hb"], r["ch"], 64)
                if small is not None:
                    a = np.asarray(small.convert("RGBA"), np.float32) / 255.0
                    info["avg"] = tuple(a[:, :, :3].reshape(-1, 3).mean(0))
                    info["cut"] = float((a[:, :, 3] < 0.5).mean())
                    info["alpha_mean"] = float(a[:, :, 3].mean())
                pt = passthrough_dds(r["hb"], r["ch"], edge)
                if pt is not None:
                    data, w, h, _ = pt
                else:
                    img = decode(r["hb"], r["ch"], edge)
                    if img is None or min(img.size) < 4:
                        raise ValueError(f"fmt={th.format} type={th.type}")
                    img = _fit4(img)
                    w, h = img.size
                    data = encode_dds(img, bc3=True, srgb=True)
                idx = self.gb.image(r["name"], data, "image/vnd-ms.dds")
            elif role == "normal":
                img = decode(r["hb"], r["ch"], edge)
                if img is None or min(img.size) < 4:
                    raise ValueError(f"fmt={th.format} type={th.type}")
                img = _fit4(img)
                a = np.asarray(img.convert("RGBA"), np.uint8)
                x, y = a[:, :, 0], a[:, :, 1]
                nm = np.stack([x, y, np.full_like(x, 255), x], -1)
                w, h = img.size
                data = encode_dds(Image.fromarray(nm, "RGBA"), bc3=True, srgb=False)
                idx = self.gb.image(r["name"] + "_iw4nm", data, "image/vnd-ms.dds")
            elif role == "spec":
                img = decode(r["hb"], r["ch"], edge)
                if img is None or min(img.size) < 4:
                    raise ValueError(f"fmt={th.format} type={th.type}")
                img = _fit4(img)
                a = np.asarray(img.convert("RGBA"), np.float32) / 255.0
                refl, smooth, metal = a[:, :, 0], a[:, :, 1], a[:, :, 2]
                f0 = 0.16 * refl * refl  # Frostbite reflectance -> F0 (0.5 -> 4 %)
                tint = np.array(albedo_avg if albedo_avg is not None else (0.6, 0.6, 0.6), np.float32) ** 2.2
                spec = f0[:, :, None] * (1 - metal[:, :, None]) + tint[None, None, :] * metal[:, :, None]
                rgb = np.clip(spec, 0, 1) ** (1 / 2.2)
                out = np.concatenate([rgb, smooth[:, :, None]], -1)
                w, h = img.size
                data = encode_dds(Image.fromarray((out * 255 + 0.5).astype(np.uint8), "RGBA"), bc3=True, srgb=False)
                idx = self.gb.image(r["name"] + "_iw4spec", data, "image/vnd-ms.dds")
            info["size"] = (w, h)
            info["bytes"] = len(data)
        except Exception as ex:  # noqa
            self.fail[f"{role}: {ex!r}"[:80]] += 1
            idx = None
        self.out[key] = (idx, info)
        return self.out[key]


def _fit4(img):
    w, h = img.size
    W, H = max(4, w // 4 * 4), max(4, h // 4 * 4)
    return img if (W, H) == (w, h) else img.resize((W, H), Image.BOX)


def boundary_tris(half, y0, y1, lid=True):
    """Invisible walls on the four edges of the square (and a lid), as triangles (N,3,3)."""
    c = [(-half, -half), (half, -half), (half, half), (-half, half)]
    T = []
    for i in range(4):
        (x0, z0), (x1, z1) = c[i], c[(i + 1) % 4]
        a, b, cc, d = (x0, y0, z0), (x1, y0, z1), (x1, y1, z1), (x0, y1, z0)
        # split long walls so clip leaves stay small
        n = max(1, int(math.ceil(math.hypot(x1 - x0, z1 - z0) / 20.0)))
        for k in range(n):
            t0, t1 = k / n, (k + 1) / n
            p = lambda P, Q, t: tuple(P[j] + (Q[j] - P[j]) * t for j in range(3))  # noqa: E731
            a2, b2, c2, d2 = p(a, b, t0), p(a, b, t1), p(d, cc, t1), p(d, cc, t0)
            T += [(a2, b2, c2), (a2, c2, d2)]
    if lid:
        n = max(1, int(math.ceil(2 * half / 20.0)))
        s = 2 * half / n
        for i in range(n):
            for k in range(n):
                x0, z0 = -half + i * s, -half + k * s
                a, b, cc, d = (x0, y1, z0), (x0 + s, y1, z0), (x0 + s, y1, z0 + s), (x0, y1, z0 + s)
                T += [(a, cc, b), (a, d, cc)]
    return np.array(T, np.float32)


# ----------------------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", required=True)
    ap.add_argument("--title", default=None, help="menu name (arena.json `title`), e.g. \"Anchor Rooftops\"")
    ap.add_argument("--center", nargs=2, type=float, required=True, metavar=("X", "Z"),
                    help="region centre, Frostbite world x z")
    ap.add_argument("--size", type=float, default=80.0, help="play square edge length (m)")
    ap.add_argument("--ymin", type=float, default=-1e9, help="drop triangles below this Frostbite y")
    ap.add_argument("--dump", default=DEFAULT_DUMP)
    ap.add_argument("--out", default=DEFAULT_ARENAS)
    ap.add_argument("--cache", default=DEFAULT_CACHE, help="index/world-scan cache dir (derived data only)")
    ap.add_argument("--backdrop", type=float, default=300.0,
                    help="edge (m) of the square of render-only surroundings around the play square; 0 = none")
    ap.add_argument("--detail-top", type=float, default=60.0,
                    help="Frostbite y above which play-square objects are 'far' (not subdivided, coarse bake only)")
    ap.add_argument("--backdrop-min-extent", type=float, default=10.0, help="backdrop objects smaller than this are left out")
    ap.add_argument("--backdrop-tri-cap", type=int, default=3000,
                    help="backdrop meshes use their first LOD at or under this many triangles")
    ap.add_argument("--ceiling", type=float, default=22.0,
                    help="play band height above the street (m): spawns stay under it, a collision lid and the "
                         "out-of-bounds kill sit just above; 0 = no band (whole height)")
    ap.add_argument("--no-boundary", dest="boundary", action="store_false",
                    help="no invisible walls/lid on the play square edges")
    ap.add_argument("--no-decals", dest="decals", action="store_false")
    ap.add_argument("--tex-max", type=int, default=2048, help="max colour map edge (px) in the play square")
    ap.add_argument("--normal-max", type=int, default=1024, help="max normal map edge (px)")
    ap.add_argument("--spec-max", type=int, default=512, help="max specular map edge (px)")
    ap.add_argument("--backdrop-tex-max", type=int, default=256, help="max texture edge for backdrop-only textures")
    ap.add_argument("--tex-budget-mb", type=float, default=200.0,
                    help="total GPU texture budget (BCn bytes incl. mips); the largest textures halve until it fits")
    ap.add_argument("--normal-flip-y", action="store_true", help="invert normal map green")
    ap.add_argument("--collision-budget", type=int, default=165000,
                    help="max collision triangles (iw4L clipmap: <256 segments x 1024 verts)")
    ap.add_argument("--mirror-z", action="store_true",
                    help="treat Frostbite as left-handed and mirror Z (NOT what the data shows; see docstring)")
    ap.add_argument("--spawns", type=int, default=10)
    ap.add_argument("--ground-share", type=float, default=0.3, help="share of spawns on the band's lowest level")
    ap.add_argument("--band-base", type=float, default=None,
                    help="Frostbite y of the play band's floor (default: the street). For a rooftop cluster: the "
                         "lowest roof level; collision below base-16 m is dropped and leaving it kills")
    ap.add_argument("--sun", nargs=2, type=float, default=(30.0, 55.0), metavar=("AZ", "EL"),
                    help="sun azimuth and elevation in degrees")
    ap.add_argument("--no-bake", dest="bake", action="store_false", help="skip the per-vertex sky/sun bake")
    ap.add_argument("--voxel", type=float, default=0.3, help="fine bake occupancy cell (m), play square")
    ap.add_argument("--coarse-voxel", type=float, default=1.0, help="coarse bake cell (m), surroundings")
    ap.add_argument("--rays", type=int, default=32, help="sky rays per vertex")
    ap.add_argument("--subdiv", type=float, default=1.25,
                    help="split play-square triangles to this edge (m) before baking; 0 = off")
    ap.add_argument("--coll-cell", type=float, default=0.05,
                    help="collision vertex-clustering cell (m, x/z; y uses 0.4x); 0 keeps every triangle")
    args = ap.parse_args()
    t0 = time.time()
    cx, cz = args.center
    zs = -1.0 if args.mirror_z else 1.0
    half = args.size / 2
    bhalf = max(half, args.backdrop / 2)
    outdir = os.path.join(args.out, args.name)
    os.makedirs(outdir, exist_ok=True)

    dump = Dump(args.dump, args.cache)
    world = scan_world(dump)
    log(f"world: {len(world)} static instances ({time.time()-t0:.0f}s)")
    margin = 400.0
    cand = [w for w in world if abs(w["M"][12] - cx) < bhalf + margin and abs(w["M"][14] - cz) < bhalf + margin]
    log(f"candidates near region: {len(cand)}")
    resolver = MaterialResolver(dump)
    log(f"mesh variation entries: {len(resolver.mvdb)}")

    audit = {"instances": defaultdict(int), "skipped_instances": defaultdict(int), "skipped_tris": defaultdict(int),
             "tris_by_kind": defaultdict(int), "no_albedo_tris_by_preset": defaultdict(int),
             "double_sided_tris": defaultdict(int)}
    meshsets = {}

    def meshset(ref):
        key = (ref[0], ref[1])
        if key in meshsets:
            return meshsets[key]
        val = None
        e = dump.load(ref[0])
        mo = e.by_guid.get(ref[1]) if e else None
        path = dump.ebx_path(ref[0]) or ""
        if mo is None:
            val = ("missing", None)
        elif mo.type not in ("RigidMeshAsset", "CompositeMeshAsset"):
            val = (mo.type, None)
        elif any(s in path for s in SKIP_PATH):
            val = ("path", None)
        else:
            data, meta = dump.res_bytes(mo["MeshSetResource"])
            try:
                ms = MeshSet(data) if data else None
            except Exception as ex:  # noqa
                ms = None
                log("  meshset failed", path, repr(ex)[:100])
            val = ("ok", dict(e=e, mo=mo, path=path, ms=ms, meta=meta, data=data, lods={}, mats={})) if ms else ("decode", None)
        meshsets[key] = val
        return val

    def lod_parts(m, li):
        if li in m["lods"]:
            return m["lods"][li]
        ms, data, meta = m["ms"], m["data"], m["meta"]
        L = ms.lods[li]
        parts = []
        try:
            chunk = dump.chunk(L.chunk_id) if L.chunk_id != "00000000-0000-0000-0000-000000000000" else None
            inline = None
            if chunk is None and meta is not None:
                ioff, isz = int.from_bytes(meta[0:4], "little"), int.from_bytes(meta[4:8], "little")
                if isz:
                    inline = data[ioff + L.inline_off: ioff + L.inline_off + L.vb_size + L.ib_size]
            parts = decode_lod(ms, li, chunk, inline, categories=(0, 1, 2))
        except Exception as ex:  # noqa
            log("  mesh decode failed", m["path"], repr(ex)[:120])
        for p in parts:
            mid = p["material_id"]
            if mid not in m["mats"]:
                m["mats"][mid] = resolver.resolve(m["e"], m["mo"], mid)
            p["mat"] = m["mats"][mid]
        m["lods"][li] = parts
        return parts

    # ---------------- classify instances: play square (LOD0, collision) / backdrop (render only)
    lo_r = np.array([cx - half, cz - half])
    hi_r = np.array([cx + half, cz + half])
    items = []  # (zone, mesh dict, M, ext)
    for w in cand:
        kind, m = meshset(w["mesh"])
        if m is None:
            audit["skipped_instances"][kind] += 1
            continue
        M = np.array(w["M"], np.float64).reshape(4, 4)
        bmn, bmx = bbox_world(np.array(m["ms"].bbox_min), np.array(m["ms"].bbox_max), M)
        ext = float(np.max(bmx - bmn))
        if not (bmx[0] < lo_r[0] - 1 or bmn[0] > hi_r[0] + 1 or bmx[2] < lo_r[1] - 1 or bmn[2] > hi_r[1] + 1):
            items.append(("core", m, M, ext))
        elif args.backdrop > 0 and not (bmx[0] < cx - bhalf or bmn[0] > cx + bhalf or bmx[2] < cz - bhalf or bmn[2] > cz + bhalf):
            if ext < args.backdrop_min_extent:
                audit["skipped_instances"]["backdrop_small"] += 1
                continue
            items.append(("backdrop", m, M, ext))
    for z in ("core", "backdrop"):
        audit["instances"][z] = sum(1 for it in items if it[0] == z)
    log(f"instances: {dict(audit['instances'])}, skipped {dict(audit['skipped_instances'])}")

    # ---------------- gather triangles
    render = defaultdict(lambda: {"pos": [], "nrm": [], "uv": [], "idx": [], "n": 0})
    mats_by_key = {}
    coll = []
    coll_meta = []
    path_tris = defaultdict(int)
    for zone, m, M, ext in items:
        ms = m["ms"]
        if zone == "core":
            li = 0
        else:
            li = len(ms.lods) - 1
            for k in range(len(ms.lods)):
                ntri = sum(sc.prim_count for sc in ms.lods[k].sections
                           if any(sc.index in ms.lods[k].categories[c] for c in (0, 1)))
                if ntri <= args.backdrop_tri_cap:
                    li = k
                    break
        parts = lod_parts(m, li)
        path = m["path"]
        det = np.linalg.det(M[:3, :3])
        flip = (det > 0) if args.mirror_z else (det < 0)
        no_col_mesh = zone != "core" or ext < 0.3 or any(s in path for s in NO_COLLIDE_PATH)
        for p in parts:
            mat = p["mat"]
            kind = mat["kind"]
            if p["category"] == CAT_DECAL or "/decals/" in path or path.startswith("objects/decals"):
                kind = "decal"
            if kind == "invisible" or (p["name"] or "").lower() in ("nmvisualize",):
                audit["skipped_tris"]["invisible material"] += len(p["idx"])
                continue
            if kind == "decal" and (not args.decals or zone != "core"):
                audit["skipped_tris"]["decal" if zone == "core" else "backdrop decal"] += len(p["idx"])
                continue
            pos, nrm = transform_part(p, M)
            I = p["idx"]
            cen = (pos[I[:, 0]] + pos[I[:, 1]] + pos[I[:, 2]]) / 3
            keep = cen[:, 1] >= args.ymin
            if not keep.any():
                continue
            I = I[keep]
            G = np.empty_like(pos)
            G[:, 0] = pos[:, 0] - cx
            G[:, 1] = pos[:, 1]
            G[:, 2] = (pos[:, 2] - cz) * zs
            GN = None
            if nrm is not None:
                GN = nrm.copy()
                GN[:, 2] = GN[:, 2] * zs
            if flip:
                I = I[:, [0, 2, 1]]
            used_v, inv = np.unique(I.reshape(-1), return_inverse=True)
            I2 = inv.reshape(-1, 3)
            G = G[used_v]
            GN = GN[used_v] if GN is not None else np.zeros_like(G)
            UV = p["uv"][used_v] if p["uv"] is not None else np.zeros((len(G), 2), np.float32)
            ak = (mat["albedo"].file, mat["albedo"].inst) if mat["albedo"] is not None else None
            nk = (mat["normal"].file, mat["normal"].inst) if mat["normal"] is not None else None
            rk = (mat["rsm"].file, mat["rsm"].inst) if mat["rsm"] is not None else None
            col = tuple(np.round(mat["color"], 3)) if mat["color"] is not None else None
            if ak is None and kind not in ("glass",):
                preset = (mat["shader"] or "(none)").split(" <- ")[-1].split("/")[-1]
                audit["no_albedo_tris_by_preset"][f"{preset} [{mat['source']}]"] += len(I2)
            audit["tris_by_kind"][f"{zone}:{kind}"] += len(I2)
            # Two-sided Frostbite materials (DoubleSided shaders, cut-outs, glass) get explicit
            # back faces: the cloned IW4 techsets cull back faces.
            if "doublesided" in (mat["shader"] or "").lower() or kind in ("alphatest", "glass"):
                nv = len(G)
                G = np.concatenate([G, G])
                GN = np.concatenate([GN, -GN])
                UV = np.concatenate([UV, UV])
                I2 = np.concatenate([I2, I2[:, [0, 2, 1]] + nv])
                audit["double_sided_tris"]["added"] += len(I2) // 2
            # play-square detail (baked finely, subdivided) vs far parts of the same objects
            if zone == "core":
                c3 = G[I2].mean(1)
                if args.band_base is not None:
                    vy = (c3[:, 1] >= args.band_base - 25) & (c3[:, 1] <= args.band_base + args.ceiling + 25)
                else:
                    vy = c3[:, 1] <= args.detail_top
                near = (np.abs(c3[:, 0]) <= half + 15) & (np.abs(c3[:, 2]) <= half + 15) & vy
                groups = [("core", I2[near]), ("far", I2[~near])]
            else:
                groups = [(zone, I2)]
            for gz, Ig in groups:
                if not len(Ig):
                    continue
                uv_, inv_ = np.unique(Ig.reshape(-1), return_inverse=True)
                mkey = (gz, kind, ak, nk, rk, col)
                if mkey not in mats_by_key:
                    mats_by_key[mkey] = dict(mat=mat, kind=kind, zone=gz)
                r = render[mkey]
                r["pos"].append(G[uv_].astype(np.float32))
                r["nrm"].append(GN[uv_].astype(np.float32))
                r["uv"].append(UV[uv_].astype(np.float32))
                r["idx"].append(inv_.reshape(-1, 3) + r["n"])
                r["n"] += len(uv_)
            if zone == "core":
                path_tris["/".join(path.split("/")[:4])] += len(I2)
            if no_col_mesh or kind == "decal" or any(s in (mat["shader"] or "").lower() for s in NO_COLLIDE_SHADER):
                continue
            T = G[I2]
            c2 = T.mean(1)
            inside = (np.abs(c2[:, 0]) <= half + 1.0) & (np.abs(c2[:, 2]) <= half + 1.0)
            if inside.any():
                coll.append(T[inside])
                coll_meta.append(ext)
        # free decoded LODs of meshes used once
    for k, v in sorted(path_tris.items(), key=lambda kv: -kv[1])[:20]:
        log(f"    {v:9d} {k}")
    log(f"tris by kind: {dict(audit['tris_by_kind'])}")
    for k, v in sorted(audit["no_albedo_tris_by_preset"].items(), key=lambda kv: -kv[1])[:12]:
        log(f"    no-albedo {v:8d} tris  {k}")

    # ---------------- street level and the play band
    CT_all = np.concatenate(coll).astype(np.float64)
    nn = np.cross(CT_all[:, 1] - CT_all[:, 0], CT_all[:, 2] - CT_all[:, 0])
    ar = np.linalg.norm(nn, axis=1)
    walk = (nn[:, 1] > 0.7 * ar) & (ar > 1e-4)
    wy = CT_all[walk].mean(1)[:, 1]
    order = np.argsort(wy)
    cum = np.cumsum(ar[walk][order])
    street_y = float(wy[order][np.searchsorted(cum, 0.05 * cum[-1])]) if len(cum) else 0.0
    if args.band_base is not None:
        street_y = args.band_base
    band_top = street_y + args.ceiling if args.ceiling > 0 else None
    lid_y = band_top + 3.0 if band_top is not None else None
    log(f"street level y={street_y:.2f}" + (f", play band to {band_top:.1f}, lid at {lid_y:.1f}" if band_top else ""))
    if lid_y is not None:
        coll = [T[(T[:, :, 1].min(1) < lid_y) & (T[:, :, 1].max(1) > street_y - 16.0)] for T in coll]

    # ---------------- collision: simplified, then smallest objects dropped for budget
    live = [i for i in range(len(coll)) if len(coll[i])]
    CT, piece = simplify_collision([coll[i] for i in live], args.coll_cell)
    ext = np.array([coll_meta[i] for i in live])
    counts = np.bincount(piece, minlength=len(live))
    order = np.argsort(-ext, kind="stable")
    keep = np.zeros(len(live), bool)
    total = 0
    budget = args.collision_budget // 2 - (200 if args.boundary else 0)  # both windings are written
    for i in order:
        if total + counts[i] > budget:
            continue
        keep[i] = True
        total += counts[i]
    dropped = int((~keep & (counts > 0)).sum())
    CT = CT[keep[piece]]
    log(f"collision: {sum(len(coll[i]) for i in live)} tris -> {len(piece)} simplified -> {len(CT)} in budget "
        f"({dropped} small pieces dropped for budget)")
    walk_CT = CT

    # ---------------- materials + textures
    gb = GlbBuilder()
    tex = Textures(dump, gb, log)
    planned = {}
    for mkey, info in mats_by_key.items():
        mat, zone = info["mat"], info["zone"]
        core = zone in ("core", "far")
        planned[mkey] = (
            tex.want("albedo", mat["albedo"], args.tex_max if core else args.backdrop_tex_max),
            tex.want("normal", mat["normal"], args.normal_max if core else args.backdrop_tex_max // 2) if mat["kind"] != "glass" else None,
            tex.want("spec", mat["rsm"], args.spec_max if core else 64),
        )
    mb = tex.plan(args.tex_budget_mb)
    log(f"textures: {sum(1 for r in tex.req.values() if r)} requested, ~{mb:.0f} MB planned (budget {args.tex_budget_mb:.0f})")
    prims = []
    preview_prims = []
    mat_stats = defaultdict(int)
    for mkey, r in render.items():
        info = mats_by_key[mkey]
        mat, kind, zone = info["mat"], info["kind"], info["zone"]
        pos = np.concatenate(r["pos"])
        nrm = np.concatenate(r["nrm"])
        uv = np.concatenate(r["uv"])
        idx = np.concatenate(r["idx"]).astype(np.uint32)
        nlen = np.linalg.norm(nrm, axis=1, keepdims=True)
        bad = nlen[:, 0] < 0.5
        nrm = np.where(bad[:, None], np.array([0, 1, 0], np.float32), nrm / np.maximum(nlen, 1e-9)).astype(np.float32)
        ak, nk, sk = planned[mkey]
        ti, ainfo = tex.get(ak)
        if kind == "glass":
            # Catalyst glass: dark, reflective, see-through; a flat tinted colour reads better
            # than the (mostly smudge/dirt) glass colour maps under IW4 blending.
            g = np.array(ainfo.get("avg", (0.35, 0.42, 0.48)), np.float32)
            mat = dict(mat, color=tuple((0.25 + 0.35 * g).tolist()) + (1.0,))
            ti = None
        avg = ainfo.get("avg", None)
        ni, _ = tex.get(nk)
        si, _ = tex.get(sk, avg)
        if ti is not None:
            color = [1.0, 1.0, 1.0, 1.0]
            mat_stats["textured"] += 1
        else:
            c = mat["color"]
            if c is None:
                c = (0.10, 0.12, 0.14, 1.0) if kind == "glass" else (0.80, 0.80, 0.80, 1.0)
            color = [min(1.0, max(0.0, float(v))) for v in (list(c) + [1.0])[:4]]
            color[3] = 1.0
            avg = tuple(color[:3])
            mat_stats["flat colour"] += 1
        alpha_mode = None
        cut = ainfo.get("cut", 0.0)
        if kind == "alphatest" and ti is not None and 0.01 < cut < 0.99:
            alpha_mode = "MASK"
        elif kind == "decal" and ti is not None:
            alpha_mode = "BLEND"
        elif kind == "glass":
            alpha_mode = "BLEND"
            color[3] = 0.45
        if alpha_mode:
            mat_stats[alpha_mode] += 1
        if ni is not None:
            mat_stats["normal map"] += 1
        if si is not None:
            mat_stats["spec map"] += 1
        name = (mat["shader"] or "flat").split(" <- ")[-1].split("/")[-1]
        mi = gb.material(f"{kind}:{zone}:{name}", tuple(color), ti, double_sided=alpha_mode is not None,
                         alpha_mode=alpha_mode, normal=ni, specular=si)
        prims.append(dict(pos=pos, nrm=nrm, uv=uv, idx=idx.reshape(-1, 3), material=mi, zone=zone))
        if zone == "core":
            preview_prims.append(dict(pos=pos, idx=idx.reshape(-1, 3), color=avg or (0.6, 0.6, 0.6)))
    log(f"materials: {len(prims)} {dict(mat_stats)}; texture failures {dict(tex.fail)}")

    # ---------------- sun, then the per-vertex sky/sun bake
    az, el = math.radians(args.sun[0]), math.radians(args.sun[1])
    to_sun = np.array([math.sin(az) * math.cos(el), math.sin(el), -math.cos(az) * math.cos(el)])
    sun_dir = (-to_sun).round(4).tolist()  # direction light travels (iw4L convention)
    vox = None
    if args.bake:
        tb = time.time()
        allpos = np.concatenate([p["pos"] for p in prims])
        rng = np.random.default_rng(3)
        top = (lid_y + 40.0) if lid_y is not None else allpos[:, 1].max() + 2
        vox = Voxels(np.array([-half - 30, street_y - 8, -half - 30]), np.array([half + 30, top, half + 30]), args.voxel)
        clo = np.array([-bhalf - 4, allpos[:, 1].min() - 1, -bhalf - 4])
        chi = np.array([bhalf + 4, allpos[:, 1].max() + 2, bhalf + 4])
        coarse = Voxels(clo, chi, args.coarse_voxel)
        for p in prims:
            if materials_opaque(gb, p["material"]):
                T = p["pos"][p["idx"]].astype(np.float64)
                vox.add_triangles(T, rng)
                coarse.add_triangles(T, rng)
        before = sum(len(p["idx"]) for p in prims)
        if args.subdiv > 0:
            for p in prims:
                if p["zone"] == "core":
                    p["pos"], p["nrm"], p["uv"], p["idx"] = subdivide(p["pos"], p["nrm"], p["uv"], p["idx"], args.subdiv)
                    p["idx"] = p["idx"].reshape(-1, 3)
        log(f"bake: fine voxels {vox.dim.tolist()} @ {args.voxel} m ({vox.occ.mean():.3f} filled), coarse "
            f"{coarse.dim.tolist()} @ {args.coarse_voxel} m; split {before} -> {sum(len(p['idx']) for p in prims)} tris")
        sky, sun = bake(vox, np.concatenate([p["pos"] for p in prims]), np.concatenate([p["nrm"] for p in prims]),
                        sun_dir, rays=args.rays, ao_range=6.0, sun_range=300.0, log=log, coarse=coarse,
                        coarse_ao_range=20.0)
        k = 0
        for p in prims:
            n = len(p["pos"])
            s_, u_ = sky[k:k + n], sun[k:k + n]
            p["color"] = np.stack([s_, s_, s_, u_], 1).__mul__(255).round().clip(0, 255).astype(np.uint8)
            k += n
        log(f"bake: {time.time() - tb:.0f}s")
    gb.mesh_node(args.name, prims)
    gb.save(os.path.join(outdir, "arena.glb"))

    # ---------------- boundary (invisible walls + lid) and collision.glb
    floor_y = street_y - 1.0
    CB = CT
    if args.boundary and lid_y is not None:
        CB = np.concatenate([CT, boundary_tris(half, (street_y - 13.0) if args.band_base is not None else floor_y - 6.0, lid_y)])
    # IW4 collision triangles block from their front side only: write both windings so thin
    # walls, railings and simplified single-plane geometry stop the player from either side.
    CB = np.concatenate([CB, CB[:, [0, 2, 1]]])
    cg = GlbBuilder()
    flat = CB.reshape(-1, 3).astype(np.float32)
    q = np.round(flat / 0.001).astype(np.int64)
    _, first, inv = np.unique(q, axis=0, return_index=True, return_inverse=True)
    cg.mesh_node(args.name + "_collision", [dict(pos=flat[first], nrm=None, uv=None,
                                                 idx=inv.reshape(-1, 3).astype(np.uint32), material=None)])
    cg.save(os.path.join(outdir, "collision.glb"))

    # ---------------- spawns, bounds, sun
    spawns = pick_spawns(walk_CT.astype(np.float64), half, count=args.spawns, vox=vox, edge_margin=5.0,
                         max_y=(band_top - 2.0) if band_top is not None else None, min_y=street_y - 1.0,
                         ground_share=args.ground_share, y_weight=2.0)
    core_pos = np.concatenate([p["pos"] for p in prims if p["zone"] == "core"])
    allpos = np.concatenate([p["pos"] for p in prims])
    bmin, bmax = allpos.min(0), allpos.max(0)
    a, b, c = CT[:, 0], CT[:, 1], CT[:, 2]
    n = np.cross(b - a, c - a)
    walk = (n[:, 1] > 0.7 * np.linalg.norm(n, axis=1)) & (np.linalg.norm(n, axis=1) > 1.0)
    cen = CT.mean(1)
    walk &= (np.abs(cen[:, 0]) < half) & (np.abs(cen[:, 2]) < half)
    lowest = float(cen[walk, 1].min()) if walk.any() else float(core_pos[:, 1].min())
    play_floor = (street_y - 12.0) if args.band_base is not None else min(lowest, street_y) - 4.0
    play_top = (lid_y + 2.0) if lid_y is not None else round(float(core_pos[:, 1].max()) + 20.0, 2)
    meta = {"name": args.name, **({"title": args.title} if args.title else {}), "spawns": spawns,
            "bounds_min": [round(float(v), 3) for v in bmin], "bounds_max": [round(float(v), 3) for v in bmax],
            "sun_dir": sun_dir,
            "play_min": [-half - 0.5, round(play_floor, 2), -half - 0.5],
            "play_max": [half + 0.5, round(play_top, 2), half + 0.5],
            "street_y": round(street_y, 2),
            "vertex_color": "rgb = sky visibility, a = sun visibility" if args.bake else None,
            "materials": "baseColorTexture = colour map, normalTexture = IW4 DXT5nm normal map (x in alpha, y in "
                         "green), metallicRoughnessTexture = IW4 specular map (rgb specular colour, a gloss); images "
                         "are DDS (BC1/BC3/BC7 with mips); material name = '<kind>:<core|backdrop>:<preset>'",
            "boundary": ("invisible walls on the play square and a lid at play_max - 2 m" if args.boundary and lid_y else None),
            "source": {"game": "Mirror's Edge Catalyst", "level": "Levels/SP/SP_MainCity",
                       "frostbite_center_xz": [cx, cz], "size_m": args.size, "backdrop_m": args.backdrop,
                       "ceiling_m": args.ceiling,
                       "transform": ("gltf = (x - cx, y, -(z - cz))" if args.mirror_z else "gltf = (x - cx, y, z - cz)")
                       + " from Frostbite world (metres)"}}
    with open(os.path.join(outdir, "arena.json"), "w") as fh:
        json.dump(meta, fh, indent=1)

    render_preview(preview_prims, spawns, half, os.path.join(outdir, "preview.png"))
    render_preview(preview_prims, [], half, os.path.join(outdir, "minimap.png"), px=512)
    tex_mb = sum(i.get("bytes", 0) for _, i in tex.out.values()) / 1048576
    st = {"instances": dict(audit["instances"]), "unique_meshes": sum(1 for v in meshsets.values() if v[1]),
          "render_tris": int(sum(len(p["idx"]) for p in prims)), "render_verts": int(len(allpos)),
          "core_tris": int(sum(len(p["idx"]) for p in prims if p["zone"] == "core")),
          "materials": len(prims), "material_stats": dict(mat_stats),
          "textures": sum(1 for v in tex.out.values() if v[0] is not None), "texture_mb": round(tex_mb, 1),
          "texture_failures": dict(tex.fail),
          "collision_tris": int(len(CB)), "collision_pieces_dropped": dropped, "spawns": len(spawns),
          "spawn_heights": sorted(round(s["origin"][1] - street_y, 1) for s in spawns),
          "street_y": round(street_y, 2), "bounds_min": meta["bounds_min"], "bounds_max": meta["bounds_max"],
          "glb_bytes": os.path.getsize(os.path.join(outdir, "arena.glb")),
          "collision_glb_bytes": os.path.getsize(os.path.join(outdir, "collision.glb")),
          "audit": {k: dict(v) for k, v in audit.items()}, "seconds": round(time.time() - t0, 1)}
    with open(os.path.join(outdir, "stats.json"), "w") as fh:
        json.dump(st, fh, indent=1)
    for k, v in st.items():
        log(f"  {k}: {v}")
    log(f"wrote {outdir}")


def materials_opaque(gb, mi):
    """Blocks light in the bake: everything but blended glass and decals."""
    m = gb.j["materials"][mi]
    return m.get("alphaMode") != "BLEND"


if __name__ == "__main__":
    main()
