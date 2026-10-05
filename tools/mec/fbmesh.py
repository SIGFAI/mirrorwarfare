"""MEC MeshSet res + chunk decoding (layout follows FrostyToolsuite
Plugins/MeshSetPlugin/Resources/MeshSet.cs, ProfileVersion.MirrorsEdgeCatalyst branches).

Only what is needed for static geometry: LOD0 (or any LOD) sections -> positions, normals,
uv0, triangle indices.  Coordinates are returned in Frostbite space (left-handed, Y up,
meters); conversion to glTF happens in the arena builder.
"""
from __future__ import annotations

import struct

import numpy as np

from fbebx import guid_str

# VertexElementUsage
U_POS, U_NORMAL, U_TANGENT, U_BINORMAL, U_BINSIGN = 0x01, 0x06, 0x07, 0x08, 0x09
U_TEX0, U_TANGENTSPACE = 0x21, 0x34
# VertexElementFormat
F_FLOAT2, F_FLOAT3, F_FLOAT4 = 0x02, 0x03, 0x04
F_HALF2, F_HALF3, F_HALF4 = 0x06, 0x07, 0x08
F_UBYTE4N, F_USHORT4N, F_UINT = 0x0D, 0x19, 0x20
F_SHORT2N = 0x13

CAT_OPAQUE, CAT_TRANSPARENT, CAT_DECAL, CAT_ZONLY, CAT_SHADOW = range(5)


class Section:
    pass


class Lod:
    pass


class MeshSet:
    def __init__(self, data: bytes):
        self.d = d = data
        r = Reader(d)
        self.bbox_min = r.vec3()
        self.bbox_max = r.vec3()
        lod_offsets = [r.i64() for _ in range(6)]
        r.i64()
        full_off = r.i64()
        name_off = r.i64()
        self.name_hash = r.u32()
        self.mesh_type = r.u32()
        r.skip(12 * 2)  # lod fade distance factors
        self.flags = r.u32()
        lod_count = r.u16()
        self.section_count = r.u16()
        self.fullname = cstr(d, full_off)
        self.name = cstr(d, name_off)
        self.lods = []
        for i in range(lod_count):
            self.lods.append(self._read_lod(lod_offsets[i]))

    def _read_lod(self, off):
        d = self.d
        r = Reader(d, off)
        L = Lod()
        L.type = r.u32()
        L.max_instances = r.u32()
        nsec = r.i32()
        sec_off = r.i64()
        L.categories = []
        for _ in range(5):
            cnt = r.i32()
            coff = r.i64()
            L.categories.append(set(d[coff:coff + cnt]) if cnt else set())
        L.flags = r.u32()
        L.index_format = r.i32()
        L.ib_size = r.u32()
        L.vb_size = r.u32()
        r.i32()  # adjacency size
        L.chunk_id = guid_str(r.raw(16))
        L.inline_off = r.u32()
        r.i64()
        s1, s2, s3 = r.i64(), r.i64(), r.i64()
        L.name = cstr(d, s2)
        L.sections = []
        r2 = Reader(d, sec_off)
        for si in range(nsec):
            L.sections.append(self._read_section(r2, si))
        return L

    def _read_section(self, r, idx):
        S = Section()
        S.index = idx
        r.i64()
        r.i64()
        soff = r.i64()
        S.material_id = r.i32()
        r.u32()
        S.prim_count = r.u32()
        S.start_index = r.u32()
        S.vertex_offset = r.u32()
        S.vertex_count = r.u32()
        S.stride = r.u8()
        S.prim_type = r.u8()
        r.u8()
        r.u8()
        r.skip(12)
        r.u8()
        r.u16()
        r.u8()
        r.i64()  # bone list
        decls = []
        for _ in range(2):
            elems = [struct.unpack_from("<4B", r.d, r.p + 4 * i) for i in range(16)]
            r.skip(64)
            # MEC: GeometryDeclarationDesc.MaxStreams = 8 (Frosty default branch)
            streams = [struct.unpack_from("<2B", r.d, r.p + 2 * i) for i in range(8)]
            r.skip(16)
            ecount, scount = r.u8(), r.u8()
            r.skip(2)
            decls.append((elems[:ecount], streams))
        S.decl = decls[0]
        r.skip(24 + 36)
        S.name = cstr(self.d, soff)
        return S


def cstr(d, off):
    if off <= 0 or off >= len(d):
        return ""
    e = d.index(b"\0", off)
    return d[off:e].decode("utf-8", "replace")


class Reader:
    def __init__(self, d, p=0):
        self.d, self.p = d, p

    def _u(self, fmt, n):
        v = struct.unpack_from(fmt, self.d, self.p)[0]
        self.p += n
        return v

    def u8(self): return self._u("<B", 1)
    def u16(self): return self._u("<H", 2)
    def u32(self): return self._u("<I", 4)
    def i32(self): return self._u("<i", 4)
    def i64(self): return self._u("<q", 8)

    def vec3(self):
        v = struct.unpack_from("<3f", self.d, self.p)
        self.p += 16
        return v

    def raw(self, n):
        v = self.d[self.p:self.p + n]
        self.p += n
        return v

    def skip(self, n):
        self.p += n


# ---------------------------------------------------------------------------
def _face_normals(pos, idx):
    """Area-weighted vertex normals from the triangles (pos (N,3), idx (M,3))."""
    n = np.zeros_like(pos, dtype=np.float64)
    a, b, c = pos[idx[:, 0]], pos[idx[:, 1]], pos[idx[:, 2]]
    f = np.cross(b - a, c - a)
    for k in range(3):
        np.add.at(n, idx[:, k], f)
    n /= np.maximum(np.linalg.norm(n, axis=1, keepdims=True), 1e-12)
    return n.astype(np.float32)


def _read_elem(buf, base, count, stride, off, fmt):
    """Gather one vertex element for `count` vertices -> float32 array."""
    if count == 0:
        return None
    if fmt in (F_FLOAT2, F_FLOAT3, F_FLOAT4):
        n = {F_FLOAT2: 2, F_FLOAT3: 3, F_FLOAT4: 4}[fmt]
        dt = np.dtype("<f4")
        size = 4 * n
    elif fmt in (F_HALF2, F_HALF3, F_HALF4):
        n = {F_HALF2: 2, F_HALF3: 3, F_HALF4: 4}[fmt]
        dt = np.dtype("<f2")
        size = 2 * n
    elif fmt == F_UBYTE4N:
        n, dt, size = 4, np.dtype("u1"), 4
    elif fmt == F_USHORT4N:
        n, dt, size = 4, np.dtype("<u2"), 8
    elif fmt == F_SHORT2N:
        n, dt, size = 2, np.dtype("<i2"), 4
    else:
        return None
    a = np.frombuffer(buf, dtype=np.uint8, count=stride * count, offset=base)
    a = a.reshape(count, stride)[:, off:off + size].copy()
    v = a.view(dt).reshape(count, n).astype(np.float32)
    if fmt == F_UBYTE4N:
        v /= 255.0
    elif fmt == F_USHORT4N:
        v /= 65535.0
    elif fmt == F_SHORT2N:
        v /= 32767.0
    return v


def decode_lod(ms: MeshSet, lod_index: int, chunk_bytes: bytes | None, inline: bytes | None = None,
               categories=(CAT_OPAQUE, CAT_TRANSPARENT)):
    """-> list of dict(section, material_id, name, pos (N,3), nrm (N,3)|None, uv (N,2)|None,
    idx (M,3) int32, category)."""
    L = ms.lods[lod_index]
    buf = chunk_bytes if chunk_bytes is not None else inline
    if buf is None:
        return []
    total_idx = sum(s.prim_count * 3 for s in L.sections)
    isz = 4 if (total_idx and L.ib_size >= total_idx * 4) else 2
    out = []
    for s in L.sections:
        cat = None
        for c in categories:
            if s.index in L.categories[c]:
                cat = c
                break
        if cat is None or s.prim_count == 0 or not s.name or s.prim_type != 3:
            continue
        elems, streams = s.decl
        # stream base offsets: streams stored back to back per section
        bases = []
        b = s.vertex_offset
        for (stride, _cls) in streams:
            bases.append(b)
            b += stride * s.vertex_count
        pos = nrm = uv = ts = None
        for (usage, fmt, off, si) in elems:
            if usage == 0 or si >= len(streams):
                continue
            stride = streams[si][0]
            if stride == 0:
                continue
            if bases[si] + stride * s.vertex_count > len(buf):
                continue
            if usage == U_POS and pos is None:
                v = _read_elem(buf, bases[si], s.vertex_count, stride, off, fmt)
                pos = v[:, :3] if v is not None else None
            elif usage == U_NORMAL and nrm is None:
                v = _read_elem(buf, bases[si], s.vertex_count, stride, off, fmt)
                nrm = v[:, :3] if v is not None else None
            elif usage == U_TEX0 and uv is None:
                v = _read_elem(buf, bases[si], s.vertex_count, stride, off, fmt)
                uv = v[:, :2] if v is not None else None
            elif usage == U_TANGENTSPACE and ts is None and fmt in (F_UBYTE4N, F_USHORT4N):
                ts = _read_elem(buf, bases[si], s.vertex_count, stride, off, fmt)
        if pos is None:
            continue
        ioff = L.vb_size + s.start_index * isz
        n = s.prim_count * 3
        if ioff + n * isz > len(buf):
            continue
        idx = np.frombuffer(buf, dtype="<u4" if isz == 4 else "<u2", count=n, offset=ioff).astype(np.int64)
        if idx.size and idx.max() >= s.vertex_count:
            # some meshes store absolute indices
            idx = idx - idx.min()
            if idx.max() >= s.vertex_count:
                continue
        if nrm is None:
            nrm = _face_normals(pos, idx.reshape(-1, 3))
        out.append(dict(section=s.index, material_id=s.material_id, name=s.name, pos=pos, nrm=nrm, uv=uv,
                        idx=idx.reshape(-1, 3).astype(np.int32), category=cat))
    return out
