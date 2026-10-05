"""Give an existing arena package its menu title unless it already has one.

    uv run --no-project --python 3.13 tools/mec/arena_title.py <arena dir> "<title>"
"""
import json
import os
import sys


def main(arena_dir, title):
    path = os.path.join(arena_dir, "arena.json")
    with open(path, encoding="utf-8") as fh:
        meta = json.load(fh)
    if meta.get("title"):
        return
    meta = {"name": meta.pop("name"), "title": title, **meta}
    with open(path, "w") as fh:
        json.dump(meta, fh, indent=1)
    print(f"   title set: {title}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
