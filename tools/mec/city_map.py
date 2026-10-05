# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Coarse city height map from building-instance bounding boxes (region picking aid).

    uv run --python 3.13 tools/mec/city_map.py [--out city_map.png]

Writes a top-down max-roof-height image (2 m/px) with a 100 m grid labelled in Frostbite x/z,
plus a ranked list of candidate arena centres (rooftop coverage + height variety).
"""
import argparse, json, os, struct, sys
import numpy as np
from PIL import Image, ImageDraw
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fbdata import DEFAULT_CACHE, Dump
from fbworld import scan_world

ap = argparse.ArgumentParser()
ap.add_argument("--out", default=os.path.join(DEFAULT_CACHE, "city_map.png"))
ap.add_argument("--size", type=float, default=160)
opt = ap.parse_args()
d = Dump()
world = scan_world(d)
bbp = os.path.join(d.cache, "mesh_bbox.json")
bb = json.load(open(bbp)) if os.path.isfile(bbp) else {}
sel = [w for w in world if ("centralcitybuildings" in w["file"] or "ground_zs" in w["file"])]
for w in sel:
    k = w["mesh"][0] + "/" + w["mesh"][1]
    if k in bb:
        continue
    bb[k] = None
    e = d.load(w["mesh"][0])
    if e is None:
        continue
    mo = e.by_guid.get(w["mesh"][1])
    if mo is None or mo.type != "RigidMeshAsset":
        continue
    data, _ = d.res_bytes(mo["MeshSetResource"])
    if data:
        mn = struct.unpack_from("<3f", data, 0); mx = struct.unpack_from("<3f", data, 16)
        bb[k] = [mn, mx, d.ebx_path(w["mesh"][0])]
json.dump(bb, open(bbp, "w"))
res = 2.0
X0, X1, Z0, Z1 = -700, 1300, -700, 1300
W, H = int((X1 - X0) / res), int((Z1 - Z0) / res)
top = np.full((H, W), -50.0, np.float32)
_tp = os.path.join(d.cache, "city_top.npy")
if os.path.isfile(_tp):
    top = np.load(_tp); sel = []
from fbmesh import MeshSet, decode_lod
lowcache = {}
rng = np.random.default_rng(0)
for n_i, w in enumerate(sel):
    k = w["mesh"][0] + "/" + w["mesh"][1]
    if k not in lowcache:
        lowcache[k] = None
        e = d.load(w["mesh"][0]); mo = e.by_guid.get(w["mesh"][1]) if e else None
        if mo is not None and mo.type == "RigidMeshAsset":
            data, _ = d.res_bytes(mo["MeshSetResource"])
            try:
                ms = MeshSet(data); li = len(ms.lods) - 1
                ch = d.chunk(ms.lods[li].chunk_id)
                parts = decode_lod(ms, li, ch) if ch else []
                if parts:
                    P = np.concatenate([p["pos"][p["idx"]].reshape(-1, 3) for p in parts])
                    lowcache[k] = P.reshape(-1, 3, 3)
            except Exception:
                pass
    T = lowcache[k]
    if T is None:
        continue
    M = np.array(w["M"]).reshape(4, 4)
    TW = (T.reshape(-1, 3) @ M[:3, :3] + M[3, :3]).reshape(-1, 3, 3)
    a, b, c = TW[:, 0], TW[:, 1], TW[:, 2]
    area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
    ns = np.clip(np.ceil(area / (res * res) * 2), 1, 2000).astype(np.int64)
    tri = np.repeat(np.arange(len(TW)), ns)
    r1, r2 = rng.random(len(tri)), rng.random(len(tri)); s_ = np.sqrt(r1)
    Q = (1 - s_)[:, None] * a[tri] + (s_ * (1 - r2))[:, None] * b[tri] + (s_ * r2)[:, None] * c[tri]
    ix = ((Q[:, 0] - X0) / res).astype(np.int64); jz = ((Z1 - Q[:, 2]) / res).astype(np.int64)
    ok = (ix >= 0) & (jz >= 0) & (ix < W) & (jz < H)
    np.maximum.at(top, (jz[ok], ix[ok]), Q[ok, 1].astype(np.float32))
    if n_i % 5000 == 0:
        print("  splat", n_i, len(sel), flush=True)
if sel:
    np.save(_tp, top)
img = np.clip((top + 10) / 160, 0, 1)
rgb = (np.stack([img, img ** 0.7, img ** 0.4], -1) * 255).astype(np.uint8)
im = Image.fromarray(rgb); dr = ImageDraw.Draw(im)
for x in range(X0, X1 + 1, 100):
    dr.line([((x - X0) / res, 0), ((x - X0) / res, H)], fill=(90, 0, 0)); dr.text(((x - X0) / res + 2, 2), str(x), fill=(255, 80, 80))
for z in range(Z0, Z1 + 1, 100):
    dr.line([(0, (Z1 - z) / res), (W, (Z1 - z) / res)], fill=(90, 0, 0)); dr.text((2, (Z1 - z) / res + 2), str(z), fill=(255, 80, 80))
# candidate scoring
half = int(opt.size / 2 / res); cands = []
for j in range(half, H - half, 10):
    for i in range(half, W - half, 10):
        t = top[j - half:j + half, i - half:i + half]
        roof = (t > 12)
        cov = roof.mean()
        if cov < 0.45:
            continue
        hs = t[roof]
        var = np.std(hs)
        score = cov * min(var, 25) * (1 if np.median(hs) < 110 else 0.3)
        cands.append((score, X0 + i * res, Z1 - j * res, cov, np.median(hs), var))
cands.sort(reverse=True)
picked = []
for c in cands:
    if all(abs(c[1] - p[1]) > opt.size or abs(c[2] - p[2]) > opt.size for p in picked):
        picked.append(c)
    if len(picked) >= 8:
        break
for k, c in enumerate(picked):
    print(f"#{k} centre x={c[1]:.0f} z={c[2]:.0f} roof_cov={c[3]:.2f} median_roof_h={c[4]:.0f} h_std={c[5]:.1f} score={c[0]:.1f}")
    x, z = c[1], c[2]
    dr.rectangle([((x - opt.size / 2 - X0) / res, (Z1 - z - opt.size / 2) / res), ((x + opt.size / 2 - X0) / res, (Z1 - z + opt.size / 2) / res)], outline=(0, 255, 0))
    dr.text(((x - X0) / res, (Z1 - z) / res), f"#{k}", fill=(0, 255, 0))
im.save(opt.out); print("wrote", opt.out)
