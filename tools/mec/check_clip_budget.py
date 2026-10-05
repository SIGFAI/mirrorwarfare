# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Replicates iw4L crates/asset_mec/src/clip.rs partitioning (median-split AABB tree, <=24 tris per
leaf, per-leaf vertex dedupe) to check the clipmap limit: < 256 segments x 1024 verts.

    uv run --python 3.13 tools/mec/check_clip_budget.py <arena dir>
"""
import sys, os
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from verify_arena import load_glb

d = sys.argv[1]
j, acc, _ = load_glb(os.path.join(d, "collision.glb"))
p = j["meshes"][0]["primitives"][0]
P = acc(p["attributes"]["POSITION"]).astype(np.float32)
I = acc(p["indices"]).astype(np.int64).reshape(-1, 3)
T = P[I]
cr = np.linalg.norm(np.cross(T[:, 1] - T[:, 0], T[:, 2] - T[:, 0]), axis=1)
T = T[cr * cr > 1e-6]  # package.rs drops |n|^2 <= 1e-6
cen = T.mean(1)
total_verts = 0
stack = [np.arange(len(T))]
leaves = 0
while stack:
    o = stack.pop()
    if len(o) <= 24:
        v = T[o].reshape(-1, 3)
        total_verts += len(np.unique(v, axis=0))
        leaves += 1
        continue
    lo, hi = T[o].reshape(-1, 3).min(0), T[o].reshape(-1, 3).max(0)
    ax = int(np.argmax(hi - lo))
    o = o[np.argsort(cen[o, ax], kind="stable")]
    m = len(o) // 2
    stack += [o[m:], o[:m]]
print(f"{len(T)} tris, {leaves} leaves, {total_verts} clip verts -> {total_verts/1024:.1f} of 256 segments",
      "OK" if total_verts < 255 * 1024 else "OVER LIMIT")
