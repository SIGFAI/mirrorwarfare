"""Access to a Frostbite-Scripts dump of Mirror's Edge Catalyst (read-only).

Dump layout (produced by Frostbite-Scripts frostbite3 dumper):
  <dump>/bundles/ebx/<asset name lower>.ebx
  <dump>/bundles/res/<res name lower>.<ResType>
  <dump>/bundles/chunks/<guid>.chunk, <dump>/chunks/<guid>.chunk   (already decompressed)
  <dump>/resTable.bin   pickle {resRid: ResInfo(name, resType, resMeta)}
"""
from __future__ import annotations

import json
import os
import pickle
import sys
import time

from fbebx import Ebx, ExtRef, IntRef, primary_type_and_guid

# Work root: MIRRORWARFARE_ROOT, else the directory holding the iw4L checkout.
WORK_ROOT = os.environ.get("MIRRORWARFARE_ROOT") or os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
DEFAULT_DUMP = os.path.join(WORK_ROOT, "mec-dump")
DEFAULT_ARENAS = os.path.join(WORK_ROOT, "mec-arenas")
DEFAULT_CACHE = os.path.join(DEFAULT_ARENAS, "_cache")


class _Stub:
    def __init__(self, *a, **k):
        pass

    def __setstate__(self, st):
        self.__dict__.update(st)


class _Unpickler(pickle.Unpickler):
    def find_class(self, mod, name):
        if mod in ("res", "dbo", "__main__", "ebx"):
            return _Stub
        return super().find_class(mod, name)


class Dump:
    def __init__(self, root=DEFAULT_DUMP, cache=DEFAULT_CACHE):
        self.root = root
        self.cache = cache
        os.makedirs(cache, exist_ok=True)
        self.ebx_root = os.path.join(root, "bundles", "ebx")
        self.res_root = os.path.join(root, "bundles", "res")
        with open(os.path.join(root, "resTable.bin"), "rb") as fh:
            rt = _Unpickler(fh).load()
        self.res = {rid: (v.name, v.resType, v.resMeta) for rid, v in rt.items()}
        self._ebx_cache: dict[str, Ebx] = {}
        self._load_index()

    # ---------------- EBX index (file guid -> relative path, primary type) ----------
    def _load_index(self):
        p = os.path.join(self.cache, "ebx_index.json")
        if os.path.isfile(p):
            with open(p, encoding="utf-8") as fh:
                idx = json.load(fh)
        else:
            idx = {}
            t0 = time.time()
            n = 0
            for d, _, files in os.walk(self.ebx_root):
                for f in files:
                    if not f.endswith(".ebx"):
                        continue
                    full = os.path.join(d, f)
                    rel = os.path.relpath(full, self.ebx_root).replace("\\", "/")[:-4]
                    try:
                        g, typ, name = primary_type_and_guid(full)
                    except Exception as e:  # noqa
                        print("  ebx index fail", rel, e, file=sys.stderr)
                        continue
                    idx[g] = [rel, typ, name]
                    n += 1
                    if n % 5000 == 0:
                        print(f"  indexed {n} ebx ({time.time()-t0:.0f}s)", flush=True)
            with open(p, "w", encoding="utf-8") as fh:
                json.dump(idx, fh)
        self.index = idx
        self.by_path = {v[0]: g for g, v in idx.items()}

    def ebx_path(self, file_guid):
        v = self.index.get(file_guid)
        return v[0] if v else None

    def ebx_type(self, file_guid):
        v = self.index.get(file_guid)
        return v[1] if v else None

    def load(self, file_guid_or_path) -> Ebx | None:
        key = file_guid_or_path
        rel = self.ebx_path(key) if key in self.index else key.lower()
        if rel is None:
            return None
        e = self._ebx_cache.get(rel)
        if e is None:
            full = os.path.join(self.ebx_root, rel + ".ebx")
            if not os.path.isfile(full):
                return None
            e = Ebx(full)
            if len(self._ebx_cache) > 4000:
                self._ebx_cache.clear()
            self._ebx_cache[rel] = e
        return e

    def deref(self, ebx: Ebx, ref):
        """Resolve a ref found in `ebx` -> (Ebx, Obj) or (None, None)."""
        if ref is None:
            return None, None
        if isinstance(ref, IntRef):
            return ebx, ebx.instances[ref.index]
        if isinstance(ref, ExtRef):
            e = self.load(ref.file)
            if e is None:
                return None, None
            return e, e.by_guid.get(ref.inst)
        return None, None

    # ---------------- res / chunks ----------------
    def res_info(self, rid):
        return self.res.get(rid)

    def res_bytes(self, rid):
        info = self.res.get(rid)
        if not info:
            return None, None
        name, rtype, meta = info
        base = os.path.join(self.res_root, name)
        d = os.path.dirname(base)
        stem = os.path.basename(base)
        try:
            for f in os.listdir(d):
                if f.startswith(stem + ".") and "." not in f[len(stem) + 1:]:
                    with open(os.path.join(d, f), "rb") as fh:
                        return fh.read(), meta
        except FileNotFoundError:
            pass
        return None, meta

    def res_by_name(self, name, ext):
        p = os.path.join(self.res_root, name + "." + ext)
        if os.path.isfile(p):
            with open(p, "rb") as fh:
                return fh.read()
        return None

    def chunk(self, guid):
        for sub in ("bundles/chunks", "chunks"):
            p = os.path.join(self.root, sub, guid + ".chunk")
            if os.path.isfile(p):
                with open(p, "rb") as fh:
                    return fh.read()
        return None
