"""Tiny dependency-free glTF 2.0 binary (GLB) writer for static triangle meshes."""
from __future__ import annotations

import json
import struct

import numpy as np


class GlbBuilder:
    def __init__(self):
        self.bin = bytearray()
        self.j = {"asset": {"version": "2.0", "generator": "iw4L tools/mec"},
                  "scene": 0, "scenes": [{"nodes": []}], "nodes": [], "meshes": [],
                  "buffers": [], "bufferViews": [], "accessors": [], "materials": []}
        self.images = {}

    def _view(self, data: bytes, target=None):
        while len(self.bin) % 4:
            self.bin.append(0)
        off = len(self.bin)
        self.bin += data
        v = {"buffer": 0, "byteOffset": off, "byteLength": len(data)}
        if target:
            v["target"] = target
        self.j["bufferViews"].append(v)
        return len(self.j["bufferViews"]) - 1

    def accessor(self, arr: np.ndarray, kind: str, target=None, minmax=False):
        comp = {np.dtype("float32"): 5126, np.dtype("uint32"): 5125, np.dtype("uint16"): 5123,
                np.dtype("uint8"): 5121}[arr.dtype]
        view = self._view(np.ascontiguousarray(arr).tobytes(), target)
        a = {"bufferView": view, "componentType": comp, "count": int(arr.shape[0]), "type": kind}
        if comp == 5121:
            a["normalized"] = True
        if minmax:
            a["min"] = [float(x) for x in arr.min(0)]
            a["max"] = [float(x) for x in arr.max(0)]
        self.j["accessors"].append(a)
        return len(self.j["accessors"]) - 1

    def image(self, key, data: bytes, mime="image/png"):
        """PNG, or DDS (`image/vnd-ms.dds`, DX10 header, BC1/BC3/BC7 with mips) as
        MSFT_texture_dds does it; arena textures are DDS so the game uploads them as they are."""
        if key in self.images:
            return self.images[key]
        v = self._view(data)
        self.j.setdefault("images", []).append({"bufferView": v, "mimeType": mime, "name": str(key)[:120]})
        self.j.setdefault("samplers", [{"magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497}])
        self.j.setdefault("textures", []).append({"sampler": 0, "source": len(self.j["images"]) - 1})
        self.images[key] = len(self.j["textures"]) - 1
        return self.images[key]

    def material(self, name, color=(0.8, 0.8, 0.8, 1.0), texture=None, double_sided=False, alpha_mode=None,
                 normal=None, specular=None):
        """`specular` (IW4 specular map: rgb = specular colour, a = gloss) rides in the
        metallicRoughnessTexture slot; arena.json `materials` documents it."""
        pbr = {"baseColorFactor": [float(c) for c in color], "metallicFactor": 0.0, "roughnessFactor": 0.9}
        if texture is not None:
            pbr["baseColorTexture"] = {"index": texture}
        if specular is not None:
            pbr["metallicRoughnessTexture"] = {"index": specular}
        m = {"name": name[:120], "pbrMetallicRoughness": pbr, "doubleSided": double_sided}
        if normal is not None:
            m["normalTexture"] = {"index": normal}
        if alpha_mode:
            m["alphaMode"] = alpha_mode
            if alpha_mode == "MASK":
                m["alphaCutoff"] = 0.5
        self.j["materials"].append(m)
        return len(self.j["materials"]) - 1

    def mesh_node(self, name, prims):
        """prims: list of dict(pos (N,3) f32, nrm (N,3)|None, uv (N,2)|None, color (N,4) u8|None,
        idx (M,3) u32, material int|None)"""
        ps = []
        for p in prims:
            attrs = {"POSITION": self.accessor(p["pos"].astype(np.float32), "VEC3", 34962, True)}
            if p.get("nrm") is not None:
                attrs["NORMAL"] = self.accessor(p["nrm"].astype(np.float32), "VEC3", 34962)
            if p.get("uv") is not None:
                attrs["TEXCOORD_0"] = self.accessor(p["uv"].astype(np.float32), "VEC2", 34962)
            if p.get("color") is not None:
                attrs["COLOR_0"] = self.accessor(p["color"].astype(np.uint8), "VEC4", 34962)
            idx = p["idx"].reshape(-1).astype(np.uint32)
            prim = {"attributes": attrs, "indices": self.accessor(idx, "SCALAR", 34963), "mode": 4}
            if p.get("material") is not None:
                prim["material"] = p["material"]
            ps.append(prim)
        self.j["meshes"].append({"name": name, "primitives": ps})
        self.j["nodes"].append({"name": name, "mesh": len(self.j["meshes"]) - 1})
        self.j["scenes"][0]["nodes"].append(len(self.j["nodes"]) - 1)

    def save(self, path):
        if not self.j["materials"]:
            del self.j["materials"]
        if any(im.get("mimeType") == "image/vnd-ms.dds" for im in self.j.get("images", [])):
            self.j["extensionsUsed"] = ["MSFT_texture_dds"]
        while len(self.bin) % 4:
            self.bin.append(0)
        self.j["buffers"] = [{"byteLength": len(self.bin)}]
        js = json.dumps(self.j, separators=(",", ":")).encode()
        while len(js) % 4:
            js += b" "
        total = 12 + 8 + len(js) + 8 + len(self.bin)
        with open(path, "wb") as fh:
            fh.write(struct.pack("<III", 0x46546C67, 2, total))
            fh.write(struct.pack("<II", len(js), 0x4E4F534A))
            fh.write(js)
            fh.write(struct.pack("<II", len(self.bin), 0x004E4942))
            fh.write(self.bin)
