"""MEC open-world static placement extraction.

Where static meshes live (SP_MainCity):
  * Zone-streamer cells  Levels/SP/SP_MainCity/<Set>_ZS_ZS_<n>  (SubWorldData, e.g. CentralCityBuildings_*,
    CentralCityProps_*) each hold one StaticModelGroupEntityData.  MemberDatas[i]:
      MeshAsset (RigidMeshAsset), MemberType (ObjectBlueprint), InstanceCount,
      InstanceTransforms (LinearTransform[] - only filled for members WITHOUT physics),
      PhysicsPartRange First..Last (physics part index == instance index of the top-level
      hknpStaticCompoundShape in the cell's GroupHavokAsset HavokPhysicsData res).
    => member with physics: transforms come from the Havok packfile (see fbhavok.py);
       member without physics: transforms are InstanceTransforms in EBX.
  * Layers  <cell>_Layer (WorldPartData) + other WorldPartData: SpatialPrefabReferenceObjectData /
    ObjectReferenceObjectData with BlueprintTransform -> prefab (SpatialPrefabBlueprint.Objects) or
    ObjectBlueprint(Object = StaticModelEntityData{Mesh}).  The cooked SMGs already contain everything
    "groupable"; the layers keep "nongroupable_autogen" prefabs (glass, doors, lights, ...).

Transforms are Frostbite LinearTransform rows (right, up, forward, trans); p_world = p_local @ M
(row-vector 4x4).  Composition: M_world = M_child @ M_parent.
"""
from __future__ import annotations

import json
import os
import sys
import time

import numpy as np

from fbebx import ExtRef, IntRef, Obj
from fbhavok import compound_instances

LEVEL_PREFIX = "levels/sp/sp_maincity/"


def lt_to_mat(lt) -> np.ndarray:
    M = np.eye(4, dtype=np.float64)
    if lt is None:
        return M
    f = lt.f if isinstance(lt, Obj) else lt
    for i, k in enumerate(("right", "up", "forward", "trans")):
        v = f[k]
        vf = v.f if isinstance(v, Obj) else v
        M[i, :3] = (vf["x"], vf["y"], vf["z"])
    return M


def smg_instances(dump, ebx):
    """All instances of the StaticModelGroupEntityData objects in a SubWorldData ebx.
    -> list of (mesh_ref(ExtRef), member_type_ref, 4x4 matrix, source tag)"""
    out = []
    smgs = [o for o in ebx.instances if o.type == "StaticModelGroupEntityData"]
    if not smgs:
        return out
    havok = [o for o in ebx.instances if o.isa("HavokAsset") and o.type == "GroupHavokAsset"]
    hk = None
    if havok:
        data, _meta = dump.res_bytes(havok[0]["Resource"])
        if data:
            try:
                comps = compound_instances(data)
                hk = comps[0] if comps else None
            except Exception as e:  # noqa
                print("  havok parse failed", ebx.name, e, file=sys.stderr)
    for smg in smgs:
        base = lt_to_mat(smg.get("Transform"))
        for mi, m in enumerate(smg["MemberDatas"]):
            mesh = m.get("MeshAsset")
            if mesh is None:
                continue
            n = m["InstanceCount"]
            its = m.get("InstanceTransforms") or []
            pr = m["PhysicsPartRange"]
            first, last = pr["First"], pr["Last"]
            per = max(1, m.get("PhysicsPartCountPerInstance") or 1)
            for k in range(n):
                M = None
                tag = "ebx"
                if k < len(its):
                    M = lt_to_mat(its[k])
                elif hk is not None and first != 0xFFFFFFFF:
                    p = first + k * per
                    if p < len(hk["trans"]):
                        M = np.eye(4)
                        R = hk["rot"][p].astype(np.float64)  # rows: right, up, forward (hk columns)
                        S = hk["scale"][p].astype(np.float64)
                        M[:3, :3] = R * S[:, None]
                        M[3, :3] = hk["trans"][p]
                        tag = "havok"
                if M is None:
                    continue
                out.append((mesh, m.get("MemberType"), M @ base, tag))
    return out


def _blueprint_static_meshes(dump, bp_ebx, bp_obj, M, depth, out, seen):
    """Recurse through prefab/object blueprints collecting (mesh_ref, matrix)."""
    if depth > 8 or bp_obj is None:
        return
    if bp_obj.isa("ObjectBlueprint"):
        e2, o2 = dump.deref(bp_ebx, bp_obj.get("Object"))
        if o2 is not None:
            mesh = o2.get("Mesh")
            if isinstance(mesh, (ExtRef, IntRef)) and (o2.isa("StaticModelEntityData") or o2.isa("StaticModelEntityData")):
                if isinstance(mesh, IntRef):
                    mesh = ExtRef(e2.file_guid, e2.instances[mesh.index].guid)
                out.append((mesh, None, M, "prefab"))
        return
    if bp_obj.isa("PrefabBlueprint"):
        for ref in bp_obj.get("Objects") or []:
            e3, o3 = dump.deref(bp_ebx, ref)
            if o3 is None:
                continue
            _reference_object(dump, e3, o3, M, depth + 1, out, seen)


def _reference_object(dump, ebx, o, Mparent, depth, out, seen):
    if not o.isa("ReferenceObjectData"):
        return
    if o.get("Excluded"):
        return
    if o.isa("LayerReferenceObjectData") or o.isa("SubWorldReferenceObjectData"):
        return  # handled by the caller's file walk
    Ml = lt_to_mat(o.get("BlueprintTransform"))
    M = Ml @ Mparent
    e2, bp = dump.deref(ebx, o.get("Blueprint"))
    if bp is None:
        return
    _blueprint_static_meshes(dump, e2, bp, M, depth, out, seen)


def layer_instances(dump, ebx):
    """Static meshes placed by reference objects in a WorldPartData / SubWorldData file."""
    out = []
    root = ebx.primary
    I = np.eye(4)
    for ref in root.get("Objects") or []:
        e2, o = dump.deref(ebx, ref)
        if o is None:
            continue
        _reference_object(dump, e2, o, I, 0, out, set())
    return out


def scan_world(dump, cache_name="world_instances.json"):
    """Scan every SubWorldData/WorldPartData under SP_MainCity; cache placements as JSON:
    [{mesh_file, mesh_inst, type_file, M(16), src, file}]"""
    p = os.path.join(dump.cache, cache_name)
    if os.path.isfile(p):
        with open(p, encoding="utf-8") as fh:
            return json.load(fh)
    files = [(g, v) for g, v in dump.index.items()
             if v[0].startswith(LEVEL_PREFIX) and v[1] in ("SubWorldData", "WorldPartData")
             and "_zs_zs_" in v[0]]
    t0 = time.time()
    res = []
    for i, (g, v) in enumerate(sorted(files, key=lambda x: x[1][0])):
        try:
            e = dump.load(g)
        except Exception as ex:  # noqa
            print("  load fail", v[0], ex, file=sys.stderr)
            continue
        if e is None:
            continue
        try:
            items = smg_instances(dump, e) + layer_instances(dump, e)
        except Exception as ex:  # noqa
            print("  scan fail", v[0], repr(ex), file=sys.stderr)
            continue
        for mesh, mt, M, src in items:
            res.append({"mesh": [mesh.file, mesh.inst], "type": [mt.file, mt.inst] if isinstance(mt, ExtRef) else None,
                        "M": [round(float(x), 5) for x in M.reshape(-1)], "src": src, "file": v[0]})
        if (i + 1) % 100 == 0:
            print(f"  scanned {i+1}/{len(files)} files, {len(res)} instances ({time.time()-t0:.0f}s)", flush=True)
        dump._ebx_cache.clear()
    with open(p, "w", encoding="utf-8") as fh:
        json.dump(res, fh)
    return res
