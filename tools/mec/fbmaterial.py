"""Mesh material -> texture set resolution (MEC).

Sources, lowest priority first (later ones override; all keyed by the mesh's MeshMaterial):
  1. The referenced SurfaceShaderPreset.ShaderPreset.TextureParameters / VectorParameters
  2. MeshMaterial.Shader (SurfaceShaderInstanceDataStruct).TextureParameters / VectorParameters
  3. MeshVariationDatabase root variation (VariationAssetNameHash 0): Entries[] with
     Mesh=(mesh asset), Materials[].Material=(MeshMaterial) and the *resolved* TextureParameters.
     Like FrostyToolsuite (Viewport/MeshVariationDb.cs, which indexes every database), all 509
     MeshVariationDb_Win32 assets are merged: a mesh's entry often lives only in the database
     of another zone or level, and the per-cell zone database alone left ~1/3 of the materials
     with no textures at all.  The merged table is cached as _cache/mvdb_root.json.

Parameter names follow FrostyToolsuite RenderMesh.cs (MEC branch): albedo = Diffuse*,
normal = Normal* (DXT5nm/BC5 xy), RSM = mask (r: AO, g: smoothness, b/a: metal/emissive mask).
"""
from __future__ import annotations

import json
import os
import time

from fbebx import ExtRef, IntRef

ALBEDO_NAMES = ["diffuse", "material01diffuse", "_d", "diffuse_ao", "basecolor", "albedo", "diffusemap",
                "colormap", "diffusetexture", "basecolormap", "diffusewalkway"]
NORMAL_NAMES = ["normal", "material01normalmap", "_n", "normalmap", "normalrs", "normalwalkway"]
RSM_NAMES = ["rsm", "material01rsm", "_rsm", "rsmwalkway"]
COLOR_NAMES = ["diffusecolor", "color", "basecolor", "tint", "colortint"]


def _params(struct_obj):
    tex, vec = {}, {}
    if struct_obj is None:
        return tex, vec
    for p in struct_obj.get("TextureParameters") or []:
        tex[p["ParameterName"]] = p["Value"]
    for p in struct_obj.get("VectorParameters") or []:
        v = p["Value"]
        vec[p["ParameterName"]] = (v["x"], v["y"], v["z"], v["w"])
    return tex, vec


def _pick(tex: dict, names, contains=(), exclude=("detail", "dirt", "puddle", "mask", "blinds", "interior")):
    low = {k.lower(): k for k in tex}
    for n in names:
        if n in low and isinstance(tex[low[n]], (ExtRef, IntRef)):
            return tex[low[n]]
    for k, v in sorted(tex.items()):
        kl = k.lower()
        if isinstance(v, (ExtRef, IntRef)) and any(c in kl for c in contains) and not any(x in kl for x in exclude):
            return v
    return None


def pick_albedo(tex: dict):
    return _pick(tex, ALBEDO_NAMES, ("diffuse", "albedo", "basecolor"))


def pick_normal(tex: dict):
    return _pick(tex, NORMAL_NAMES, ("normal",))


def pick_rsm(tex: dict):
    return _pick(tex, RSM_NAMES, ("rsm",))


def name_color(shader_name: str):
    """Fallback flat colour for texture-less presets, from the preset name (MEC's palette is
    mostly white/grey with black and coloured accents)."""
    n = (shader_name or "").lower().split("/")[-1]
    table = [("glass", (0.10, 0.12, 0.14, 1.0)), ("black", (0.06, 0.06, 0.06, 1.0)), ("dark", (0.12, 0.12, 0.13, 1.0)),
             ("white", (0.92, 0.92, 0.92, 1.0)), ("red", (0.75, 0.08, 0.06, 1.0)), ("orange", (0.95, 0.45, 0.08, 1.0)),
             ("yellow", (0.95, 0.8, 0.1, 1.0)), ("blue", (0.15, 0.35, 0.75, 1.0)), ("green", (0.2, 0.6, 0.25, 1.0)),
             ("concrete", (0.62, 0.62, 0.6, 1.0)), ("metal", (0.55, 0.56, 0.58, 1.0)), ("grey", (0.5, 0.5, 0.5, 1.0)),
             ("gray", (0.5, 0.5, 0.5, 1.0)), ("plastic", (0.85, 0.85, 0.85, 1.0))]
    for k, c in table:
        if k in n:
            return c
    return None


def pick_color(vec: dict):
    low = {k.lower(): k for k in vec}
    for n in COLOR_NAMES:
        if n in low:
            return vec[low[n]]
    return None


def shader_kind(shader_name: str):
    """Coarse blend class from the shader graph / preset names:
    'opaque' | 'alphatest' | 'glass' | 'decal' | 'emissive' | 'invisible'."""
    s = (shader_name or "").lower()
    if "invisible" in s or "rvo" in s or "nmvisualize" in s:
        return "invisible"
    if "decal" in s:
        return "decal"
    if "glass" in s or "translucent" in s or "transparent" in s:
        return "glass"
    if "alphatest" in s or "alpha_test" in s or "_alpha" in s or "foliage" in s:
        return "alphatest"
    if "emissive" in s:
        return "emissive"
    return "opaque"


class MaterialResolver:
    def __init__(self, dump):
        self.dump = dump
        self.mvdb = {}  # (mesh_inst_guid, material_inst_guid) -> texparams
        self._load_all_mvdb()

    def _load_all_mvdb(self):
        p = os.path.join(self.dump.cache, "mvdb_root.json")
        if os.path.isfile(p):
            with open(p, encoding="utf-8") as fh:
                raw = json.load(fh)
            self.mvdb = {tuple(k.split("|")): {n: ExtRef(*v) for n, v in t.items()} for k, t in raw.items()}
            return
        t0 = time.time()
        files = [g for g, v in self.dump.index.items() if v[1] == "MeshVariationDatabase"]
        for g in files:
            e = self.dump.load(g)
            if e is None or e.primary is None:
                continue
            for ent in e.primary.get("Entries") or []:
                if ent.get("VariationAssetNameHash", 0) != 0:
                    continue
                mesh = ent.get("Mesh")
                if not isinstance(mesh, ExtRef):
                    continue
                for m in ent.get("Materials") or []:
                    mat = m.get("Material")
                    if not isinstance(mat, ExtRef):
                        continue
                    tex = {q["ParameterName"]: q["Value"] for q in (m.get("TextureParameters") or [])
                           if isinstance(q["Value"], ExtRef)}
                    key = (mesh.inst, mat.inst)
                    if key not in self.mvdb or len(tex) > len(self.mvdb[key]):
                        self.mvdb[key] = tex
            self.dump._ebx_cache.clear()
        with open(p, "w", encoding="utf-8") as fh:
            json.dump({"|".join(k): {n: [v.file, v.inst] for n, v in t.items()} for k, t in self.mvdb.items()}, fh)
        print(f"  mesh variation db: {len(files)} databases, {len(self.mvdb)} materials ({time.time() - t0:.0f}s)",
              flush=True)

    def load_mvdb(self, path):  # kept for callers; every database is already merged
        return

    def resolve(self, mesh_ebx, mesh_obj, material_index):
        """-> dict(albedo/normal/rsm = ExtRef|None, color=(r,g,b,a)|None, shader=str, kind=str,
        texnames=[...], source=str, key=str)"""
        mats = mesh_obj.get("Materials") or []
        if material_index >= len(mats):
            return dict(albedo=None, normal=None, rsm=None, color=None, shader="", kind="opaque", texnames=[],
                        source="none", key=f"{mesh_obj.guid}:{material_index}")
        e, mm = self.dump.deref(mesh_ebx, mats[material_index])
        tex, vec = {}, {}
        shader_name = ""
        source = "none"
        if mm is not None:
            s = mm.get("Shader")
            t2, v2 = _params(s)
            if s is not None and s.get("Shader") is not None:
                pe, po = self.dump.deref(e, s.get("Shader"))
                if po is not None:
                    shader_name = po.get("Name") or po.type
                    if po.isa("SurfaceShaderPreset"):
                        t3, v3 = _params(po.get("ShaderPreset"))
                        if t3:
                            source = "preset"
                        tex.update({k: v for k, v in t3.items()})
                        vec.update(v3)
                        sp = po.get("ShaderPreset")
                        if sp is not None and sp.get("Shader") is not None:
                            ge, go = self.dump.deref(pe, sp.get("Shader"))
                            if go is not None:
                                shader_name = (go.get("Name") or "") + " <- " + shader_name
            if t2:
                source = "material"
            tex.update(t2)
            vec.update(v2)
            mv = self.mvdb.get((mesh_obj.guid, mm.guid))
            if mv:
                source = "mvdb"
                tex.update({k: v for k, v in mv.items() if v is not None})

        def ext(r):
            if isinstance(r, IntRef) and e is not None:
                r = ExtRef(e.file_guid, e.instances[r.index].guid)
            return r if isinstance(r, ExtRef) else None

        alb = ext(pick_albedo(tex))
        col = pick_color(vec)
        if col is None and alb is None:
            col = name_color(shader_name)
        return dict(albedo=alb, normal=ext(pick_normal(tex)), rsm=ext(pick_rsm(tex)), color=col,
                    shader=shader_name, kind=shader_kind(shader_name), texnames=sorted(tex.keys()), source=source,
                    key=f"{mesh_obj.guid}:{material_index}")
