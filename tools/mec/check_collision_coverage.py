# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Collision coverage: do the surfaces you can see block you?

    uv run --python 3.13 tools/mec/check_collision_coverage.py <arena dir> [--samples 200000] [--tol 0.05]

Samples points (area weighted) on the render triangles of the play volume (inside the play
square, between play_min.y and the lid) and measures the distance from each to the nearest
collision triangle.  Reports the share within --tol for walls (|n.y| < 0.7) and floors, overall
and by material, so gaps (render-only meshes, simplification, budget drops) show up as numbers.
Decals and the far/backdrop groups are left out; tiny props and no-collide classes (foliage,
cables, lamps, signs) are reported but are expected to stay below 100 %.
"""
import argparse
import json
import os
import sys
from collections import defaultdict

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from verify_arena import load_glb  # noqa: E402


def point_tri_dist(P, A, B, C):
    """Distances from points P (k,3) to triangles (m,3) each -> (k, m). Ericson 5.1.5."""
    P = P[:, None, :]
    A, B, C = A[None], B[None], C[None]
    ab, ac, ap = B - A, C - A, P - A
    d1, d2 = (ab * ap).sum(-1), (ac * ap).sum(-1)
    bp = P - B
    d3, d4 = (ab * bp).sum(-1), (ac * bp).sum(-1)
    cp = P - C
    d5, d6 = (ab * cp).sum(-1), (ac * cp).sum(-1)
    va = d3 * d6 - d5 * d4
    vb = d5 * d2 - d1 * d6
    vc = d1 * d4 - d3 * d2
    den = np.where(np.abs(va + vb + vc) < 1e-20, 1e-20, va + vb + vc)
    v = vb / den
    w = vc / den
    Q = A + ab * v[..., None] + ac * w[..., None]  # interior
    # edge / vertex regions
    def edge(X, Y, t):
        return X + (Y - X) * np.clip(t, 0, 1)[..., None]
    t_ab = d1 / np.where(np.abs(d1 - d3) < 1e-20, 1e-20, d1 - d3)
    t_ac = d2 / np.where(np.abs(d2 - d6) < 1e-20, 1e-20, d2 - d6)
    t_bc = (d4 - d3) / np.where(np.abs((d4 - d3) + (d5 - d6)) < 1e-20, 1e-20, (d4 - d3) + (d5 - d6))
    Q = np.where(((vc <= 0) & (d1 >= 0) & (d3 <= 0))[..., None], edge(A, B, t_ab), Q)
    Q = np.where(((vb <= 0) & (d2 >= 0) & (d6 <= 0))[..., None], edge(A, C, t_ac), Q)
    Q = np.where(((va <= 0) & ((d4 - d3) >= 0) & ((d5 - d6) >= 0))[..., None], edge(B, C, t_bc), Q)
    Q = np.where(((d1 <= 0) & (d2 <= 0))[..., None], np.broadcast_to(A, Q.shape), Q)
    Q = np.where(((d3 >= 0) & (d4 <= d3))[..., None], np.broadcast_to(B, Q.shape), Q)
    Q = np.where(((d6 >= 0) & (d5 <= d6))[..., None], np.broadcast_to(C, Q.shape), Q)
    return np.linalg.norm(P - Q, axis=-1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--samples", type=int, default=200000)
    ap.add_argument("--tol", type=float, default=0.05)
    a = ap.parse_args()
    meta = json.load(open(os.path.join(a.dir, "arena.json")))
    lo = np.array(meta.get("play_min", [-1e9] * 3), float)
    hi = np.array(meta.get("play_max", [1e9] * 3), float)
    j, acc, _ = load_glb(os.path.join(a.dir, "arena.glb"))
    tris, names = [], []
    for m in j["meshes"]:
        for p in m["primitives"]:
            mname = j["materials"][p["material"]]["name"] if "material" in p else "?"
            kind, zone = (mname.split(":") + ["", ""])[:2]
            if ":" in mname and (kind == "decal" or zone != "core"):
                continue
            P = acc(p["attributes"]["POSITION"]).astype(np.float64)
            I = acc(p["indices"]).astype(np.int64).reshape(-1, 3)
            T = P[I]
            c = T.mean(1)
            ok = np.all((c >= lo) & (c <= hi), axis=1)
            if ok.any():
                tris.append(T[ok])
                names += [mname] * int(ok.sum())
    T = np.concatenate(tris)
    names = np.array(names)
    area = 0.5 * np.linalg.norm(np.cross(T[:, 1] - T[:, 0], T[:, 2] - T[:, 0]), axis=1)
    nrm = np.cross(T[:, 1] - T[:, 0], T[:, 2] - T[:, 0])
    ny = np.abs(nrm[:, 1]) / np.maximum(np.linalg.norm(nrm, axis=1), 1e-12)
    rng = np.random.default_rng(0)
    pick = rng.choice(len(T), size=a.samples, p=area / area.sum())
    r1, r2 = rng.random(a.samples), rng.random(a.samples)
    s = np.sqrt(r1)
    S = (1 - s)[:, None] * T[pick, 0] + (s * (1 - r2))[:, None] * T[pick, 1] + (s * r2)[:, None] * T[pick, 2]

    cj, cacc, _ = load_glb(os.path.join(a.dir, "collision.glb"))
    cp = cj["meshes"][0]["primitives"][0]
    CP = cacc(cp["attributes"]["POSITION"]).astype(np.float64)
    CI = cacc(cp["indices"]).astype(np.int64).reshape(-1, 3)
    CT = CP[CI]
    cell = 1.0
    grid = defaultdict(list)
    tlo = np.floor((CT.min(1) - a.tol) / cell).astype(np.int64)
    thi = np.floor((CT.max(1) + a.tol) / cell).astype(np.int64)
    for t in range(len(CT)):
        for x in range(tlo[t, 0], thi[t, 0] + 1):
            for y in range(tlo[t, 1], thi[t, 1] + 1):
                for z in range(tlo[t, 2], thi[t, 2] + 1):
                    grid[(x, y, z)].append(t)
    keys = np.floor(S / cell).astype(np.int64)
    order = np.lexsort(keys.T)
    dist = np.full(len(S), np.inf)
    ks = keys[order]
    brk = np.nonzero(np.any(np.diff(ks, axis=0) != 0, axis=1))[0] + 1
    for grp in np.split(order, brk):
        k = tuple(keys[grp[0]])
        ids = grid.get(k)
        if not ids:
            continue
        C = CT[ids]
        for c0 in range(0, len(grp), 256):
            g = grp[c0:c0 + 256]
            dist[g] = point_tri_dist(S[g], C[:, 0], C[:, 1], C[:, 2]).min(1)
    hit = dist <= a.tol
    wall = ny[pick] < 0.7
    print(f"{a.dir}: {len(T)} play-volume render tris, {len(CT)} collision tris, {a.samples} samples, tol {a.tol} m")
    print(f"  covered: all {100 * hit.mean():.2f}%  walls {100 * hit[wall].mean():.2f}%  floors {100 * hit[~wall].mean():.2f}%")
    by = defaultdict(lambda: [0, 0])
    for n, h in zip(names[pick], hit):
        by[n][0] += 1
        by[n][1] += int(h)
    worst = sorted(by.items(), key=lambda kv: -(kv[1][0] - kv[1][1]))[:15]
    print("  largest uncovered materials (samples missed / sampled):")
    for n, (tot, h) in worst:
        if tot - h:
            print(f"    {tot - h:7d} / {tot:7d}  {n}")
    out = {"samples": a.samples, "tol_m": a.tol, "covered_pct": round(100 * hit.mean(), 2),
           "walls_pct": round(100 * hit[wall].mean(), 2), "floors_pct": round(100 * hit[~wall].mean(), 2)}
    st = os.path.join(a.dir, "stats.json")
    if os.path.isfile(st):
        d = json.load(open(st))
        d["collision_coverage"] = out
        json.dump(d, open(st, "w"), indent=1)


if __name__ == "__main__":
    main()
