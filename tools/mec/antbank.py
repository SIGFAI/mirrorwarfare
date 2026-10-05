# SPDX-License-Identifier: GPL-3.0-only
# GenericData layout follows marv7000/AssetBankPlugin (GPL-3.0); see tools/mec/LICENSE-GPL-3.0.
# /// script
# requires-python = ">=3.11"
# ///
"""Mirror's Edge Catalyst ANT ``*.AssetBank`` reader (EA ANT "GenericData" container).

Format notes, decoded movement data and evidence:
``context/artifacts/2026-10-04-mec-ant/README.md``.  Prior art used for the container
layout: marv7000/AssetBankPlugin (Frosty plugin, BF4/BF1) — MEC is the same GenericData
family with 64-bit offsets, big-endian ("GD.xxxxb") sections.

File layout (all big-endian unless noted):
  u32 packaging_type, u32 header_size, header bytes ...         (sections start at 4+header_size)
    packaging 3 header: @0x38 u32 nbytes, @0x3c nbytes/20 x {16-byte EBX AntRef GUID (on-disk
    .NET layout), u32 LE internal id}; then a 12-byte-entry table (streaming chunk slots?), 0xff pad.
  "GD.STRMb" u32 total_size u32 ?     (16 bytes, container marker, no payload of its own)
  "GD.REFLb" u32 size u32 index_off   reflection: i64 count, i64 offsets[count] -> class layouts
  "GD.DATAb" u32 size u32 index_off   one asset per section: 32-byte header
                                      {u64 hash, u64 0, u64 type_hash, u32 0, u16 payload_off, u16},
                                      payload (layout of type_hash), pointer fix-up index at index_off.
  Offsets inside DATA (strings, arrays, DataRefs) are relative to the section payload start.
  Arrays: {u32 capacity, u32 count, i64 offset}.  DataRef: i64 offset of another 32-byte header
  in the same section (``__base`` = base-class sub-object, or an embedded object).
  Guid fields referencing other assets hold internal ids 0x0008xxxx in the first 4 bytes.
  Time unit in the state graph: ticks of 1/60 s (clips are 30 fps with TimeScale 0.5).

Usage (``uv run --python 3.13 tools/mec/antbank.py <cmd> ...``):
  info      BANK                       sections, layout count, asset count
  layouts   BANK [REGEX]               class layouts (fields, types, offsets)
  list      BANK [--type T] [--name RE]  offset, type, internal id, name of every asset (TSV)
  antrefs   BANK                       EBX AntRef GUID -> internal id (+type/name) (TSV)
  dump      BANK OUT.jsonl [--type T] [--name RE] [--full]
                                       decoded assets as JSON lines (big numeric arrays are
                                       summarised unless --full)
  get       BANK ID [ID ...]           one or more assets (by internal id '#0008xxxx') as JSON
  node      BANK NAME|ID [...]         state-flow node: subject sequences/clips/drivers, timeline
                                       tags (branch windows, driver ranges), transitions
  clips     BANK [REGEX]               ClipControllerAsset table (ticks, seconds, root distance)
  rootmotion BANK CLIP_NAME [--every N] root trajectory (DOF 969) for RawAnimationAsset clips
  expr      BANK ID                    decompile an ExpressionAsset kernel (movement rules)
Index-building commands (node, antrefs) decode the whole bank once (~90 s for the 295 MB
sp_maincity bank); pass --cache FILE.pkl to reuse it.
"""
from __future__ import annotations

import argparse
import json
import math
import mmap
import os
import pickle
import re
import struct
import sys
import uuid

TICK_HZ = 60.0
# DOF id of the root/delta trajectory translation channel in MEC character anims (empirical:
# the only channel whose XZ path length equals ClipControllerAsset.Distance; see README).
TRAJ_DOF = 969


class Field:
    __slots__ = ("name", "type_off", "type", "offset", "count", "flags", "elem_size", "elem_align", "layout_hash", "rle")

    def __repr__(self):
        arr = "[]" if self.flags & 1 else ""
        return f"{self.name}:{self.type}{arr}@{self.offset}"


class Layout:
    __slots__ = ("off", "name", "size", "align", "hash", "native", "reordered", "fields", "min_slot", "max_slot")

    def __repr__(self):
        return f"<{self.name} size={self.size} hash={self.hash:#x} {self.fields}>"


def cstr(buf, off):
    end = buf.find(b"\0", off)
    return bytes(buf[off:end]).decode("latin-1")


def guid_text(raw: bytes):
    """Big-endian GUID. Internal ANT ids (only first 4 bytes set) print as '#0008xxxx'."""
    if raw == b"\0" * 16:
        return None
    if raw[4:] == b"\0" * 12:
        return "#%08x" % struct.unpack(">I", raw[:4])[0]
    return raw.hex()


class Bank:
    PRIM = {
        "Bool": ("?", 1), "Int8": ("b", 1), "UInt8": ("B", 1), "Int16": ("h", 2), "UInt16": ("H", 2),
        "Int32": ("i", 4), "UInt32": ("I", 4), "Int64": ("q", 8), "UInt64": ("Q", 8),
        "Float": ("f", 4), "Double": ("d", 8),
    }

    def __init__(self, path):
        self.path = path
        self.f = open(path, "rb")
        self.buf = mmap.mmap(self.f.fileno(), 0, access=mmap.ACCESS_READ)
        b = self.buf
        self.packaging, self.header_size = struct.unpack_from(">II", b, 0)
        pos = 4 + self.header_size
        self.sections = []
        while pos + 16 <= len(b):
            tag = bytes(b[pos:pos + 7]).decode("latin-1")
            if chr(b[pos + 7]) != "b":
                raise ValueError(f"little-endian GD section at {pos:#x} not supported")
            a, c = struct.unpack_from(">II", b, pos + 8)
            if tag == "GD.STRM":
                self.sections.append((tag, pos, a, c))
                pos += 16
            elif tag in ("GD.REFL", "GD.DATA", "GD.REF2"):
                self.sections.append((tag, pos, a, c))
                pos += a
            else:
                raise ValueError(f"unknown section {tag!r} at {pos:#x}")
        self.layouts = {}
        self.layout_by_off = {}
        for s in self.sections:
            if s[0] == "GD.REFL":
                self._read_refl(s[1])
        self.resolve_refs = True

    # ---------------------------------------------------------------- header
    def antref_map(self):
        """{EBX AntRef GUID text (as the EBX text dump prints it): '#0008xxxx'}."""
        out = {}
        if self.packaging != 3:
            return out
        (nbytes,) = struct.unpack_from(">I", self.buf, 0x38)
        p = 0x3C
        for _ in range(nbytes // 20):
            raw = bytes(self.buf[p:p + 16])
            (iid,) = struct.unpack_from("<I", self.buf, p + 16)
            out[str(uuid.UUID(bytes_le=raw))] = "#%08x" % iid
            p += 20
        return out

    # ------------------------------------------------------------------ REFL
    def _read_layout(self, base, off):
        b = self.buf
        p = base + off
        L = Layout()
        L.off = off
        (L.min_slot, L.max_slot, L.size, L.align, st_off, _st_len, L.reordered, L.native, L.hash) = struct.unpack_from(">iiIIIIBB2xI", b, p)
        L.fields = []
        q = p + 0x20
        for _ in range(L.max_slot - L.min_slot + 1):
            lh, es, fo, nm, cnt, fl, ea, rle, lay = struct.unpack_from(">IIIIHHHhq", b, q)
            q += 32
            F = Field()
            F.layout_hash, F.elem_size, F.offset, F.count, F.flags, F.elem_align, F.rle = lh, es, fo, cnt, fl, ea, rle
            F.name = cstr(b, p + st_off + nm)
            F.type_off = lay
            F.type = None
            L.fields.append(F)
        L.name = cstr(b, p + st_off + 1)
        return L

    def _read_refl(self, pos):
        base = pos + 16
        (count,) = struct.unpack_from(">q", self.buf, base)
        for o in struct.unpack_from(f">{count}q", self.buf, base + 8):
            L = self._read_layout(base, o)
            self.layouts[L.hash] = L
            self.layout_by_off[o] = L
        for L in self.layouts.values():
            for F in L.fields:
                t = self.layout_by_off.get(F.type_off)
                F.type = t.name if t else f"?{F.type_off:#x}"

    # ------------------------------------------------------------------ DATA
    def data_sections(self):
        for s in self.sections:
            if s[0] == "GD.DATA":
                yield s[1]

    def root_layout(self, sec_pos):
        _h, _z, typ, _z2, off = struct.unpack_from(">QQQIH", self.buf, sec_pos + 16)
        return self.layouts.get(typ), off

    def peek(self, sec_pos):
        """(type name, internal id, name) without decoding the whole asset."""
        L, off = self.root_layout(sec_pos)
        base = sec_pos + 16
        gid = name = None
        for F in L.fields:
            if F.name == "__guid":
                gid = self.read_value(base, base + off + F.offset, "Guid", None)
            elif F.name == "__name":
                name = self.read_value(base, base + off + F.offset, "String", None)
        return L.name, gid, name

    def read_asset(self, sec_pos):
        """Decode the root object of one GD.DATA section. DataRefs (``__base`` and embedded
        objects such as ActorControllerAsset tracks) are inlined; base-class fields are
        flattened into the object and their class names listed in ``__bases``."""
        out = self.read_embedded(sec_pos + 16, 0)
        out["__off"] = sec_pos
        return out

    def read_embedded(self, base, off, depth=0):
        _h, _z, typ, _z2, poff = struct.unpack_from(">QQQIH", self.buf, base + off)
        L = self.layouts.get(typ)
        if L is None or depth > 16:
            return {"__dataref": off, "__typehash": typ}
        obj = self.read_struct(base, base + off + poff, L, depth)
        out = {"__type": L.name}
        out.update(obj)
        bref = out.pop("__base", None)
        if isinstance(bref, dict):
            for k, v in bref.items():
                if k not in ("__type", "__bases") and k not in out:
                    out[k] = v
            out["__bases"] = [bref.get("__type")] + bref.get("__bases", [])
        return out

    def read_value(self, base, p, tname, tl, depth=0):
        b = self.buf
        if tname in self.PRIM:
            v = struct.unpack_from(">" + self.PRIM[tname][0], b, p)[0]
            return float(f"{v:.7g}") if tname == "Float" else v
        if tname == "Guid":
            return guid_text(bytes(b[p:p + 16]))
        if tname == "String":
            _cap, size, o = struct.unpack_from(">IIq", b, p)
            return bytes(b[base + o:base + o + size]).rstrip(b"\0").decode("latin-1") if size else ""
        if tname == "DataRef":
            off = struct.unpack_from(">q", b, p)[0]
            if not self.resolve_refs or off <= 0:
                return off
            return self.read_embedded(base, off, depth + 1)
        if tname in ("Vector2", "Vector3", "Vector4", "Quaternion"):
            n = {"Vector2": 2, "Vector3": 3, "Quaternion": 4, "Vector4": 4}[tname]
            return [float(f"{x:.7g}") for x in struct.unpack_from(f">{n}f", b, p)]
        if tl is None:
            return None
        return self.read_struct(base, p, tl, depth)

    def read_struct(self, base, p, L, depth=0):
        out = {}
        for F in L.fields:
            if F.count == 0:
                continue
            tl = self.layout_by_off.get(F.type_off)
            q = p + F.offset
            esz = tl.size if tl else F.elem_size
            al = max(tl.align if tl else 1, 1)
            esz = (esz + al - 1) // al * al
            if F.flags & 1:
                _cap, size, o = struct.unpack_from(">IIq", self.buf, q)
                if F.type in self.PRIM and size > 0:
                    vals = list(struct.unpack_from(f">{size}{self.PRIM[F.type][0]}", self.buf, base + o))
                    out[F.name] = [float(f"{x:.7g}") for x in vals] if F.type == "Float" else vals
                else:
                    out[F.name] = [self.read_value(base, base + o + i * esz, F.type, tl, depth) for i in range(size)]
            elif F.count > 1:
                out[F.name] = [self.read_value(base, q + i * esz, F.type, tl, depth) for i in range(F.count)]
            else:
                out[F.name] = self.read_value(base, q, F.type, tl, depth)
        return out


# ---------------------------------------------------------------------- index
def lighten(a, limit=64):
    for k, v in list(a.items()):
        if isinstance(v, list) and len(v) > limit and v and not isinstance(v[0], (str, dict)):
            a[k] = {"__len": len(v)}
    return a


class Index:
    """All assets of a bank decoded once (large numeric arrays summarised); guid -> asset."""

    def __init__(self, bank: Bank, cache=None):
        self.bank = bank
        order = None
        if cache and os.path.exists(cache) and os.path.getmtime(cache) > os.path.getmtime(bank.path):
            with open(cache, "rb") as fh:
                order = pickle.load(fh)
        if order is None:
            order = [lighten(bank.read_asset(s)) for s in bank.data_sections()]
            if cache:
                with open(cache, "wb") as fh:
                    pickle.dump(order, fh)
        self.order = order
        self.by_id = {a["__guid"]: a for a in order if a.get("__guid")}

    def full(self, gid):
        return self.bank.read_asset(self.by_id[gid]["__off"])

    def name(self, gid):
        a = self.by_id.get(gid)
        return f"{gid}<{a['__type']}:{a.get('__name')}>" if a else str(gid)


# ------------------------------------------------------------- graph printing
_SKIP = {"__guid", "__name", "__type", "__bases", "__off", "TagEnable", "Enable", "StartLinkOffset",
         "EndLinkOffset", "LinkFlags", "StartLinkUpdates"}
_NOISE = re.compile(r"MovementImpactTagAsset|FbImpactTagAsset|FbTagAsset|ImpactTimeline|LookAtTag|DamageTag|StrideLength")


class GraphPrinter:
    def __init__(self, ix: Index, out=sys.stdout, noise=False):
        self.ix, self.out, self.noise, self.seen = ix, out, noise, set()

    def p(self, s):
        if self.noise or not _NOISE.search(s):
            print(s, file=self.out)

    def short(self, v):
        if isinstance(v, str) and v.startswith("#"):
            return self.ix.name(v)
        if isinstance(v, list):
            return [self.short(x) for x in v[:8]] + ([f"..{len(v)}"] if len(v) > 8 else [])
        if isinstance(v, dict):
            return {k: self.short(x) for k, x in v.items() if k != "__bases" and x not in (None, [], 0, 0.0, False)}
        return v

    def tag_line(self, t):
        a = self.ix.by_id.get(t)
        if not a:
            return f"{t} ?"
        extra = {k: self.short(v) for k, v in a.items() if k not in _SKIP and v not in (None, [], 0, 0.0, False)}
        so, eo = a.get("StartLinkOffset"), a.get("EndLinkOffset")
        return f"[start {so} len {eo} flags {a.get('LinkFlags')}] {a['__type']} '{a['__name']}' {t} {json.dumps(extra)[:700]}"

    def tagset(self, g, ind):
        s = self.ix.by_id.get(g)
        if not s:
            return
        for c in s.get("TagCollectionKeys", []):
            ca = self.ix.by_id.get(c)
            if not ca:
                continue
            self.p(ind + f"tags {ca['__type']} {c}")
            keys = ca.get("TagKeys", []) or []
            for t in keys + [m for m in (ca.get("MonitoredTagKeys") or []) if m not in keys]:
                self.p(ind + "  " + self.tag_line(t))

    def subject(self, g, ind="", depth=0):
        ix = self.ix
        a = ix.by_id.get(g)
        if not a:
            self.p(ind + f"subject {g} (not in this bank)")
            return
        t = a["__type"]
        hdr = ind + f"subject {t} '{a['__name']}' {g}"
        if depth > 7:
            self.p(hdr + " ...")
            return
        if t == "SequenceContainerAsset":
            self.p(hdr + (" (see above)" if g in self.seen else ""))
            if g in self.seen:
                return
            self.seen.add(g)
            for act in a["ActorAssets"]:
                ac = ix.by_id[act]
                self.p(ind + f"  actor '{ac['__name']}' Length={ac['Length']} ticks = {ac['Length'] / TICK_HZ:.3f} s")
                for tr in ac["Tracks"]:
                    for an in tr.get("Anims", []):
                        c = ix.by_id.get(an["Asset"], {})
                        self.p(ind + f"    clip {ix.name(an['Asset'])} start={an['StartTime']} in=[{an['StartInTime']},{an['EndInTime']}] "
                                     f"scale={an['Scale']} clipTicks={c.get('NumTicks')} rootDist={c.get('Distance')}")
                self.tagset(ac.get("TagCollectionSet"), ind + "    ")
        elif t == "ClipControllerAsset":
            self.p(hdr + f" NumTicks={a['NumTicks']} ({a['NumTicks'] / TICK_HZ:.3f} s) rootDist={a['Distance']}")
        elif t in ("ContextDatabaseChooserWrapperAsset", "ContextDatabaseChooserControllerAsset"):
            self.p(hdr)
            self.subject(a["ContextDatabaseAsset"], ind + "  ", depth + 1)
        elif t == "ContextDatabaseAsset":
            fds = [(f["FieldName"], ix.by_id.get(f.get("GameStateDriverAsset0"), {}).get("__name"), f["FieldType"]) for f in a["FieldDescriptionAssets"]]
            self.p(hdr + " fields(name, gamestate, type)=" + json.dumps(fds))
            for e in a["EntryAssets"]:
                cells = []
                for f, c in zip(fds, e["Cells"]):
                    cells.append(f"{f[0]}[{c['Float0']},{c['Float1']}]" if f[2] in (4, 6) else f"{f[0]}={c['Int0']}")
                self.p(ind + "  entry " + "; ".join(cells))
                self.subject(e["Asset"], ind + "    ", depth + 1)
        elif t in ("ChooserControllerAsset", "BlendMaskChooserControllerAsset"):
            self.p(hdr)
            for e in a["EntryAssetList"]:
                ea = ix.by_id.get(e, {})
                extra = {k: self.short(v) for k, v in ea.items() if k in ("TrueSignals", "FalseSignals", "Weight", "Threshold", "SignalAsset", "EnumerationValueAsset")}
                self.p(ind + f"  choice {ea.get('__type')} {json.dumps(extra)}")
                if ea.get("ControllerAsset"):
                    self.subject(ea["ControllerAsset"], ind + "    ", depth + 1)
        elif t == "LayersControllerAsset":
            self.p(hdr)
            for c in a["ControllerAssets"]:
                self.subject(c, ind + "  ", depth + 1)
        else:
            extra = {k: self.short(v) for k, v in a.items() if k not in _SKIP and v not in (None, [], 0, 0.0, False)}
            self.p(hdr + " " + json.dumps(extra)[:500])
            sub = a.get("SubjectControllerAsset") or a.get("SubjectController") or a.get("ControllerAsset") or a.get("Subject")
            if sub:
                self.subject(sub, ind + "  ", depth + 1)

    def node(self, g):
        ix = self.ix
        a = ix.by_id[g]
        nm = lambda xs: [ix.by_id.get(x, {}).get("__name", x) for x in xs]
        self.p(f"NODE '{a['__name']}' {g} NodeType={a.get('NodeType')}")
        for k in ("EntryConditionsRequiredTrue", "EntryConditionsRequiredFalse"):
            if a.get(k):
                self.p(f"  {k}: {nm(a[k])}")
        if a.get("TagCollectionSet"):
            self.p("  node tags:")
            self.tagset(a["TagCollectionSet"], "    ")
        if a.get("SubjectController"):
            self.subject(a["SubjectController"], "  ")
        for t in a.get("Transitions", []):
            tr = ix.by_id.get(t)
            if not tr:
                continue
            win = ix.by_id.get(tr.get("BranchWindowTypeAsset"), {}).get("__name")
            self.p(f"  -> {ix.name(tr['To'])} if true={nm(tr['ConditionsRequiredTrue'])} false={nm(tr['ConditionsRequiredFalse'])} "
                   f"window={win} atEnd={tr['BranchAtEnd']} afterFirstUpdate={tr['BranchAfterFirstUpdate']} destPhase={tr['DestinationPhase']}")
            if tr.get("TagCollectionSet"):
                self.tagset(tr["TagCollectionSet"], "       ")


# --------------------------------------------------------------- root motion
def raw_root_motion(ix: Index, clip, dof=TRAJ_DOF):
    """[(t_seconds, x, y, z)] of the trajectory channel of a RawAnimationAsset clip
    (Y up, Z forward, metres), or None when the anim is not Raw / has no such channel."""
    an = ix.full(clip["Anims"][0])
    if an["__type"] != "RawAnimationAsset":
        return None
    dofs = ix.full(an["ChannelToDofAsset"])["DofIds"]
    Q, V, Fc = an["QuatCount"], an["Vec3Count"], an["FloatCount"]
    stride = Q * 4 + V * 4 + Fc
    mi = an["MappingIndices"]
    for vi in range(V):
        j = Q + vi
        if dofs[mi[j]] == dof:
            o = Q * 4 + vi * 4
            D = an["Data"]
            fps = clip["FPS"] or 30.0
            return [(an["KeyTimes"][k] / fps, D[k * stride + o], D[k * stride + o + 1], D[k * stride + o + 2]) for k in range(an["NumKeys"])]
    return None


# ---------------------------------------------------------- expression kernel
_FIXED = {30: 6, 31: 7, 28: 7, 27: 6, 32: 8, 34: 4, 20: 4, 36: 4, 38: 4, 33: 7, 21: 3}


def decompile_expression(ix: Index, gid, kernel="ExpressionKernel"):
    """Best-effort decompiler for ANT ExpressionAsset kernels. Opcode formats were inferred
    from the data (see README): 30 = binop(a,b)->out unit; 31 = 3 in; 27/28 = 1 in 2/3 out;
    32 = reader (arg0==896) or 4-input op; 33 = writer; 34/36 = assign; 20 = if-not-goto;
    2 = switch; 3 = generic n-in m-out. Operands are byte offsets into the kernel memory image
    (mCleanState); constants live in that image."""
    bk = ix.bank
    e = ix.full(gid)
    k = e[kernel]
    d, units = k["mUIntData"], k["mUnitNames"]
    # raw clean-state image
    base = ix.by_id[gid]["__off"] + 16
    _h, _z, typ, _z2, poff = struct.unpack_from(">QQQIH", bk.buf, base)
    L = bk.layouts[typ]
    kf = [F for F in L.fields if F.name == kernel][0]
    kref = struct.unpack_from(">q", bk.buf, base + poff + kf.offset)[0]
    _h, _z, ktyp, _z2, kpoff = struct.unpack_from(">QQQIH", bk.buf, base + kref)
    cf = [F for F in bk.layouts[ktyp].fields if F.name == "mCleanState"][0]
    cref = struct.unpack_from(">q", bk.buf, base + kref + kpoff + cf.offset)[0]
    _h, _z, ctyp, _z2, cpoff = struct.unpack_from(">QQQIH", bk.buf, base + cref)
    raw = bytes(bk.buf[base + cref + cpoff: base + cref + cpoff + bk.layouts[ctyp].size])
    hand = {h: ix.by_id.get(v, {}).get("__name", v) for h, v in zip(e["GameStateHandles"], e["GameStateValueAssets"])}
    const_hi = min(e["GameStateHandles"] or [len(raw)])
    written = set()  # filled by a first pass; never-written low offsets are constants

    def sym(o):
        if not isinstance(o, int):
            return str(o)
        if o == 3064:
            return "false"
        if o == 3084:
            return "true"
        if o not in written and o < const_hi and o + 4 <= len(raw):
            u = struct.unpack_from(">I", raw, o)[0]
            return f"#{u}" if u < 0x10000 else "%g" % struct.unpack_from(">f", raw, o)[0]
        return f"m{o}"

    def outputs(f, a):
        if f in (30,):
            return [a[2]]
        if f == 31:
            return [a[3]]
        if f == 27:
            return a[1:3]
        if f == 28:
            return a[1:4]
        if f == 32:
            return [a[4]]
        if f in (34, 36, 38):
            return [a[1]]
        if f == 3:
            n = a[0]
            m = a[1 + n]
            return a[2 + n:2 + n + m]
        return []

    for passno in (0, 1):
        lines, i = [], 6
        while i < len(d):
            f = d[i]
            if f in _FIXED:
                x = d[i:i + _FIXED[f]]
                i += _FIXED[f]
            elif f == 2:
                n = d[i + 3]
                x = d[i:i + 4 + 2 * n]
                i += 4 + 2 * n
            elif f == 3:
                n = d[i + 2]
                m = d[i + 3 + n]
                x = d[i:i + 6 + n + m]
                i += 6 + n + m
            else:
                lines.append(f"?? unknown opcode {f} at word {i}; stop")
                break
            a = x[2:]
            if passno == 0:
                written.update(o for o in outputs(f, a) if isinstance(o, int))
                continue
            try:
                if f == 30:
                    s = f"{sym(a[2])} = {units[a[3]]}({sym(a[0])}, {sym(a[1])})"
                elif f == 31:
                    s = f"{sym(a[3])} = {units[a[4]]}({sym(a[0])}, {sym(a[1])}, {sym(a[2])})"
                elif f == 27:
                    s = f"{sym(a[1])}, {sym(a[2])} = {units[a[3]]}({sym(a[0])})"
                elif f == 28:
                    s = f"{sym(a[1])}, {sym(a[2])}, {sym(a[3])} = {units[a[4]]}({sym(a[0])})"
                elif f == 32 and a[0] == 896:
                    s = f"{sym(a[4])} = READ {hand.get(a[1], a[1])}"
                elif f == 32:
                    s = f"{sym(a[4])} = {units[a[5]]}({sym(a[0])}, {sym(a[1])}, {sym(a[2])}, {sym(a[3])})"
                elif f == 33:
                    s = f"WRITE {hand.get(a[1], a[1])} = {sym(a[3])}"
                elif f in (34, 36, 38):
                    s = f"{sym(a[1])} = {sym(a[0])}" + (f"   (then goto {x[1]})" if f == 36 else "")
                elif f == 20:
                    s = f"if not {sym(a[0])}: goto {a[1]}"
                elif f == 21:
                    s = f"op21 {sym(a[0])}   (unknown 1-operand op)"
                elif f == 2:
                    n = a[1]
                    s = f"switch {sym(a[0])}: " + ", ".join(f"{c}->{t}" for c, t in zip(a[2:2 + n], a[2 + n:2 + 2 * n]))
                else:  # 3
                    n = a[0]
                    ins = a[1:1 + n]
                    m = a[1 + n]
                    outs = a[2 + n:2 + n + m]
                    s = f"{', '.join(sym(o) for o in outs)} = {units[a[3 + n + m]]}({', '.join(sym(o) for o in ins)})"
            except (IndexError, TypeError):
                s = f"?? {x}"
            lines.append(f"{x[1]:4d}  {s}")
    return lines


# ----------------------------------------------------------------------- CLI
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd")
    ap.add_argument("bank")
    ap.add_argument("args", nargs="*")
    ap.add_argument("--type")
    ap.add_argument("--name")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--cache")
    ap.add_argument("--every", type=int, default=1)
    ap.add_argument("--noise", action="store_true", help="node: keep footstep/impact/lookat tags")
    o = ap.parse_args(argv)
    bk = Bank(o.bank)
    out = sys.stdout
    name_re = re.compile(o.name, re.I) if o.name else None

    def want(t, n):
        return (not o.type or t == o.type) and (not name_re or (n and name_re.search(n)))

    if o.cmd == "info":
        n = sum(1 for _ in bk.data_sections())
        print(f"packaging={bk.packaging} header_size={bk.header_size:#x} sections={len(bk.sections)} assets={n} layouts={len(bk.layouts)} antrefs={len(bk.antref_map())}")
    elif o.cmd == "layouts":
        r = re.compile(o.args[0]) if o.args else None
        for L in sorted(bk.layouts.values(), key=lambda L: L.name):
            if not r or r.search(L.name):
                print(f"{L.name}\tsize={L.size}\talign={L.align}\thash={L.hash}\t" + ", ".join(repr(f) for f in L.fields))
    elif o.cmd == "list":
        for s in bk.data_sections():
            t, g, n = bk.peek(s)
            if want(t, n):
                print(f"{s:#x}\t{t}\t{g}\t{n}")
    elif o.cmd == "dump":
        with open(o.args[0], "w", encoding="utf-8") as fh:
            for s in bk.data_sections():
                t, g, n = bk.peek(s)
                if want(t, n):
                    a = bk.read_asset(s)
                    fh.write(json.dumps(a if o.full else lighten(a)) + "\n")
    elif o.cmd == "get":
        ix = Index(bk, o.cache)
        for g in o.args:
            print(json.dumps(ix.full(g) if o.full else lighten(ix.full(g)), indent=1))
    elif o.cmd == "antrefs":
        ix = Index(bk, o.cache)
        for eg, iid in sorted(bk.antref_map().items(), key=lambda kv: kv[1]):
            a = ix.by_id.get(iid, {})
            print(f"{eg}\t{iid}\t{a.get('__type')}\t{a.get('__name')}")
    elif o.cmd == "node":
        ix = Index(bk, o.cache)
        gp = GraphPrinter(ix, noise=o.noise)
        for arg in o.args:
            gs = [arg] if arg.startswith("#") else [a["__guid"] for a in ix.order if a["__type"] == "StateFlowNodeControllerAsset" and a["__name"] == arg]
            for g in gs:
                gp.node(g)
                print()
    elif o.cmd == "clips":
        r = re.compile(o.args[0], re.I) if o.args else None
        print("name\tticks\tseconds\tfps\ttimescale\troot_distance_m\tanims\tid")
        for s in bk.data_sections():
            L, _ = bk.root_layout(s)
            if L.name != "ClipControllerAsset":
                continue
            _, g, n = bk.peek(s)
            if r and not (n and r.search(n)):
                continue
            a = bk.read_asset(s)
            print(f"{n}\t{a['NumTicks']:g}\t{a['NumTicks'] / TICK_HZ:.3f}\t{a['FPS']:g}\t{a['TimeScale']:g}\t{a['Distance']:.3f}\t{len(a['Anims'])}\t{g}")
    elif o.cmd == "rootmotion":
        ix = Index(bk, o.cache)
        for a in ix.order:
            if a["__type"] == "ClipControllerAsset" and a["__name"] == o.args[0]:
                tr = raw_root_motion(ix, a)
                if tr is None:
                    print(f"{a['__guid']} {a['__name']}: anim codec {ix.by_id.get(a['Anims'][0], {}).get('__type')} not decoded")
                    continue
                x0, y0, z0 = tr[0][1:]
                print(f"# {a['__guid']} {a['__name']} ticks={a['NumTicks']} Distance={a['Distance']} (t s, x, y, z m; Y up, Z fwd; relative to frame 0)")
                for k, (t, x, y, z) in enumerate(tr):
                    if k % o.every == 0 or k == len(tr) - 1:
                        print(f"{t:.3f}\t{x - x0:.3f}\t{y - y0:.3f}\t{z - z0:.3f}")
    elif o.cmd == "expr":
        ix = Index(bk, o.cache)
        for line in decompile_expression(ix, o.args[0]):
            print(line)
    else:
        ap.error(f"unknown command {o.cmd}")


if __name__ == "__main__":
    main()
