# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Build the derived caches every arena build reads: <cache>/ebx_index.json (~7 min, all EBX
headers) and <cache>/world_instances.json (~70 s, SP_MainCity static placements). Existing
files are kept.

    uv run --python 3.13 tools/mec/prepare_cache.py [--dump DIR] [--cache DIR]
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from fbdata import DEFAULT_CACHE, DEFAULT_DUMP, Dump  # noqa: E402
from fbworld import scan_world  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", default=DEFAULT_DUMP)
    ap.add_argument("--cache", default=DEFAULT_CACHE)
    args = ap.parse_args()
    t0 = time.time()
    dump = Dump(args.dump, args.cache)
    print(f"ebx index: {len(dump.index)} assets ({time.time() - t0:.0f}s)", flush=True)
    world = scan_world(dump)
    print(f"world: {len(world)} static instances ({time.time() - t0:.0f}s)", flush=True)


if __name__ == "__main__":
    main()
