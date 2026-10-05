"""report.json -> report.md."""
from __future__ import annotations

import json
from pathlib import Path


def _v(v):
    if isinstance(v, dict):
        return ", ".join(f"{k}: {x}" for k, x in v.items()) or "-"
    if isinstance(v, list):
        return ", ".join(str(x) for x in v) or "-"
    if v is None:
        return "-"
    return str(v)


def checks_table(checks):
    lines = ["| check | value | expected | status | note |", "|---|---|---|---|---|"]
    for c in checks:
        val = _v(c["value"]) + (f" {c['unit']}" if c.get("unit") and c["value"] is not None else "")
        lines.append(f"| `{c['key']}` | {val} | {c['range']} | **{c['status']}** | {c.get('note', '')} |")
    return "\n".join(lines)


def counts(checks):
    out = {}
    for c in checks:
        out[c["status"]] = out.get(c["status"], 0) + 1
    return out


def render(rep: dict, out: Path) -> str:
    m = rep["meta"]
    L = [f"# Mirrorwarfare harness report", ""]
    L += [f"* exe: `{m['exe']}` (built {m['exe_mtime']}, sha256 {m['exe_sha256'][:16]}…)",
          f"* started {m['started']}, finished {m.get('finished', '(running)')}, {m.get('duration_min', '?')} min",
          f"* other iw4l.exe running at start (not ours): {m.get('foreign_iw4l_pids_at_start') or 'none'}",
          f"* sound: {'on (--sound)' if m.get('sound') else 'off for every launch (IW4L_SOUND=off; --sound opts in)'}"]
    if m.get("prior_sound_on_load"):
        L.append(f"* sound-on load times, prior measurement (not re-run): {m['prior_sound_on_load']}")
    L.append("")
    L += ["## Summary", "", "| suite | PASS | FAIL | MISS | INFO |", "|---|---|---|---|---|"]
    for name, su in rep["suites"].items():
        c = counts(su.get("checks", []))
        L.append(f"| {name} | {c.get('PASS', 0)} | {c.get('FAIL', 0)} | {c.get('MISS', 0)} | {c.get('INFO', 0)} |")
    L.append("")
    fails = [c for su in rep["suites"].values() for c in su.get("checks", []) if c["status"] in ("FAIL", "MISS")]
    if fails:
        L += ["**Failing / missing:** " + ", ".join(f"`{c['key']}`={_v(c['value'])}" for c in fails), ""]

    for name, su in rep["suites"].items():
        L += [f"## {name}", ""]
        if su.get("error"):
            L += ["```", su["error"], "```", ""]
        if name == "load":
            L += load_section(su)
        L += [checks_table(su.get("checks", [])), ""]
        if name == "bots":
            L += bots_section(su)
        if name == "perf":
            for k, d in su.get("data", {}).items():
                L.append(f"* {k}: " + ", ".join(f"{a}={b}" for a, b in d.items() if not isinstance(b, list)))
            L.append("")
        if name == "movement":
            L += movement_section(su)
        if name == "anim":
            L += anim_section(su, out)
        if name == "stability":
            L += stability_section(su)
        if name == "bots":
            for key, d in su.get("data", {}).items():
                for s in d.get("screenshots", []):
                    L.append(f"![{s}]({s.replace(chr(92), '/')})")
            L.append("")

    L += ["## Runs", "", "| run | zone | wall s | rc | peak RSS MiB | timeout |", "|---|---|---|---|---|---|"]
    for r in rep["runs"]:
        L.append(f"| {r['name']} | {r['zone']} | {r['wall_s']} | {r['returncode']} | {r['peak_rss_mb']} | {r['timed_out']} |")
    L.append("")
    return "\n".join(L)


def load_section(su):
    L = []
    agg = su.get("data", {}).get("agg", [])
    if not agg:
        return L
    L += ["Times in seconds from process start (median / min / max). `stdout` = first log line, `renderer` =",
          "`runtime:` line (window + device), `world` = `wait world` satisfied (mark), `in game` = after `spawn 0` (mark),",
          "`bench` = IW4L_BENCH command → playable.", "",
          "| zone | sound | cache | n | stdout | renderer | world | in game | bench playable | peak RSS MiB |",
          "|---|---|---|---|---|---|---|---|---|---|"]

    def f(x):
        return "-" if not x else f"{x['median']} / {x['min']} / {x['max']}"

    for a in agg:
        L.append(f"| {a['zone']} | {'on' if a['sound'] else 'off'} | {a['cache']} | {a['n']} | {f(a['first_stdout_s'])} | "
                 f"{f(a['renderer_up_s'])} | {f(a['world_s'])} | {f(a['ingame_s'])} | {f(a['bench_command_to_playable_s'])} | "
                 f"{f(a['peak_rss_mb'])} |")
    L.append("")
    return L


def bots_section(su):
    L = []
    for key, d in su.get("data", {}).items():
        if not d.get("per_bot"):
            continue
        L += [f"### {key}: {d.get('minutes')} min, {d.get('kills')} kills, deaths by means {d.get('deaths_by_mod')}", "",
              "| bot | kills | deaths | alive s | moving % | stalls >3 s | longest stall s (at) | permanent | respawns | parkour |",
              "|---|---|---|---|---|---|---|---|---|---|"]
        for b, s in d["per_bot"].items():
            L.append(f"| {b} | {s['kills']} | {s['deaths']} | {s['alive_s']} | {s['moving_pct']} | {s['stall_events']} | "
                     f"{s['longest_stall_s']} ({_v(s.get('longest_stall_at'))}) | {s['permanent_stuck']} | {s['respawns']} | {_v(s['parkour'])} |")
        if d.get("fall_deaths"):
            L.append("")
            L.append("Fall deaths: " + "; ".join(f"{k['victim_name']}#{k['victim']} dmg={k['damage']} tick={k['tick']}" for k in d["fall_deaths"]))
        L.append("")
    return L


def movement_section(su):
    L = []
    d = su.get("data", {})
    tb = d.get("testbox", {})
    if tb.get("sprint_curve"):
        pts = tb["sprint_curve"][::4]
        L += ["Sprint curve (s: in/s): " + ", ".join(f"{t}:{v}" for t, v in pts), ""]
    if tb.get("events"):
        L += [f"Player movement events on testbox: {_v(tb['events'])}", ""]
    af = d.get("anchor_flicker", {}).get("per_spawn")
    if af:
        L += ["| anchor spawn seed | start | flickers | ground running s | modes |", "|---|---|---|---|---|"]
        for p in af:
            L.append(f"| {p['seed']} | {_v(p['start'])} | {p['flickers']} | {p['ground_run_s']} | {_v(p['modes'])} |")
        L.append("")
    rp = d.get("rust", {}).get("event_places")
    if rp:
        L += ["mp_rust (IW4L_MOVEMENT=mec) parkour event places (IW4 in):", ""]
        for k, v in rp.items():
            L.append(f"* {k}: " + "; ".join(str(tuple(x)) for x in v))
        L.append("")
    return L


def anim_section(su, out: Path):
    L = []
    review = {}
    rp = out / "animation_review.json"
    if rp.exists():
        review = json.loads(rp.read_text(encoding="utf-8"))
    shots = su.get("data", {}).get("shots", {})
    for s in su.get("data", {}).get("screenshots", []):
        name = Path(s).stem
        meta = shots.get(name + ".png") or shots.get(name) or {}
        L.append(f"### {name}")
        L.append(f"trace at capture: mode={meta.get('mode')} z={meta.get('z')} lowered={meta.get('lowered')}")
        if name in review:
            L.append(f"\n**Review:** {review[name]}")
        L.append(f"\n![{name}]({s.replace(chr(92), '/')})\n")
    if review.get("_overall"):
        L += ["**Overall animation judgement:** " + review["_overall"], ""]
    if not review:
        L += ["_No animation_review.json yet: look at the screenshots and write {\"<shot>\": \"judgement\", \"_overall\": ...},",
              "then `uv run tools/harness/run.py --rerender <this dir>`._", ""]
    return L


def stability_section(su):
    L = []
    d = su.get("data", {})
    if d.get("gsc_errors"):
        L += ["| GSC runtime error (at function: fault) | count | runs |", "|---|---|---|"]
        for e in d["gsc_errors"]:
            L.append(f"| `{e['error'][:160]}` | {e['count']} | {e['runs']} |")
        L.append("")
    for p in d.get("panics", []):
        L.append(f"* panic in {p['run']}: `{p['lines'][0]}`")
    return L
