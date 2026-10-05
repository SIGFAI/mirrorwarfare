# SPDX-License-Identifier: MIT
# Derived from Frostbite-Scripts (c) 2019 NicknineTheEagle, MIT License; see NOTICE.
"""Minimal Frostbite 3 (Mirror's Edge Catalyst) EBX reader.

Port of Frostbite-Scripts/frostbite3/ebx.py (NicknineTheEagle) that produces plain Python
objects instead of text.  MEC ships EBX "version 1" (magic CE D1 B2 0F, little endian).

Representation:
  Ebx.instances : list[Obj]  (Obj.type, Obj.guid (str|None), Obj.f (dict, inheritance flattened))
  value types   : Obj as well (guid None)
  class refs    : None | IntRef(index) | ExtRef(file_guid, inst_guid)
  arrays        : list, enums : str, ResourceRef : int, GUID : str
"""
from __future__ import annotations

import os
import struct
import uuid

_u32 = struct.Struct("<I")


def guid_str(b: bytes) -> str:
    """Frostbite/.NET GUID layout (first three fields LE) -> canonical text."""
    return str(uuid.UUID(bytes_le=bytes(b)))


def fnv1_5381(s: str) -> int:
    h = 5381
    for ch in s:
        h = ((h * 33) ^ ord(ch)) & 0xFFFFFFFF
    return h


class IntRef:
    __slots__ = ("index",)

    def __init__(self, index):
        self.index = index

    def __repr__(self):
        return f"IntRef({self.index})"


class ExtRef:
    __slots__ = ("file", "inst")

    def __init__(self, file, inst):
        self.file = file
        self.inst = inst

    def __repr__(self):
        return f"ExtRef({self.file}/{self.inst})"


class Obj:
    __slots__ = ("type", "types", "guid", "f", "index")

    def __init__(self, typ):
        self.type = typ
        self.types = [typ]
        self.guid = None
        self.f = {}
        self.index = -1

    def get(self, k, d=None):
        return self.f.get(k, d)

    def __getitem__(self, k):
        return self.f[k]

    def isa(self, name):
        return name in self.types

    def __repr__(self):
        return f"<{self.type} {self.guid or self.index}>"


# field types
VOID, DBOBJ, VALUE, CLASS, ARRAY, FIXARRAY, STRING, CSTRING, ENUM, FILEREF, BOOL, I8, U8, I16, U16, I32, U32, I64, U64, F32, F64, GUID, SHA1, RESREF = range(24)
_PRIM = {BOOL: ("<?", 1), I8: ("<b", 1), U8: ("<B", 1), I16: ("<h", 2), U16: ("<H", 2), I32: ("<i", 4),
         U32: ("<I", 4), I64: ("<q", 8), U64: ("<Q", 8), F32: ("<f", 4), F64: ("<d", 8), RESREF: ("<Q", 8)}


class Ebx:
    def __init__(self, path: str, primary_only: bool = False):
        with open(path, "rb") as fh:
            d = fh.read()
        self.d = d
        magic = d[:4]
        if magic == b"\xCE\xD1\xB2\x0F":
            self.version = 1
        elif magic == b"\xCE\xD1\xB4\x0F":
            self.version = 2
        else:
            raise ValueError("not ebx: " + path)
        (self.absStringOffset, self.lenStringToEOF, self.numGUID, numInstRep, self.numGUIDRepeater, _unk,
         numComplex, numField, lenName, lenString, numArrayRep, lenPayload) = struct.unpack_from("<3I6H3I", d, 4)
        self.lenString = lenString
        self.arrayStart = self.absStringOffset + lenString + lenPayload
        self.file_guid = guid_str(d[40:56])
        p = 56
        while p % 16:
            p += 1
        self.ext = []
        for _ in range(self.numGUID):
            self.ext.append((guid_str(d[p:p + 16]), guid_str(d[p + 16:p + 32])))
            p += 32
        kw = d[p:p + lenName].decode("utf-8", "replace").split("\0")
        p += lenName
        kwd = {fnv1_5381(k): k for k in kw}
        self.fields = []
        for _ in range(numField):
            nh, typ, ref, off, off2 = struct.unpack_from("<IHHii", d, p)
            p += 16
            name = kwd.get(nh, f"0x{nh:08x}")
            if name == "$":
                off -= 8
            ft = ((typ >> 4) & 0x1F) if self.version == 1 else ((typ >> 5) & 0x1F)
            self.fields.append((name, ft, ref, off))
        self.complexes = []
        for _ in range(numComplex):
            nh, fstart, nf, align, typ, size, size2 = struct.unpack_from("<IIBBHHH", d, p)
            p += 16
            nfield = nf | ((align << 1) & 0x100)
            self.complexes.append((kwd.get(nh, f"0x{nh:08x}"), fstart, nfield, align & 0x7F, size))
        self.instReps = []
        for _ in range(numInstRep):
            self.instReps.append(struct.unpack_from("<2H", d, p))
            p += 4
        while p % 16:
            p += 1
        self.arrReps = []
        for _ in range(numArrayRep):
            self.arrReps.append(struct.unpack_from("<3I", d, p))
            p += 12
        self._enum_cache = {}
        # payload
        pos = self.absStringOffset + lenString
        self.instances: list[Obj] = []
        self.internal_guids = []
        self.name = None
        for ri, (cidx, reps) in enumerate(self.instReps):
            align = self.complexes[cidx][3] or 1
            for _ in range(reps):
                while pos % align:
                    pos += 1
                g = None
                if ri < self.numGUIDRepeater:
                    g = guid_str(d[pos:pos + 16])
                    pos += 16
                obj, pos = self._read_complex(cidx, pos, True)
                obj.guid = g
                obj.index = len(self.instances)
                self.instances.append(obj)
                if self.name is None:
                    self.name = obj.f.get("Name")
            if primary_only:
                break
        self.primary = self.instances[0] if self.instances else None
        self.by_guid = {o.guid: o for o in self.instances if o.guid}

    # ------------------------------------------------------------------
    def _cstr(self, off):
        if off == -1:
            return None
        a = self.absStringOffset + off
        e = self.d.index(b"\0", a)
        return self.d[a:e].decode("utf-8", "backslashreplace")

    def _read_complex(self, cidx, pos, is_instance=False, into=None):
        name, fstart, nfield, align, size = self.complexes[cidx]
        shift = 8 if (is_instance and align == 4) else 0
        obj = into if into is not None else Obj(name)
        if into is not None:
            obj.types.append(name)
        for fi in range(fstart, fstart + nfield):
            fname, ft, ref, off = self.fields[fi]
            fpos = pos + off - shift
            if ft == VOID:
                self._read_complex(ref, fpos, False, obj)  # flatten inheritance
            else:
                obj.f[fname] = self._read_value(fi, fpos)[0]
        return obj, pos + size - shift

    def _read_value(self, fi, pos):
        d = self.d
        fname, ft, ref, off = self.fields[fi]
        if ft in _PRIM:
            fmt, n = _PRIM[ft]
            return struct.unpack_from(fmt, d, pos)[0], pos + n
        if ft == VALUE:
            return self._read_complex(ref, pos)
        if ft == CLASS:
            v = _u32.unpack_from(d, pos)[0]
            if v >> 31:
                fg, ig = self.ext[v & 0x7FFFFFFF]
                r = ExtRef(fg, ig)
            elif v == 0:
                r = None
            else:
                r = IntRef(v - 1)
            return r, pos + 4
        if ft == ARRAY:
            ai = _u32.unpack_from(d, pos)[0]
            aoff, reps, _c = self.arrReps[ai]
            acname, afstart, _anf, _aal, _asz = self.complexes[ref]
            p2 = self.arrayStart + aoff
            out = []
            for _ in range(reps):
                v, p2 = self._read_value(afstart, p2)
                out.append(v)
            return out, pos + 4
        if ft in (CSTRING, FILEREF):
            off = struct.unpack_from("<i", d, pos)[0]
            return self._cstr(off), pos + 4
        if ft == ENUM:
            v = struct.unpack_from("<i", d, pos)[0]
            if ref not in self._enum_cache:
                en, efs, enf, _a, _s = self.complexes[ref]
                self._enum_cache[ref] = {self.fields[i][3]: self.fields[i][0] for i in range(efs, efs + enf)}
            return self._enum_cache[ref].get(v, v), pos + 4
        if ft == GUID:
            return guid_str(d[pos:pos + 16]), pos + 16
        if ft == SHA1:
            return d[pos:pos + 20].hex(), pos + 20
        return None, pos

    # ------------------------------------------------------------------
    def enum_values(self, enum_name):
        """All values of an enum type declared in this file (name->int)."""
        for cidx, (name, fs, nf, _a, _s) in enumerate(self.complexes):
            if name == enum_name:
                return {self.fields[i][0]: self.fields[i][3] for i in range(fs, fs + nf)}
        return None

    def resolve(self, ref):
        if isinstance(ref, IntRef):
            return self.instances[ref.index]
        return None


def primary_type_and_guid(path):
    """Cheap header scan: (file_guid, primary instance type, Name)."""
    e = Ebx(path, primary_only=True)
    return e.file_guid, (e.primary.type if e.primary else None), e.name
