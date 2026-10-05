# SPDX-License-Identifier: GPL-3.0-only
# The Dct decoder is a port of marv7000/AssetBankPlugin (GPL-3.0); see tools/mec/LICENSE-GPL-3.0.
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Mirror's Edge Catalyst ANT animation clips: decode, validate, export.

Builds on ``antbank.py`` (container + reflection).  Codecs:

* ``RawAnimationAsset``  (CodecType 'Raw ') key frames of f32 quats/vec3s/floats + constants.
* ``DctAnimationAsset``  (CodecType 'Dct ') 8-frame blocks, per-DOF DCT-II coefficients packed
  MSB-first in 64-bit slices; port of marv7000/AssetBankPlugin ``DctDecompressor.cs``.
* ``CurveAnimationAsset`` key-reduced curves (layout as documented by IceBloc).
* ``VbrAnimationAsset``  (CodecType 'Vbr ') — see ``decode_vbr`` (reverse-engineered here).

Channel -> DOF: ``ChannelToDofAsset.DofIds`` (quats, then vec3s, then floats; Raw uses
``MappingIndices``).  DOF -> joint channel name + bind value: Faith's ``RigAsset`` (#000813bd):
``DofIds`` parallel to the concatenated slots of ``RigDofSets`` (start = ``DofSetIdIndices``),
bind pose in ``DefaultVector3Values`` / ``DefaultVector4Values``.  Joint hierarchy:
``SkeletonAsset`` 'Faith'.  Units: metres, Y up, Z forward (character space).

Usage (``uv run --python 3.13 tools/mec/antanim.py <cmd> ...``):
  validate  BANK                     decode Raw/Dct/Vbr twins of the same clip and compare
  export    BANK OUTDIR [CLIP ...]   write OUTDIR/<clip>.json (+ skeleton.json); default clip set
                                     = Faith's parkour moves (CLIPS below)
  plot      OUTDIR CLIP [N]          stick-figure PNG of N frames (matplotlib)
"""
from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from antbank import Bank, Index  # noqa: E402

FAITH_RIG = "#000813bd"
FAITH_SKELETON = "#00083f71"
TRAJ_DOF = 969

# Faith's third-person parkour clips (ClipControllerAsset names in the sp_maincity bank).
CLIPS = [
    "WallRunLeft", "WallRunRight", "WallRunVertical", "WallRunVertical180Turn",
    "WallRunJumpLeft", "WallRunJumpRight",
    "VaultOver", "VaultOverFast", "VaultOverFastLong", "VaultOverHigh", "VaultOverHighFast",
    "VaultOverClose", "VaultOnto", "VaultOntoFast", "VaultOntoLow", "VaultOntoHigh",
    "VaultOntoHighCrouch", "VaultSpringBoard", "VaultSpringBoardLedge",
    "CrouchSlide", "CrouchSlideHard", "CrouchSlideToCrouch", "CrouchSlideEnd",
    "FallingLandRoll", "FallingLandRollFast", "FallingLandFailNoDamage", "FallingLandFailMedium",
    "FallingLandFail",
    "RunFwdLand", "Loco_Land",
    "HangStart", "HangStartWallClimb", "HangHeaveUp", "HangHeaveOver",
    "JumpCoil", "StandRunTurn180Left", "JumpTurn180", "CrouchTurn180Left",
    "JumpStill", "JumpFast3", "JumpFastLoop", "JumpMedium", "Fall",
    "Loco_Sprint_Fwd", "Loco_Run_Start_Fwd", "Loco_Run_Stop_Fwd",
]


# ------------------------------------------------------------------- rig
class Rig:
    def __init__(self, ix: Index, rig_id=FAITH_RIG, skel_id=FAITH_SKELETON):
        r = ix.full(rig_id)
        self.dof_name = {}
        dofs = r["DofIds"]
        for set_id, start in zip(r["RigDofSets"], r["DofSetIdIndices"]):
            if start == 0xFFFF:
                continue
            s = ix.full(set_id)
            for k, slot in enumerate(s.get("Slots") or []):
                if start + k < len(dofs):
                    self.dof_name[dofs[start + k]] = slot["Name"]
        self.default = {d["DofId"]: d["Value"] for d in r["DefaultVector3Values"]}
        self.default.update({d["DofId"]: d["Value"] for d in r["DefaultVector4Values"]})
        self.default.update({d["DofId"]: d["Value"] for d in r["DefaultFloatValues"]})
        sk = ix.full(skel_id)
        self.joints = [j["JointName"] for j in sk["Joints"]]
        self.parents = [j["ParentIndex"] for j in sk["Joints"]]
        self.name_dof = {v: k for k, v in self.dof_name.items()}

    def bind(self):
        """Local bind (q xyzw, t) per joint from the rig defaults."""
        out = []
        for j in self.joints:
            q = self.default.get(self.name_dof.get(j + ".q"), [0, 0, 0, 1])
            t = self.default.get(self.name_dof.get(j + ".t"), [0, 0, 0])
            out.append((list(q), list(t)[:3]))
        return out


# ---------------------------------------------------------------- codecs
def decode_raw(a, c2d):
    """-> (key_times, {dof: ndarray[keys, n]})"""
    Q, V, F = a["QuatCount"], a["Vec3Count"], a["FloatCount"]
    CQ, CV, CF = a["ConstQuatCount"], a["ConstVec3Count"], a["ConstFloatCount"]
    n = a["NumKeys"]
    mi = a["MappingIndices"]
    stride = Q * 4 + V * 4 + (F + 3) // 4 * 4
    D = np.asarray(a["Data"], dtype=np.float64).reshape(n, stride) if n else np.zeros((0, stride))
    C = np.asarray(a["ConstData"], dtype=np.float64)
    out = {}
    j = 0
    for i in range(Q):
        out[c2d[mi[j]]] = D[:, i * 4:i * 4 + 4]; j += 1
    for i in range(V):
        o = Q * 4 + i * 4
        out[c2d[mi[j]]] = D[:, o:o + 3]; j += 1
    for i in range(F):
        o = Q * 4 + V * 4 + i
        out[c2d[mi[j]]] = D[:, o:o + 1]; j += 1
    o = 0
    for i in range(CQ):
        out[c2d[mi[j]]] = np.repeat(C[None, o:o + 4], n, 0); o += 4; j += 1
    for i in range(CV):
        out[c2d[mi[j]]] = np.repeat(C[None, o:o + 3], n, 0); o += 4; j += 1
    for i in range(CF):
        out[c2d[mi[j]]] = np.repeat(C[None, o:o + 1], n, 0); o += 1; j += 1
    return list(a["KeyTimes"]), out


_DCT = np.array([[math.cos((2 * x + 1) * u * math.pi / 16) * (0.25 if u == 0 else 0.5)
                  for u in range(8)] for x in range(8)])


class _Bits:
    """MSB-first bit reader (the 64-bit big-endian slices of the C# reader reduce to this)."""

    def __init__(self, data):
        self.v = int.from_bytes(bytes(data), "big")
        self.n = len(data) * 8
        self.p = 0

    def sread(self, nb):
        if nb == 0:
            return 0
        x = (self.v >> (self.n - self.p - nb)) & ((1 << nb) - 1)
        self.p += nb
        return x - (1 << nb) if x >> (nb - 1) else x


def decode_dct(a, c2d):
    nq, nv, nfv, nf = a["NumQuats"], a["NumVec3"], a["NumFloatVec"], a["NumFloat"]
    ndof = nq + nv + nfv
    nkeys = a["NumKeys"]
    desc, bps = a["DofTableDescBytes"], a["BitsPerSubblock"]
    base = np.array([a["DeltaBaseX"], a["DeltaBaseY"], a["DeltaBaseZ"], a["DeltaBaseW"]], dtype=np.int64).T
    catch = a["CatchAllBitCount"]
    tables, k = [], 0
    for i in range(ndof):
        nsub = (desc[i] >> 4) & 0xF
        tables.append(bps[k:k + nsub]); k += nsub
    br = _Bits(a["Data"])
    nblocks = (nkeys + 7) // 8
    coef = np.zeros((nblocks, ndof, 8, 4))
    for b in range(nblocks):
        for d in range(ndof):
            comps = tables[d]
            start = 1 if b == 0 else 0
            blk = np.zeros((8, 4), dtype=np.int64)
            for s, bits in enumerate(comps[start:], start):
                for c, sh in enumerate((12, 8, 4, 0)):
                    nb = (bits >> sh) & 0xF
                    if nb == 0xF:
                        nb = catch
                    blk[s, c] = br.sread(nb)
            blk[0] += base[d]
            blk = ((blk + 0x8000) & 0xFFFF) - 0x8000  # C# (short) wrap
            coef[b, d] = blk
    mult = np.array([(a["QuantizeMultSubblock"] * 0.1 * i + 1.0) / a["QuantizeMultBlock"] for i in range(8)])
    vals = np.zeros((nkeys, ndof, 4))
    for f in range(nkeys):
        w = _DCT[f % 8] * mult  # [8]
        vals[f] = np.einsum("s,dsc->dc", w, coef[f // 8])
    out = {}
    for i in range(nq):
        out[c2d[i]] = vals[:, i, :]
    for i in range(nv):
        out[c2d[nq + i]] = vals[:, nq + i, :3]
    for i in range(nf):
        if nq + nv + i < len(c2d):
            out[c2d[nq + nv + i]] = vals[:, nq + nv + i // 4, i % 4:i % 4 + 1]
    return list(a["KeyTimes"]), out


def decode_curve(a, c2d):
    """Key-reduced curves: one row per key in `Values`, rotations (4 floats each) first,
    then vectors (3 floats each)."""
    nr, nv = a["NumRotations"], a["NumVectors"]
    width = nr * 4 + nv * 3
    keys = list(a["Keys"])
    rows = np.asarray(a["Values"][: len(keys) * width], dtype=np.float64).reshape(len(keys), width)
    out = {c2d[i]: rows[:, 4 * i:4 * i + 4] for i in range(nr)}
    out.update({c2d[nr + j]: rows[:, 4 * nr + 3 * j:4 * nr + 3 * j + 3] for j in range(nv)})
    return keys, out


def decode(ix, anim_id):
    a = ix.full(anim_id)
    c2d = ix.full(a["ChannelToDofAsset"])["DofIds"]
    t = a["__type"]
    if t == "RawAnimationAsset":
        return a, decode_raw(a, c2d)
    if t == "DctAnimationAsset":
        return a, decode_dct(a, c2d)
    if t == "CurveAnimationAsset":
        return a, decode_curve(a, c2d)
    if t == "VbrAnimationAsset":
        return a, decode_vbr(a, c2d)
    raise ValueError(f"codec {t} not supported")


def decode_vbr(a, c2d):
    raise NotImplementedError("Vbr")


# ---------------------------------------------------------------- helpers
def find_clip(ix, name):
    for a in ix.order:
        if a["__type"] == "ClipControllerAsset" and a["__name"] == name and a["Anims"]:
            if a["Anims"][0] in ix.by_id:
                return a
    return None


def resample(times, chan, kind, fps_out_times):
    """Linear (slerp-ish nlerp for quats) resample of key-framed channels to frame times."""
    times = np.asarray(times, dtype=np.float64)
    out = np.zeros((len(fps_out_times), chan.shape[1]))
    for i, t in enumerate(fps_out_times):
        k = int(np.searchsorted(times, t, side="right") - 1)
        k = max(0, min(k, len(times) - 1))
        if k + 1 < len(times) and times[k + 1] > times[k]:
            u = (t - times[k]) / (times[k + 1] - times[k])
            a, b = chan[k], chan[k + 1]
            if kind == "q" and np.dot(a, b) < 0:
                b = -b
            v = a * (1 - u) + b * u
        else:
            v = chan[k]
        if kind == "q":
            v = v / (np.linalg.norm(v) or 1)
        out[i] = v
    return out


# ---------------------------------------------------------------- export
# Body joints kept in the pack (Faith names); parents are re-resolved inside the subset.
BODY_JOINTS = [
    "AITrajectory", "Hips", "Spine", "Spine1", "Spine2", "Neck", "Neck1", "Head", "HeadEnd",
    "LeftShoulder", "LeftArm", "LeftForeArm", "LeftHand",
    "RightShoulder", "RightArm", "RightForeArm", "RightHand",
    "LeftUpLeg", "LeftLeg", "LeftFoot", "LeftToeBase", "LeftToe",
    "RightUpLeg", "RightLeg", "RightFoot", "RightToeBase", "RightToe",
]
INCH = 39.3701


def f2iw_v(v):
    """Faith character space (X left, Y up, Z forward, m) -> IW4 (X fwd, Y left, Z up, in)."""
    return [v[2] * INCH, v[0] * INCH, v[1] * INCH]


def f2iw_q(q):
    """Same proper rotation applied to a quaternion (x, y, z, w)."""
    return [q[2], q[0], q[1], q[3]]


def clip_frames(ix, rig, clip):
    """-> dict with per-frame local rotations of BODY_JOINTS (IW4 axes), Hips translation,
    trajectory translation; frames uniformly at the clip FPS from 0 to EndFrame."""
    a, (times, ch) = decode(ix, clip["Anims"][0])
    fps = clip["FPS"] or 30.0
    end = a["EndFrame"] if a.get("EndFrame") else (times[-1] if times else 0)
    ft = list(range(0, int(end) + 1))
    bind = rig.bind()
    jidx = {n: i for i, n in enumerate(rig.joints)}
    qs, missing = [], []
    for name in BODY_JOINTS:
        d = rig.name_dof.get(name + ".q")
        if name != "AITrajectory" and d in ch:
            v = resample(times, ch[d], "q", ft)
        else:
            if name != "AITrajectory":
                missing.append(name)
            v = np.repeat(np.asarray([bind[jidx[name]][0]], dtype=np.float64), len(ft), 0)
        qs.append(v)
    q = np.stack(qs, 1)  # [F, J, 4]
    d = rig.name_dof.get("Hips.t")
    hips = resample(times, ch[d], "v", ft) if d in ch else np.repeat(np.asarray([bind[jidx["Hips"]][1]]), len(ft), 0)
    d = rig.name_dof.get("AITrajectory.t")
    traj = resample(times, ch[d], "v", ft) if d in ch else np.zeros((len(ft), 3))
    d = rig.name_dof.get("AITrajectory.q")
    trq = resample(times, ch[d], "q", ft) if d in ch else np.repeat(np.asarray([[0, -0.70710678, 0, 0.70710678]]), len(ft), 0)
    r6 = lambda xs: [round(float(x), 6) for x in xs]
    return {
        "codec": a["__type"].replace("AnimationAsset", ""),
        "fps": fps,
        "ticks": clip["NumTicks"],
        "seconds": clip["NumTicks"] / 60.0,
        "distance_m": clip["Distance"],
        "frames": len(ft),
        "missing": missing,
        "q": [[f2iw_q(r6(q[f, j])) for j in range(q.shape[1])] for f in range(len(ft))],
        "hips_t": [f2iw_v(r6(hips[f])) for f in range(len(ft))],
        "traj_t": [f2iw_v(r6(traj[f] - traj[0])) for f in range(len(ft))],
        "traj_q": [f2iw_q(r6(trq[f])) for f in range(len(ft))],
    }


def skeleton_json(rig):
    bind = rig.bind()
    jidx = {n: i for i, n in enumerate(rig.joints)}
    out = []
    for name in BODY_JOINTS:
        i = jidx[name]
        p = rig.parents[i]
        pname = rig.joints[p] if p >= 0 else None
        while pname is not None and pname not in BODY_JOINTS:
            p = rig.parents[jidx[pname]]
            pname = rig.joints[p] if p >= 0 else None
        q, t = bind[i]
        if name == "AITrajectory":
            t = [0, 0, 0]
        out.append({"name": name, "parent": BODY_JOINTS.index(pname) if pname else -1,
                    "bind_q": f2iw_q(q), "bind_t": f2iw_v(t)})
    return out


def _qmul(a, b):
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return np.array([aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx,
                     aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz])


def _qrot(q, v):
    x, y, z, w = q
    u = np.array([x, y, z])
    t = 2 * np.cross(u, v)
    return v + w * t + np.cross(u, t)


def world_positions(skel, q_frame, hips_t):
    """Joint positions (IW4 axes, inches) of one frame; AITrajectory pinned at bind."""
    n = len(skel)
    wq, wp = [None] * n, [None] * n
    for i, j in enumerate(skel):
        lq = np.array(j["bind_q"]) if j["name"] == "AITrajectory" else np.array(q_frame[i])
        lt = np.array(hips_t) if j["name"] == "Hips" else np.array(j["bind_t"])
        if j["parent"] < 0:
            wq[i], wp[i] = lq, lt
        else:
            p = j["parent"]
            wq[i] = _qmul(wq[p], lq)
            wp[i] = wp[p] + _qrot(wq[p], lt)
    return np.array(wp)


def plot_clip(outdir, name, nshow=6):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    skel = json.load(open(os.path.join(outdir, "skeleton.json")))["joints"]
    c = json.load(open(os.path.join(outdir, "clips", name + ".json")))
    F = c["frames"]
    picks = sorted(set(int(round(k * (F - 1) / max(nshow - 1, 1))) for k in range(nshow)))
    fig, axes = plt.subplots(2, len(picks), figsize=(2.2 * len(picks), 6.4), squeeze=False)
    for col, f in enumerate(picks):
        P = world_positions(skel, c["q"][f], c["hips_t"][f])
        for row in (0, 1):
            ax = axes[row][col]
            for i, j in enumerate(skel):
                if j["parent"] > 0:
                    a, b = P[j["parent"]], P[i]
                    colr = "tab:red" if "Left" in j["name"] else ("tab:blue" if "Right" in j["name"] else "k")
                    # row 0: side view (x forward to the right); row 1: front view (viewer faces the body,
                    # body's left (+y) appears on the viewer's right)
                    xa, xb = (a[0], b[0]) if row == 0 else (a[1], b[1])
                    ax.plot([xa, xb], [a[2], b[2]], color=colr, lw=2)
            ax.plot([-30, 30], [0, 0], color="0.7", lw=1)
            ax.set_xlim(-40, 40)
            ax.set_ylim(-5, 75)
            ax.set_aspect("equal")
            ax.set_xticks([])
            ax.set_yticks([])
            ax.set_title(("side" if row == 0 else "front") + f" f{f}/{F - 1}", fontsize=7)
    fig.suptitle(f"{name} ({c['codec']}, {c['seconds']:.2f} s) red=left blue=right", fontsize=9)
    fig.tight_layout()
    os.makedirs(os.path.join(outdir, "png"), exist_ok=True)
    out = os.path.join(outdir, "png", name + ".png")
    fig.savefig(out, dpi=80)
    plt.close(fig)
    return out


# Twins: the same clip stored with two codecs in the sp_maincity bank (Raw id, compressed id).
TWINS = [("Melee_Stand_Kick_Right01", "#00083969", "#00083108"),
         ("Melee_Stand_Kick_Back", "#00082dc8", "#00086e95"),
         ("Loco_Land", "#00083b08", "#000807d9")]


def validate(ix, rig):
    body = ["Hips.q", "Spine.q", "Spine1.q", "Spine2.q", "Neck.q", "Head.q", "LeftUpLeg.q", "LeftLeg.q", "LeftFoot.q",
            "RightUpLeg.q", "RightLeg.q", "RightFoot.q", "LeftArm.q", "LeftForeArm.q", "RightArm.q", "RightForeArm.q"]
    for name, raw_id, cmp_id in TWINS:
        _, (tr, dr) = decode(ix, raw_id)
        ac, (tc, dc) = decode(ix, cmp_id)
        norms = np.concatenate([np.linalg.norm(v, axis=1) for d, v in dc.items() if v.shape[1] == 4])
        print(f"{name}: {ac['__type']} keys {len(tc)} (raw {len(tr)}); |q| {norms.min():.4f}..{norms.max():.4f}")
        for nm in body + ["Hips.t"]:
            d = rig.name_dof[nm]
            a = resample(tr, dr[d], "q" if nm.endswith(".q") else "v", tc)
            b = dc[d]
            if nm.endswith(".q"):
                b = b / np.linalg.norm(b, axis=1)[:, None]
                e = np.degrees(2 * np.arccos(np.abs(np.sum(a * b, axis=1)).clip(0, 1)))
                print(f"   {nm:16s} median {np.median(e):6.2f} deg  p90 {np.percentile(e, 90):6.2f}  max {e.max():6.2f}")
            else:
                e = np.abs(a - b).max(axis=1) * 1000
                print(f"   {nm:16s} median {np.median(e):6.2f} mm   p90 {np.percentile(e, 90):6.2f}  max {e.max():6.2f}")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd")
    ap.add_argument("args", nargs="*")
    ap.add_argument("--cache")
    ap.add_argument("--png", action="store_true", help="export: also render stick-figure PNGs")
    o = ap.parse_args(argv)
    if o.cmd == "plot":
        print(plot_clip(o.args[0], o.args[1], int(o.args[2]) if len(o.args) > 2 else 6))
        return
    bk = Bank(o.args[0])
    ix = Index(bk, o.cache)
    rig = Rig(ix)
    if o.cmd == "export":
        outdir = o.args[1]
        names = o.args[2:] or CLIPS
        os.makedirs(os.path.join(outdir, "clips"), exist_ok=True)
        with open(os.path.join(outdir, "skeleton.json"), "w") as fh:
            json.dump({"version": 1, "units": "inch", "axes": "iw4: x forward, y left, z up",
                       "source": "Mirror's Edge Catalyst Faith rig #000813bd", "joints": skeleton_json(rig)}, fh, indent=1)
        index = {}
        for name in names:
            clip = find_clip(ix, name)
            if clip is None:
                print(f"{name}: no clip")
                continue
            try:
                c = clip_frames(ix, rig, clip)
            except (NotImplementedError, ValueError) as e:
                print(f"{name}: skipped ({e!r})")
                continue
            c["name"] = name
            with open(os.path.join(outdir, "clips", name + ".json"), "w") as fh:
                json.dump(c, fh, separators=(",", ":"))
            index[name] = {k: c[k] for k in ("codec", "fps", "frames", "seconds", "ticks", "distance_m")}
            print(f"{name}\t{c['codec']}\t{c['frames']} frames\t{c['seconds']:.3f} s\tmissing={c['missing']}")
            if o.png:
                plot_clip(outdir, name)
        with open(os.path.join(outdir, "index.json"), "w") as fh:
            json.dump({"version": 1, "clips": index}, fh, indent=1)
    elif o.cmd == "validate":
        validate(ix, rig)
    else:
        ap.error(f"unknown command {o.cmd}")


if __name__ == "__main__":
    main()
