"""Frostbite HavokPhysicsData (MEC, hk_2013.3.0-r1 binary packfile) – just enough to read
static-model-group instance transforms.

Res layout (MEC):  [Frostbite header, size = u16 resMeta[0:2]] [packfile #1, 32-bit pointer layout]
[packfile #2, 64-bit layout] ...   We use the 64-bit packfile (layoutRules = 08 01 00 01).

Section header (v11): name[19] 0xFF | absDataStart | localFixups | globalFixups | virtualFixups |
exports | imports | end.  Frostbite leaves the pointers unfixed and the local/global fixup
fields do not point at standard fixup tables, so we only rely on the *virtual* fixups
(src, classSection, classNameOffset) to find object starts + class names, and on the fact
that the packfile writer serialises an object's array payloads right after the object.

hknpStaticCompoundShape (64-bit): instances hkArray at +0x60 (ptr u64, size i32, cap i32);
array payload begins at +0xD0.  hknpShapeInstance = 0x80 bytes:
  +0x00 hkTransform (3 rotation columns + translation, 4 floats each; .w carry packed ints)
  +0x40 scale (vec4)    +0x50 shape ptr ...
"""
from __future__ import annotations

import struct

import numpy as np

MAGIC = b"\x57\xE0\xE0\x57\x10\xC0\xC0\x10"


class Packfile:
    def __init__(self, data: bytes, base: int):
        self.d = data
        self.base = base
        (self.version,) = struct.unpack_from("<i", data, base + 12)
        self.ptr_size = data[base + 16]
        nsec = struct.unpack_from("<i", data, base + 20)[0]
        self.contents_version = data[base + 40:base + 56].split(b"\0")[0].decode()
        pred = struct.unpack_from("<h", data, base + 62)[0] if self.version >= 11 else 0
        p = base + 64 + (pred if self.version >= 11 else 0)
        self.sections = {}
        for _ in range(nsec):
            name = data[p:p + 19].split(b"\0")[0].decode()
            vals = struct.unpack_from("<7i", data, p + 20)
            self.sections[name] = vals
            p += 48 + (16 if self.version >= 11 else 0)
        cn = self.sections["__classnames__"]
        cstart = base + cn[0]
        self.classnames = {}
        q = cstart
        cend = cstart + cn[4] if cn[4] else cstart
        while q < len(data) - 5:
            if data[q:q + 4] == b"\xff\xff\xff\xff":
                break
            q += 5  # signature + 0x09
            e = data.index(b"\0", q)
            self.classnames[q - cstart] = data[q:e].decode("ascii", "replace")
            q = e + 1
            if cn[4] and q >= cend:
                break
        ds = self.sections["__data__"]
        self.data_start = base + ds[0]
        vstart, vend = ds[3], ds[4]
        self.objects = []  # (offset in data section, class name)
        q = self.data_start + vstart
        while q + 12 <= self.data_start + vend:
            src, sec, off = struct.unpack_from("<3i", data, q)
            if src == -1:
                break
            self.objects.append((src, self.classnames.get(off, f"?{off:x}")))
            q += 12
        self.end = self.data_start + ds[6]

    def u32(self, off):
        return struct.unpack_from("<I", self.d, self.data_start + off)[0]


def packfiles(data: bytes):
    out = []
    i = data.find(MAGIC)
    while i != -1:
        try:
            pf = Packfile(data, i)
            out.append(pf)
            i = data.find(MAGIC, pf.end if pf.end > i else i + 8)
        except Exception:
            i = data.find(MAGIC, i + 8)
    return out


def compound_instances(data: bytes):
    """Return list of compound shapes; each = np.ndarray (N,4,4) world matrices (row-vector
    convention rows = right, up, forward, translation, scale applied to rotation rows) plus raw
    w-ints (N,4)."""
    pfs = [p for p in packfiles(data) if p.ptr_size == 8] or packfiles(data)
    if not pfs:
        return []
    pf = pfs[0]
    res = []
    for off, cls in pf.objects:
        if cls != "hknpStaticCompoundShape":
            continue
        a = pf.data_start + off
        n = struct.unpack_from("<i", pf.d, a + 0x68)[0]
        arr = a + 0xD0
        raw = np.frombuffer(pf.d, dtype="<f4", count=n * 32, offset=arr).reshape(n, 32)
        rawi = np.frombuffer(pf.d, dtype="<u4", count=n * 32, offset=arr).reshape(n, 32)
        rot = raw[:, 0:12].reshape(n, 3, 4)[:, :, :3].copy()  # columns? (see below)
        trans = raw[:, 12:15].copy()
        scale = raw[:, 16:19].copy()
        wints = rawi[:, [3, 7, 11, 15]] & 0xFFFFFF
        res.append({"rot": rot, "trans": trans, "scale": scale, "w": wints, "raw": rawi})
    return res
