"""Compare two autopilot runs tick by tick: are their player traces identical?

    uv run --python 3.13 tools/harness/determinism.py RUN_A.jsonl RUN_B.jsonl [--client 0]

Inputs are the per-run trace jsonl files (IW4L_TRACE_PLAYER=1). A run may drive several
routes (`autopilot: start <route>` ... `autopilot: done|FAIL`); each route is compared on its
own, aligned on its first segment line (`autopilot: seg 0 ... tick=T`, T = the fixed-step
command tick, the authority's tick clock): every `trace:` row of the client while the route
runs is compared on lifecycle, origin, velocity, view angles and mec mode, and the
autopilot's own seg/act/step lines are compared with their ticks made relative. The trace
samples once per rendered frame, so only ticks both runs logged are compared. Prints a
summary and exits 1 on any difference.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

TRACE = re.compile(r"trace: t=(\d+) c=(\d+) L=(\w+) o=(\S+) v=(\S+) hp=\S+ ang=(\S+) g=\S+ m=(\S+)")
AP = re.compile(r"autopilot: (start|seg|act|step|done|FAIL)\b")
TICK = re.compile(r"tick=(\d+)")


def load_routes(path, client: str = "0") -> list:
    """-> [{route, rows: [(tick, life, o, v, ang, mode)], lines: [autopilot lines]}] per route run."""
    routes, cur = [], None
    for line in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            msg = json.loads(line).get("msg", "")
        except json.JSONDecodeError:
            continue
        m = AP.search(msg)
        if m:
            text = msg[m.start():]
            if m.group(1) == "start":
                cur = {"route": text.split()[2] if len(text.split()) > 2 else "?", "rows": [], "lines": []}
                routes.append(cur)
            elif cur is not None:
                cur["lines"].append(text)
                if m.group(1) in ("done", "FAIL"):
                    cur["closed"] = True
            continue
        m = TRACE.search(msg)
        if m and m.group(2) == client and cur is not None and not cur.get("closed"):
            cur["rows"].append((int(m.group(1)), m.group(3), m.group(4), m.group(5), m.group(6), m.group(7)))
    return routes


def seg0_tick(lines):
    for line in lines:
        m = TICK.search(line)
        if m and line.startswith("autopilot: seg"):
            return int(m.group(1))
    return None


def relative(lines):
    t0 = seg0_tick(lines)
    return [TICK.sub(lambda m: f"tick=+{int(m.group(1)) - t0}", l) if t0 is not None else l for l in lines]


def compare(a: dict, b: dict) -> dict:
    la, lb = relative(a["lines"]), relative(b["lines"])
    ta, tb = seg0_tick(a["lines"]), seg0_tick(b["lines"])
    ka = {r[0] - ta: r[1:] for r in a["rows"]} if ta is not None else {}
    kb = {r[0] - tb: r[1:] for r in b["rows"]} if tb is not None else {}
    common = sorted(k for k in set(ka) & set(kb) if k >= 0)
    diffs = [k for k in common if ka[k] != kb[k]]
    first_line = next((i for i, (x, y) in enumerate(zip(la, lb)) if x != y), None)
    if first_line is None and len(la) != len(lb):
        first_line = min(len(la), len(lb))
    return {
        "lines_equal": la == lb,
        "first_line": None if first_line is None else {"index": first_line,
                                                       "a": la[first_line] if first_line < len(la) else None,
                                                       "b": lb[first_line] if first_line < len(lb) else None},
        "compared": len(common),
        "differing": len(diffs),
        "first": {"tick": diffs[0], "a": ka[diffs[0]], "b": kb[diffs[0]]} if diffs else None,
    }


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    client = sys.argv[sys.argv.index("--client") + 1] if "--client" in sys.argv else "0"
    ra, rb = load_routes(args[0], client), load_routes(args[1], client)
    same = bool(ra) and [x["route"] for x in ra] == [x["route"] for x in rb]
    if not same:
        print(f"routes differ: {[x['route'] for x in ra]} vs {[x['route'] for x in rb]}")
    for a, b in zip(ra, rb):
        c = compare(a, b)
        ok = c["lines_equal"] and c["differing"] == 0 and c["compared"] > 0
        same &= ok
        print(f"{a['route']}: autopilot lines equal={c['lines_equal']}, ticks compared {c['compared']}, "
              f"differing {c['differing']} -> {'IDENTICAL' if ok else 'DIFFERENT'}")
        if c["first_line"] is not None:
            print(f"  first line difference: {c['first_line']}")
        if c["first"]:
            print(f"  first differing tick: {c['first']}")
    print("IDENTICAL" if same else "DIFFERENT")
    sys.exit(0 if same else 1)


if __name__ == "__main__":
    main()
