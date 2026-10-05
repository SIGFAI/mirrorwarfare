"""Per-vertex light bake for arena packages: sky visibility (ambient occlusion) and sun visibility.

The render triangles are voxelised into an occupancy grid; each vertex marches a fixed set of
cosine-distributed hemisphere rays (sky) and one ray against the sun through it.  Large
triangles are split first so walls and floors carry enough vertices for contact shadows.
"""
from __future__ import annotations

import math

import numpy as np


def subdivide(pos, nrm, uv, idx, max_edge):
    """Split triangles on their longest edge while it is longer than max_edge and the triangle is
    not a sliver (area > max_edge^2 / 4).  New vertices are edge midpoints, shared between
    neighbours of the same primitive."""
    pos, nrm, uv = [np.asarray(a, np.float32) for a in (pos, nrm, uv)]
    idx = np.asarray(idx, np.int64).reshape(-1, 3)
    done = []
    edge_ids = {}
    P, N, U = [pos], [nrm], [uv]
    count = len(pos)
    for _ in range(12):
        if not len(idx):
            break
        allp = np.concatenate(P) if len(P) > 1 else P[0]
        a, b, c = allp[idx[:, 0]], allp[idx[:, 1]], allp[idx[:, 2]]
        L = np.stack([np.linalg.norm(b - a, axis=1), np.linalg.norm(c - b, axis=1), np.linalg.norm(a - c, axis=1)], 1)
        k = L.argmax(1)
        area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
        big = (L[np.arange(len(L)), k] > max_edge) & (area > 0.25 * max_edge * max_edge)
        done.append(idx[~big])
        idx, k = idx[big], k[big]
        if not len(idx):
            break
        # rotate so the longest edge is (v0, v1)
        r = np.stack([idx[np.arange(len(idx)), k], idx[np.arange(len(idx)), (k + 1) % 3],
                      idx[np.arange(len(idx)), (k + 2) % 3]], 1)
        e0, e1 = np.minimum(r[:, 0], r[:, 1]), np.maximum(r[:, 0], r[:, 1])
        keys = e0 * (1 << 31) + e1
        uk, inv = np.unique(keys, return_inverse=True)
        mid = np.empty(len(uk), np.int64)
        fresh = []
        for i, key in enumerate(uk.tolist()):
            m = edge_ids.get(key)
            if m is None:
                m = count + len(fresh)
                edge_ids[key] = m
                fresh.append(i)
            mid[i] = m
        if fresh:
            fresh = np.array(fresh)
            first = np.zeros(len(uk), np.int64)
            first[inv] = np.arange(len(inv))
            va, vb = r[first[fresh], 0], r[first[fresh], 1]
            allN, allU = (np.concatenate(N), np.concatenate(U))
            P.append(((allp[va] + allp[vb]) * 0.5).astype(np.float32))
            n = allN[va] + allN[vb]
            n /= np.maximum(np.linalg.norm(n, axis=1, keepdims=True), 1e-9)
            N.append(n.astype(np.float32))
            U.append(((allU[va] + allU[vb]) * 0.5).astype(np.float32))
            count += len(fresh)
        m = mid[inv]
        idx = np.concatenate([np.stack([r[:, 0], m, r[:, 2]], 1), np.stack([m, r[:, 1], r[:, 2]], 1)])
    done.append(idx)
    return np.concatenate(P), np.concatenate(N), np.concatenate(U), np.concatenate(done).astype(np.uint32)


class Voxels:
    def __init__(self, lo, hi, cell):
        self.lo = np.asarray(lo, np.float64)
        self.cell = cell
        self.dim = np.maximum(np.ceil((np.asarray(hi) - self.lo) / cell).astype(np.int64), 1)
        self.occ = np.zeros(int(np.prod(self.dim)), bool)
        self.stride = np.array([self.dim[1] * self.dim[2], self.dim[2], 1], np.int64)

    def add_triangles(self, T, rng):
        """Mark voxels touched by area samples of the triangles (N,3,3)."""
        a, b, c = T[:, 0], T[:, 1], T[:, 2]
        area = 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1)
        ns = np.clip(np.ceil(area / (self.cell * self.cell) * 6.0), 3, 200000).astype(np.int64)
        for s in range(0, len(T), 20000):
            sl = slice(s, s + 20000)
            tri = np.repeat(np.arange(s, min(s + 20000, len(T))), ns[sl])
            r1, r2 = rng.random(len(tri)), rng.random(len(tri))
            q = np.sqrt(r1)
            Q = (1 - q)[:, None] * a[tri] + (q * (1 - r2))[:, None] * b[tri] + (q * r2)[:, None] * c[tri]
            # include the corners so thin slivers still land
            Q = np.concatenate([Q, a[sl], b[sl], c[sl]])
            ok, flat = self._flat(Q)
            self.occ[flat[ok]] = True

    def _flat(self, Q):
        g = np.floor((Q - self.lo) / self.cell).astype(np.int64)
        ok = np.all((g >= 0) & (g < self.dim), axis=1)
        g = np.clip(g, 0, self.dim - 1)
        return ok, g @ self.stride

    def march(self, O, D, t0, t1, step):
        """Fraction-free hit test: True where a ray O + tD hits an occupied voxel for t in [t0, t1]."""
        hit = np.zeros(len(O), bool)
        alive = np.ones(len(O), bool)
        t = t0
        while t <= t1 and alive.any():
            idx = np.nonzero(alive)[0]
            ok, flat = self._flat(O[idx] + D[idx] * t if D.ndim == 2 else O[idx] + D * t)
            h = ok & self.occ[flat]
            hit[idx[h]] = True
            alive[idx[h]] = False
            # left the grid sideways or above: open sky
            alive[idx[~ok]] = False
            t += step
        return hit


def hemisphere(n=16):
    """Cosine-weighted directions about +Y (glTF up), deterministic spiral."""
    out = []
    golden = math.pi * (3 - math.sqrt(5))
    for i in range(n):
        r = math.sqrt((i + 0.5) / n)
        phi = i * golden
        out.append((r * math.cos(phi), math.sqrt(max(0.0, 1 - r * r)), r * math.sin(phi)))
    return np.array(out)


def tangent_frames(N):
    up = np.where(np.abs(N[:, 1:2]) < 0.9, np.array([[0.0, 1.0, 0.0]]), np.array([[1.0, 0.0, 0.0]]))
    T = np.cross(up, N)
    T /= np.maximum(np.linalg.norm(T, axis=1, keepdims=True), 1e-9)
    B = np.cross(N, T)
    return T, B


def bake(voxels, P, N, sun_dir, rays=16, ao_range=12.0, sun_range=240.0, log=print, coarse=None,
         coarse_ao_range=40.0):
    """-> (sky (V,), sun (V,)) in [0, 1].  sun_dir is the direction light travels (glTF).

    `voxels` is the fine grid (contact shadows, ao_range); `coarse`, when given, is a wider low
    resolution grid of everything around (towers, backdrop) that rays continue through from
    two coarse cells out, so distant buildings still shade the sky and cast the sun shadow.
    Vertices outside the fine grid use the coarse grid alone."""
    P = np.asarray(P, np.float64)
    N = np.asarray(N, np.float64)
    N /= np.maximum(np.linalg.norm(N, axis=1, keepdims=True), 1e-9)
    cell = voxels.cell
    fine_ok = np.all((P >= voxels.lo) & (P < voxels.lo + voxels.dim * cell), axis=1)

    def blocked_along(O_f, O_c, D, fine_range, coarse_range, sel):
        hit = np.zeros(len(O_f), bool)
        f = sel & fine_ok
        if f.any():
            hit[f] = voxels.march(O_f[f], D[f] if D.ndim == 2 else D, cell, fine_range, cell)
        if coarse is not None:
            rest = sel & ~hit
            if rest.any():
                cc = coarse.cell
                start = np.where(fine_ok[rest], 2.0 * cc, 1.5 * cc)
                # march from the per-vertex start: shift the origin, march [0, range - start]
                Dr = D[rest] if D.ndim == 2 else np.broadcast_to(D, (int(rest.sum()), 3))
                O2 = O_c[rest] + Dr * start[:, None]
                hit[rest] = coarse.march(O2, Dr, 0.0, coarse_range, cc)
        return hit

    O = P + N * (cell * 0.75)
    Oc = P + N * ((coarse.cell if coarse is not None else cell) * 0.75)
    T, B = tangent_frames(N)
    H = hemisphere(rays)
    sky = np.zeros(len(P))
    wsum = np.zeros(len(P))
    every = np.ones(len(P), bool)
    for hx, hy, hz in H:
        D = T * hx + N * hy + B * hz
        # an upward-biased skylight: rays toward the ground count for less
        w = 0.35 + 0.65 * np.clip(D[:, 1] * 0.5 + 0.5, 0, 1)
        blocked = blocked_along(O, Oc, D, ao_range, coarse_ao_range, every)
        sky += np.where(blocked, 0.0, w)
        wsum += w
    sky = sky / np.maximum(wsum, 1e-9)
    L = -np.asarray(sun_dir, np.float64)
    L /= np.linalg.norm(L)
    facing = N @ L
    sun = np.zeros(len(P))
    lit = facing > 0.0
    if lit.any():
        blocked = blocked_along(O, Oc, L, sun_range if coarse is None else 30.0, sun_range, lit)
        sun[lit] = np.where(blocked[lit], 0.0, 1.0)
    log(f"bake: {len(P)} vertices ({int(fine_ok.sum())} in the fine grid), {rays} sky rays, "
        f"mean sky {sky.mean():.2f}, sunlit {sun.mean():.2f}")
    return sky.astype(np.float32), sun.astype(np.float32)
