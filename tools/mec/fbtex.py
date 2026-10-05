"""MEC texture res header + chunk mips -> PIL image or GPU-ready block-compressed DDS
(header layout per FrostyToolsuite FrostySdk/Resources/Texture.cs, MEC branch: no 'unknown1').

  u32 mipOffset0, u32 mipOffset1, u32 type, i32 pixelFormat, u16 flags, u16 width, u16 height,
  u16 depth, u16 sliceCount, u8 mipCount, u8 firstMip, guid chunkId, u32 mipSizes[15],
  u32 chunkSize, u32 assetNameHash, char textureGroup[16]

RenderFormat values were inferred from data (bytes/pixel of mip0 + name suffix + sRGB pairs);
2D arrays (type 3): mipSizes are per slice and the chunk is mip-major (verified: mip1 of
slice 0 starts after mip0 of *all* slices).
They match the Frostbite RenderFormat ordering ..., BC1_UNORM=54, BC1_SRGB=55, ...,
BC3_UNORM=60, BC3_SRGB=61, BC4_UNORM=62, BC5_UNORM=63, BC6U=64, BC6S=65, BC7_UNORM=66, BC7_SRGB=67.

Arena textures are written as DDS (DX10 header) so the game uploads them without decoding:
BC1/BC3 colour maps pass straight through from the dump (the game's own mip chain, starting
at the first mip <= the size cap); BC7 colour maps, BC5 normal maps (re-packed for IW4's
DXT5nm read: x in alpha, y in green) and the derived specular maps are encoded here
(numpy, bounding-box endpoints).
"""
from __future__ import annotations

import struct

import numpy as np
from PIL import Image

from fbebx import guid_str

# code -> (pillow bcn n, PIL mode, block bytes) ; None-n => raw
FORMATS = {
    54: (1, "RGBA", 8), 55: (1, "RGBA", 8), 56: (1, "RGBA", 8), 57: (1, "RGBA", 8),
    58: (2, "RGBA", 16), 59: (2, "RGBA", 16),
    60: (3, "RGBA", 16), 61: (3, "RGBA", 16),
    62: (4, "L", 8), 63: (5, "RGB", 16),
    66: (7, "RGBA", 16), 67: (7, "RGBA", 16),
}
# Frostbite RenderFormat -> DXGI (only what is passed through)
PASSTHROUGH = {54: 71, 55: 72, 56: 71, 57: 72, 60: 77, 61: 78}
DXGI_BC1, DXGI_BC1_SRGB, DXGI_BC3, DXGI_BC3_SRGB, DXGI_BC7, DXGI_BC7_SRGB = 71, 72, 77, 78, 98, 99


class TexHeader:
    def __init__(self, b: bytes):
        (self.mip_off0, self.mip_off1, self.type, self.format, self.flags, self.width, self.height,
         self.depth, self.slices, self.mip_count, self.first_mip) = struct.unpack_from("<IIIiHHHHHBB", b, 0)
        self.chunk_id = guid_str(b[0x1C:0x2C])
        self.mip_sizes = list(struct.unpack_from("<15I", b, 0x2C))
        self.chunk_size = struct.unpack_from("<I", b, 0x68)[0]


def mip_levels(header_bytes: bytes, chunk: bytes, max_size=1024):
    """Slice 0 of the mip chain from the first level <= max_size down to the last stored level.
    -> (TexHeader, [(w, h, bytes), ...]) or (h, None)."""
    h = TexHeader(header_bytes)
    if h.type not in (0, 3):
        return h, None
    sizes = h.mip_sizes[:h.mip_count]
    ns = h.slices if (h.type == 3 and h.slices > 1) else 1
    total = sum(sizes) * ns
    start = total - len(chunk) if len(chunk) < total else 0  # chunk may lack the top mips
    off = 0
    w, hh = h.width, h.height
    out = []
    for m in range(h.mip_count):
        if off >= start and (max(w, hh) <= max_size or m == h.mip_count - 1 or out):
            pos = off - start
            data = chunk[pos:pos + sizes[m]]
            if len(data) < sizes[m]:
                break
            out.append((w, hh, data))
        off += sizes[m] * ns
        w, hh = max(1, w // 2), max(1, hh // 2)
    return h, (out or None)


def _decode_level(fmt_code, w, hh, data):
    fmt = FORMATS.get(fmt_code)
    if fmt is None:
        bpp = len(data) / max(1, w * hh)
        if abs(bpp - 4) < 1e-3:
            return Image.frombuffer("RGBA", (w, hh), data, "raw", "RGBA", 0, 1)
        if abs(bpp - 0.5) < 1e-3:
            fmt = (1, "RGBA", 8)
        else:
            return None
    n, mode, _ = fmt
    bw, bh = max(4, (w + 3) // 4 * 4), max(4, (hh + 3) // 4 * 4)
    img = Image.frombuffer(mode, (bw, bh), data, "bcn", n)
    if (bw, bh) != (w, hh):
        img = img.crop((0, 0, w, hh))
    return img


def decode(header_bytes: bytes, chunk: bytes, max_size=1024):
    """First mip <= max_size as an RGBA PIL image (BC5: R, G = x, y; B = 0)."""
    h, levels = mip_levels(header_bytes, chunk, max_size)
    if not levels:
        return None
    w, hh, data = levels[0]
    img = _decode_level(h.format, w, hh, data)
    if img is None:
        return None
    return img.convert("RGBA") if img.mode != "RGBA" else img


# ----------------------------------------------------------------------------- DDS
def dds(dxgi: int, w: int, h: int, levels: list[bytes]) -> bytes:
    """DDS with a DX10 header, 2D, len(levels) mips."""
    block = 8 if dxgi in (DXGI_BC1, DXGI_BC1_SRGB) else 16
    pitch = max(1, (w + 3) // 4) * block
    flags = 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000 | (0x20000 if len(levels) > 1 else 0)
    hdr = struct.pack("<4sIIIIIII44x", b"DDS ", 124, flags, h, w, pitch * max(1, (h + 3) // 4), 0, len(levels))
    hdr += struct.pack("<II4s20x", 32, 0x4, b"DX10")  # pixel format
    hdr += struct.pack("<IIIII", 0x1000 | (0x400000 if len(levels) > 1 else 0) | 0x8, 0, 0, 0, 0)  # caps
    hdr += struct.pack("<IIIII", dxgi, 3, 0, 1, 0)  # DX10: format, TEXTURE2D, misc, array 1, misc2
    return hdr + b"".join(levels)


def _blocks(a: np.ndarray):
    """(H, W, C) -> (H/4 * W/4, 16, C), H and W padded to multiples of 4 by edge repeat."""
    H, W = a.shape[:2]
    ph, pw = (-H) % 4, (-W) % 4
    if ph or pw:
        a = np.pad(a, ((0, ph), (0, pw), (0, 0)), mode="edge")
        H, W = a.shape[:2]
    C = a.shape[2]
    return a.reshape(H // 4, 4, W // 4, 4, C).transpose(0, 2, 1, 3, 4).reshape(-1, 16, C)


def _rgb565(c):
    c = np.clip(np.round(c), 0, 255).astype(np.int32)
    return ((c[:, 0] * 31 + 127) // 255 << 11) | ((c[:, 1] * 63 + 127) // 255 << 5) | ((c[:, 2] * 31 + 127) // 255)


def _unpack565(v):
    r = ((v >> 11) & 31) * 255 / 31
    g = ((v >> 5) & 63) * 255 / 63
    b = (v & 31) * 255 / 31
    return np.stack([r, g, b], 1)


def _color_block(B):
    """BC1 colour half (4-colour mode) for (N, 16, 3) float blocks -> (N, 8) uint8."""
    lo, hi = B.min(1), B.max(1)
    inset = (hi - lo) / 16.0
    c0, c1 = _rgb565(hi - inset), _rgb565(lo + inset)
    swap = c0 < c1
    c0, c1 = np.where(swap, c1, c0), np.where(swap, c0, c1)
    p0, p1 = _unpack565(c0), _unpack565(c1)
    pal = np.stack([p0, p1, (2 * p0 + p1) / 3, (p0 + 2 * p1) / 3], 1)  # (N,4,3)
    d = ((B[:, :, None, :] - pal[:, None, :, :]) ** 2).sum(-1)  # (N,16,4)
    idx = d.argmin(-1).astype(np.uint32)
    idx[c0 == c1] = 0
    bits = (idx << (2 * np.arange(16, dtype=np.uint32))).sum(1).astype(np.uint32)
    out = np.zeros((len(B), 8), np.uint8)
    out[:, 0:2] = c0.astype("<u2").view(np.uint8).reshape(-1, 2)
    out[:, 2:4] = c1.astype("<u2").view(np.uint8).reshape(-1, 2)
    out[:, 4:8] = bits.astype("<u4").view(np.uint8).reshape(-1, 4)
    return out


def _alpha_block(A):
    """BC4/BC3-alpha half (8-value mode) for (N, 16) float blocks -> (N, 8) uint8."""
    a0 = np.clip(np.round(A.max(1)), 0, 255).astype(np.int32)
    a1 = np.clip(np.round(A.min(1)), 0, 255).astype(np.int32)
    same = a0 == a1
    a0 = np.where(same, np.minimum(a0 + 1, 255), a0)
    a1 = np.where(same & (a0 == 255), 254, a1)
    k = np.arange(8)
    w0 = np.array([7, 0, 6, 5, 4, 3, 2, 1]) / 7.0
    pal = a0[:, None] * w0[None, :] + a1[:, None] * (1 - w0[None, :])  # code k -> value
    idx = np.abs(A[:, :, None] - pal[:, None, :]).argmin(-1).astype(np.uint64)
    bits = (idx << (3 * np.arange(16, dtype=np.uint64))).sum(1).astype(np.uint64)
    out = np.zeros((len(A), 8), np.uint8)
    out[:, 0] = a0
    out[:, 1] = a1
    b6 = bits.astype("<u8").view(np.uint8).reshape(-1, 8)[:, :6]
    out[:, 2:8] = b6
    _ = k
    return out


def encode_bc1(rgba: np.ndarray) -> bytes:
    B = _blocks(rgba[:, :, :3].astype(np.float32))
    return _color_block(B).tobytes()


def encode_bc3(rgba: np.ndarray) -> bytes:
    B = _blocks(rgba.astype(np.float32))
    out = np.concatenate([_alpha_block(B[:, :, 3]), _color_block(B[:, :, :3])], 1)
    return out.tobytes()


def mip_chain(img: Image.Image, min_edge=4):
    """img, img/2, ... down to min_edge (box filtered)."""
    out = [img]
    w, h = img.size
    while max(w, h) > min_edge and min(w, h) > 1:
        w, h = max(1, w // 2), max(1, h // 2)
        out.append(out[-1].resize((w, h), Image.BOX))
    return out


def encode_dds(img: Image.Image, bc3: bool, srgb: bool) -> bytes:
    """RGBA PIL image (edges multiples of 4) -> BC1/BC3 DDS with a full mip chain."""
    levels = []
    for m in mip_chain(img.convert("RGBA")):
        a = np.asarray(m, np.uint8)
        levels.append(encode_bc3(a) if bc3 else encode_bc1(a))
    dx = (DXGI_BC3_SRGB if srgb else DXGI_BC3) if bc3 else (DXGI_BC1_SRGB if srgb else DXGI_BC1)
    return dds(dx, img.size[0], img.size[1], levels)


def passthrough_dds(header_bytes: bytes, chunk: bytes, max_size: int):
    """BC1/BC3 colour map straight from the dump -> (dds bytes, w, h, dxgi) or None."""
    h = TexHeader(header_bytes)
    dx = PASSTHROUGH.get(h.format)
    if dx is None:
        return None
    _, levels = mip_levels(header_bytes, chunk, max_size)
    if not levels:
        return None
    w, hh = levels[0][0], levels[0][1]
    if w % 4 or hh % 4:
        return None
    keep = [d for (lw, lh, d) in levels if lw >= 1 and lh >= 1]
    return dds(dx, w, hh, keep), w, hh, dx
