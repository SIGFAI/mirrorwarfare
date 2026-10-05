# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Sanity-check an arena package and render an oblique textured view (point-splat z-buffer).

    uv run --python 3.13 tools/mec/verify_arena.py ../mec-arenas/<name>

Checks: GLB structure, finite positions, unit normals, index ranges, winding vs normals
(CCW front faces), embedded PNGs decode, collision bounds, spawns stand on collision with
2 m clearance.  Writes <dir>/view.png.
"""
import io
import json
import math
import struct
import sys

import numpy as np
from PIL import Image

COMP = {5126: np.float32, 5125: np.uint32, 5123: np.uint16, 5121: np.uint8}
NCOMP = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}


def load_glb(path):
    b = open(path, "rb").read()
    magic, ver, total = struct.unpack_from("<III", b, 0)
    assert magic == 0x46546C67 and ver == 2 and total == len(b), "bad GLB header"
    jl, jt = struct.unpack_from("<II", b, 12)
    j = json.loads(b[20:20 + jl])
    bl, bt = struct.unpack_from("<II", b, 20 + jl)
    binb = b[28 + jl:28 + jl + bl]

    def acc(i):
        a = j["accessors"][i]
        v = j["bufferViews"][a["bufferView"]]
        n = NCOMP[a["type"]]
        arr = np.frombuffer(binb, COMP[a["componentType"]], a["count"] * n, v.get("byteOffset", 0) + a.get("byteOffset", 0))
        return arr.reshape(a["count"], n) if n > 1 else arr

    def image(i):
        im = j["images"][i]
        v = j["bufferViews"][im["bufferView"]]
        return Image.open(io.BytesIO(binb[v["byteOffset"]:v["byteOffset"] + v["byteLength"]]))

    return j, acc, image


def main(d):
    ok = True
    meta = json.load(open(f"{d}/arena.json"))
    j, acc, image = load_glb(f"{d}/arena.glb")
    prims = []
    ntri = 0
    wind_good = wind_total = 0
    for m in j["meshes"]:
        for p in m["primitives"]:
            P = acc(p["attributes"]["POSITION"]).astype(np.float64)
            N = acc(p["attributes"]["NORMAL"]).astype(np.float64) if "NORMAL" in p["attributes"] else None
            UV = acc(p["attributes"]["TEXCOORD_0"]) if "TEXCOORD_0" in p["attributes"] else None
            I = acc(p["indices"]).astype(np.int64).reshape(-1, 3)
            if not np.isfinite(P).all():
                print("  NaN/inf positions in", m["name"]); ok = False
            if I.max() >= len(P):
                print("  index out of range"); ok = False
            if N is not None:
                ln = np.linalg.norm(N, axis=1)
                if np.abs(ln - 1).max() > 1e-2:
                    print(f"  non-unit normals (max dev {np.abs(ln-1).max():.3f})"); ok = False
                a, b_, c = P[I[:, 0]], P[I[:, 1]], P[I[:, 2]]
                fn = np.cross(b_ - a, c - a)
                good = np.linalg.norm(fn, axis=1) > 1e-8
                vn = N[I[:, 0]] + N[I[:, 1]] + N[I[:, 2]]
                wind_good += int(((fn * vn).sum(1) > 0)[good].sum())
                wind_total += int(good.sum())
            ntri += len(I)
            mat = j["materials"][p["material"]] if "material" in p else {}
            pbr = mat.get("pbrMetallicRoughness", {})
            tex = None
            if "baseColorTexture" in pbr:
                tex = np.asarray(image(j["textures"][pbr["baseColorTexture"]["index"]]["source"]).convert("RGB"), np.float32) / 255
            COL = acc(p["attributes"]["COLOR_0"]).astype(np.float32) / 255 if "COLOR_0" in p["attributes"] else None
            prims.append((P, I, UV, tex, np.array(pbr.get("baseColorFactor", [0.8, 0.8, 0.8, 1])[:3]), COL))
    allP = np.concatenate([p[0] for p in prims])
    print(f"arena.glb: {len(prims)} primitives, {ntri} triangles, {len(allP)} verts, "
          f"{len(j.get('images', []))} images, bounds {allP.min(0).round(1)} .. {allP.max(0).round(1)}")
    print(f"  winding: {100.0*wind_good/max(1,wind_total):.2f}% of triangles CCW w.r.t. their vertex normals")
    if wind_total and wind_good / wind_total < 0.9:
        ok = False
    cj, cacc, _ = load_glb(f"{d}/collision.glb")
    CP = cacc(cj["meshes"][0]["primitives"][0]["attributes"]["POSITION"]).astype(np.float64)
    CI = cacc(cj["meshes"][0]["primitives"][0]["indices"]).astype(np.int64).reshape(-1, 3)
    CT = CP[CI]
    print(f"collision.glb: {len(CT)} triangles, bounds {CP.min(0).round(1)} .. {CP.max(0).round(1)}")
    # spawns: support + clearance by brute force vertical tests
    import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from build_arena import VGrid
    g = VGrid(CT)
    bad = 0
    for s in meta["spawns"]:
        x, y, z = s["origin"]
        hs = g.heights(x, z)
        sup = np.any(np.abs(hs - y) < 0.3)
        clear = not np.any((hs > y + 0.3) & (hs < y + 2.0))
        if not (sup and clear):
            bad += 1
    print(f"spawns: {len(meta['spawns'])}, {bad} failing support/clearance; heights "
          f"{sorted(round(s['origin'][1], 1) for s in meta['spawns'])}")
    if bad or len(meta["spawns"]) < 16:
        ok = False
    render(prims, meta, f"{d}/view.png")
    if len(sys.argv) > 2:
        # street-level view from the first ground spawn, looking along its yaw
        s0 = min(meta["spawns"], key=lambda s: s["origin"][1])
        o = np.array(s0["origin"]) + [0, 1.7, 0]
        yaw = math.radians(s0["yaw_deg"])
        render(prims, meta, f"{d}/view_street.png", eye=o, target=o + [-math.sin(yaw), 0.05, -math.cos(yaw)])
    print("OK" if ok else "PROBLEMS FOUND")


def render(prims, meta, out, W=1280, H=800, eye=None, target=None):
    """Point-splat preview; with baked COLOR_0 it shades like the game's lookup (sky ambient + masked sun)."""
    lo = np.array(meta["bounds_min"]); hi = np.array(meta["bounds_max"])
    ctr = np.array([0.0, np.percentile([s["origin"][1] for s in meta["spawns"]], 50), 0.0])
    if eye is None:
        eye = np.array([hi[0] * 1.15, ctr[1] + 120, hi[2] * 1.15])
        target = ctr
    ctr = target
    fwd = ctr - eye; fwd /= np.linalg.norm(fwd)
    right = np.cross(fwd, [0, 1, 0]); right /= np.linalg.norm(right)
    up = np.cross(right, fwd)
    f = 1.0 / math.tan(math.radians(55) / 2)
    Z = np.full(H * W, np.inf, np.float32)
    C = np.zeros((H * W, 3), np.float32)
    sun = -np.array(meta.get("sun_dir", [-0.4, -0.8, -0.3])); sun /= np.linalg.norm(sun)
    rng = np.random.default_rng(0)
    for P, I, UV, tex, factor, COL in prims:
        a, b, c = P[I[:, 0]], P[I[:, 1]], P[I[:, 2]]
        fn = np.cross(b - a, c - a); ar = np.linalg.norm(fn, axis=1); fn /= np.maximum(ar[:, None], 1e-9)
        cen = (a + b + c) / 3
        dist = np.maximum(np.linalg.norm(cen - eye, axis=1), 1)
        pix = (0.5 * ar) / (dist / (f * H / 2)) ** 2  # approx projected area in px
        ns = np.clip(np.ceil(pix * 2.5), 1, 3000).astype(np.int64)
        ns[(cen - eye) @ fwd < 0.3] = 0
        tri = np.repeat(np.arange(len(I)), ns)
        r1, r2 = rng.random(len(tri)), rng.random(len(tri)); s = np.sqrt(r1)
        w0, w1, w2 = 1 - s, s * (1 - r2), s * r2
        Q = w0[:, None] * a[tri] + w1[:, None] * b[tri] + w2[:, None] * c[tri]
        v = Q - eye
        zc = v @ fwd
        okk = zc > 0.5
        xs = (v @ right) / np.maximum(zc, 1e-3) * f * H / 2 + W / 2
        ys = -(v @ up) / np.maximum(zc, 1e-3) * f * H / 2 + H / 2
        ix, iy = xs.astype(np.int64), ys.astype(np.int64)
        okk &= (ix >= 0) & (iy >= 0) & (ix < W) & (iy < H)
        if tex is not None and UV is not None:
            uv = w0[:, None] * UV[I[tri, 0]] + w1[:, None] * UV[I[tri, 1]] + w2[:, None] * UV[I[tri, 2]]
            th, tw = tex.shape[:2]
            tu = (np.mod(uv[:, 0], 1) * tw).astype(np.int64) % tw
            tv = (np.mod(uv[:, 1], 1) * th).astype(np.int64) % th
            col = tex[tv, tu] * factor
        else:
            col = np.broadcast_to(factor.astype(np.float32), (len(tri), 3))
        nrm = fn[tri]
        okk &= (nrm * (eye - Q)).sum(1) > 0  # back-face cull (CCW front faces, as glTF)
        if COL is not None:
            cv = w0[:, None] * COL[I[tri, 0]] + w1[:, None] * COL[I[tri, 1]] + w2[:, None] * COL[I[tri, 2]]
            lam = 0.22 + 0.5 * cv[:, 0] + 0.6 * cv[:, 3] * np.clip(nrm @ sun, 0, 1)
        else:
            lam = 0.45 + 0.55 * np.clip(nrm @ sun, 0, 1)
        col = col * lam[:, None]
        flat = (iy * W + ix)[okk]; zz = zc[okk].astype(np.float32); cc = col[okk]
        order = np.argsort(-zz)  # far first so near overwrite
        flat, zz, cc = flat[order], zz[order], cc[order]
        upd = zz <= Z[flat]
        Z[flat[upd]] = zz[upd]; C[flat[upd]] = cc[upd]
    img = C.reshape(H, W, 3)
    sky = np.isinf(Z.reshape(H, W))
    img[sky] = (0.62, 0.75, 0.9)
    fog = np.clip((Z.reshape(H, W) - 150) / 600, 0, 0.6)
    fog[sky] = 0
    img = img * (1 - fog[..., None]) + np.array([0.62, 0.75, 0.9]) * fog[..., None]
    Image.fromarray((np.clip(img, 0, 1) * 255).astype(np.uint8)).save(out)
    print("wrote", out)


if __name__ == "__main__":
    main(sys.argv[1].rstrip("/\\"))
