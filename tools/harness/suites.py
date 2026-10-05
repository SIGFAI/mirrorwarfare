"""Suites: console scripts that play the game, and the analysis that turns a run into numbers.

Coordinates are IW4 inches (IW4 = (-z, -x, y) * 39.37 from glTF metres). The mec:testbox
playground (scripts/mec-testbox.py) writes the movement scenarios as autopilot routes
(<arena>/routes/<name>.json: start pose + inputs on state/tick triggers) and the showcase line
as <arena>/route.json; `autopilot` runs them with one usercmd per sim tick in lockstep with the
listen authority, so two runs are compared tick for tick. Scenario courses (glTF metres):
  sprint / jumps  south ring outer lane z = 38.5, x -36 -> +36 (clear floor)
  wallrun         wall A face x = -28.6, z -16..-4, 6 m tall (runner x = -28.0, northwards)
  wallclimb       W1 south face z = -22, 3.5 m = 137.8 in (from x = -32, z = -20.35)
  vault           north-ring hurdle 0.9 m x 0.4 m at x -24 (from x = -36, z = -35.5)
  roll            deck east lane edge: 3.0 m drop onto R1 (start (15.5, 7, -28) facing east)
  hard            deck north drop gap x 8..12: 7.0 m drop (start (10, 7, -33.65) facing north)
Thresholds come from the decoded Catalyst numbers (context/artifacts/2026-10-04-mec-ant):
sprint curve 5.0 / 6.7 / 7.15 / 7.2 m/s at 0.5 / 1 / 2 / 3 s, jumps 1.1 m (still) and
1.2 m at 8.04 m/s (fast), wallrun 1.333 s with +1.2 m, landing 6-10 m = 35-75 damage.
"""
from __future__ import annotations

import math
import re
import statistics
from collections import Counter, defaultdict

from game import TICK_S, GameRun, kill_lines, mec_events, movement_model, trace_rows

LOWERED = 0x4000  # pm_flags::SPRINTING, set by movement_mec when the weapon is lowered

# ----------------------------------------------------------------------------- thresholds
# (lo, hi) inclusive; None = open. Units: in, in/s, s, hp, degrees.
THRESHOLDS = {
    # sprint curve (one tick of input latency included): 5.0 / 6.7 / 7.15 / 7.2 m/s
    "sprint.v_0.5s": (165, 215),
    "sprint.v_1s": (238, 275),
    "sprint.v_2s": (270, 290),
    "sprint.v_3s": (276, 290),
    "sprint.top": (278, 292),
    "sprint.t_95pct_top": (None, 1.6),
    "sprint.lowered_frac": (None, 0.05),
    # JumpStill 1.1 m; JumpFast 1.2 m at 8.04 m/s (0.70 s airtime -> 5.6 m)
    "jump.stand_height": (40, 47),
    "jump.run_height": (44, 51),
    "jump.run_distance": (200, 245),
    # WallrunDriver: 80 ticks = 1.333 s, +1.2 m at 0.533 s, ~7.2 m/s along the wall; begun at
    # ground level the arc (end -0.7 m) lands at ~1.15 s
    "wallrun.duration": (1.1, 1.45),
    "wallrun.distance": (300, 440),
    "wallrun.height_gain": (43, 51),
    "wallrun.ads_max": (None, 0.10),
    "wallrun.ads_ground_control": (0.90, None),
    "wallrun.hipfire_shots": (1, None),
    "wallclimb.height_gain": (30, 105),
    "ledge.success": (1, None),
    "ledge.end_z": (136, 140),
    "climb.lowered_frac": (0.90, None),
    "vault.success": (1, None),
    "vault.speed_retained": (0.80, 1.25),
    "vault.lowered_frac": (0.90, None),
    "slide.distance": (80, 320),
    "slide.start_speed": (270, None),
    "slide.duration": (0.6, 1.5),
    "roll.success": (1, None),
    "roll.health_delta": (0, 0),
    "roll.speed_after": (100, None),
    "hard.success": (1, None),
    # 7.0 m drop: fail tier 6-10 m, 35 + (1/4) * 40 = 45 damage
    "hard.health_delta": (-47, -43),
    "quickturn.abs_yaw_delta": (178, 182),
    "quickturn.time": (None, 0.6),
    "flicker.testbox_per10s": (0, 0),
    "flicker.anchor_per10s": (0, 1.0),
    "rust.movement_is_mec": (1, 1),
    "rust.sprint_v_2s": (200, 300),
    "rust.wallruns": (1, None),
    "rust.climbs": (1, None),
    "movement.scenarios_ok": (9, 9),
    "movement.deterministic": (1, 1),
    "showcase.done_ok": (1, 1),
    "showcase.deterministic": (1, 1),
    "showcase.max_yaw_per_tick": (None, 10.5),
    "coverage.all_pct": (99.0, None),
    "bots.kills": (1, None),
    "bots.fall_deaths": (0, 0),
    "bots.kills_per_min": (1.0, None),
    "bots.permanent_stuck": (0, 0),
    "perf.fps_avg": (60, None),
    "perf.frame_ms_p99": (None, 50.0),
    "perf.peak_rss_mb": (None, 12000),
    "stability.panics": (0, 0),
    "stability.unclean_exits": (0, 0),
    "stability.timeouts": (0, 0),
}


def check(results: list, key: str, value, note: str = "", unit: str = "", tkey: str | None = None):
    lo, hi = THRESHOLDS.get(tkey or key, (None, None))
    if value is None:
        status = "MISS"
    elif (lo is None or value >= lo) and (hi is None or value <= hi):
        status = "PASS"
    else:
        status = "FAIL"
    rng = f"[{'' if lo is None else lo} .. {'' if hi is None else hi}]"
    results.append({"key": key, "value": rnd(value), "range": rng, "status": status, "unit": unit, "note": note})
    return status


def info(results: list, key: str, value, note: str = "", unit: str = ""):
    results.append({"key": key, "value": rnd(value), "range": "", "status": "INFO", "unit": unit, "note": note})


def rnd(v):
    if isinstance(v, float):
        return round(v, 3)
    return v


def J(*cmds) -> str:
    return "; ".join(c for c in cmds if c)


# ----------------------------------------------------------------------------- helpers


def hspeed(r) -> float:
    return math.hypot(r["v"][0], r["v"][1])


def dist2(a, b) -> float:
    return math.hypot(a[0] - b[0], a[1] - b[1])


def wrap(a: float) -> float:
    a = (a + 180.0) % 360.0 - 180.0
    return a


def trace_coverage(rows, c=None) -> float | None:
    """Fraction of sim ticks (between the first and last traced tick) that have a trace row.
    The trace samples the presented snapshot once per frame, so a frame hitch drops ticks."""
    ticks = {r["tick"] for r in rows if c is None or r["c"] == c}
    if len(ticks) < 2:
        return None
    return len(ticks) / (max(ticks) - min(ticks) + 1)


def local_id(run: GameRun) -> int:
    for m in run.marks().values():
        if "local_id" in m:
            return m["local_id"]
    return 0


def seg(rows, c, t0, t1):
    return [r for r in rows if r["c"] == c and r.get("o") is not None and t0 <= r["tick"] <= t1]


def ev_in(evs, c, t0, t1, kind=None):
    return [e for e in evs if e["c"] == c and e["tick"] is not None and t0 <= e["tick"] <= t1
            and (kind is None or e["kind"] == kind)]


def window(marks: dict, a: str, b: str):
    if a in marks and b in marks and "tick" in marks[a] and "tick" in marks[b]:
        return marks[a]["tick"], marks[b]["tick"]
    return None


def speed_at(rows, t0_tick, secs):
    want = t0_tick + secs / TICK_S
    best = min(rows, key=lambda r: abs(r["tick"] - want), default=None)
    if best is None or abs(best["tick"] - want) > 2:
        return None
    return hspeed(best)


def runs_of(rows):
    """Split rows into runs of consecutive ticks."""
    out = []
    for r in rows:
        if out and r["tick"] - out[-1][-1]["tick"] <= 1:
            out[-1].append(r)
        else:
            out.append([r])
    return out


def flicker_count(rows, evs, c):
    """Ground -> Air (<=3 ticks, no jump) -> Ground transitions, and seconds of ground running."""
    jumps = {e["tick"] for e in evs if e["c"] == c and e["kind"] in ("Jump", "Springboard", "WallJump")}
    flick, ground_run_s = 0, 0.0
    rows = [r for r in rows if r["c"] == c and r.get("o") is not None and r["life"] == "Alive"]
    i = 0
    while i < len(rows):
        r = rows[i]
        if r["mode"] == "Ground" and hspeed(r) > 100:
            ground_run_s += TICK_S
        if r["mode"] == "Ground" and i + 1 < len(rows) and rows[i + 1]["mode"] == "Air":
            j = i + 1
            while j < len(rows) and rows[j]["mode"] == "Air":
                j += 1
            n_air = j - (i + 1)
            jumped = any(t in jumps for t in range(r["tick"] - 1, rows[j - 1]["tick"] + 2))
            if j < len(rows) and rows[j]["mode"] == "Ground" and n_air <= 3 and not jumped:
                flick += 1
            i = j
            continue
        i += 1
    return flick, ground_run_s


PREAMBLE = "wait world; mark world; spawn 0; mark ingame; force_match_start; wait 1s"


# ============================================================================ load


def load_cmds() -> str:
    return "wait world; mark world; spawn 0; mark ingame; wait 3s; mark settled; finish_run"


def analyse_load(run: GameRun) -> dict:
    marks = run.marks()
    from game import parse_bench

    b = parse_bench(run.bench_path)
    return {
        "first_stdout_s": run.first_stdout_s("log:"),
        "renderer_up_s": run.first_stdout_s("runtime:"),
        "gsc_installed_stdout_s": run.first_stdout_s("gsc: installed"),
        "world_s": marks.get("world", {}).get("s"),
        "ingame_s": marks.get("ingame", {}).get("s"),
        "rss_at_ingame_mib": marks.get("ingame", {}).get("rss_mib") or run.rss_at(marks.get("ingame", {}).get("s")),
        "peak_rss_mb": run.peak_rss_mb,
        "bench_command_to_playable_s": b.get("command_to_playable_s"),
        "bench_match_installed_s": b.get("match_installed_s"),
        "bench_world_spawned_s": b.get("world_spawned_s"),
        "wall_s": run.wall_s,
        "ok": "ingame" in marks and not run.timed_out,
    }


# ============================================================================ movement


def movement_testbox_cmds() -> str:
    return J(
        PREAMBLE,
        # ADS control on the ground (proves the ads observable moves)
        "move 354 -1100 0 90 0", "wait 0.5s", "mark ads_go", "hold +speed_throw", "wait 1.2s",
        "release +speed_throw", "mark ads_done", "wait 0.8s",
        # sprint ramp then slide
        "mark sprint_go", "hold +forward", "wait 4s", "mark slide_go", "press +movedown 1.4",
        "wait 1.6s", "mark slide_done", "release +forward", "wait 1s",
        # quickturn standing
        "look 90 0", "wait 0.5s", "mark qt_go", "press +quickturn", "wait 1s", "mark qt_done",
        # standing jump
        "mark sjump_go", "press +gostand", "wait 1.5s", "mark sjump_done",
        # running jump
        "move 354 -1100 0 90 0", "wait 0.5s", "hold +forward", "wait 3s", "mark rjump_go",
        "press +gostand", "wait 1.6s", "mark rjump_done", "release +forward", "wait 0.5s",
        # wallrun plain
        "move -930 433 0 -100 0", "wait 0.4s", "hold +forward", "wait 1.2s", "mark wr1_go",
        "press +gostand", "wait 2.5s", "mark wr1_done", "release +forward", "wait 0.4s",
        # wallrun holding ADS
        "move -930 433 0 -100 0", "wait 0.4s", "hold +forward", "wait 1.2s", "mark wr2_go",
        "press +gostand", "wait 0.25s", "hold +speed_throw", "wait 2.2s", "release +speed_throw",
        "mark wr2_done", "release +forward", "wait 0.4s",
        # wallrun hip fire
        "move -930 433 0 -100 0", "wait 0.4s", "hold +forward", "wait 1.2s", "mark wr3_go",
        "press +gostand", "wait 0.35s", "press +attack 0.2", "wait 2.1s", "mark wr3_done",
        "release +forward", "wait 0.4s",
        # wallclimb + ledge climb onto the 3.5 m wall
        "move 945 840 0 90 0", "wait 0.4s", "hold +forward", "wait 0.4s", "mark wc_go",
        "hold +gostand", "wait 1.8s", "release +gostand", "release +forward", "wait 0.6s",
        "mark wc_done",
        # vault over the 0.9 m box (auto-vault at speed)
        "move 945 0 0 180 0", "wait 0.4s", "mark v_go", "hold +forward", "wait 3.2s",
        "release +forward", "wait 0.4s", "mark v_done",
        # roll off the 6 m tower
        "move -650 -880 237 90 0", "wait 0.5s", "mark roll_go", "hold +forward", "wait 0.9s",
        "press +movedown 0.3", "wait 1.4s", "release +forward", "mark roll_done", "wait 0.5s",
        # hard landing off the tower (last: it costs health)
        "move -650 -880 237 90 0", "wait 0.5s", "mark hard_go", "hold +forward", "wait 1.6s",
        "release +forward", "wait 0.6s", "mark hard_done",
        "showpos", "wait 0.5s", "finish_run",
    )


def analyse_movement_testbox(run: GameRun, results: list, marks: dict | None = None) -> dict:
    rows = trace_rows(run)
    evs = mec_events(run)
    marks = run.marks() if marks is None else marks
    me = local_id(run)
    data = {"trace_rows": len(rows), "mec_events": len(evs)}
    if not rows:
        info(results, "movement.trace", None, "no trace lines: build lacks IW4L_TRACE_PLAYER hook")
    info(results, "movement.model", movement_model(run))
    info(results, "movement.trace_coverage", trace_coverage(rows, me), "share of 50 ms ticks with a trace row")

    # ---- ADS control
    w = window(marks, "ads_go", "ads_done")
    ads_ground = max((r["ads"] for r in seg(rows, me, *w)), default=None) if w else None

    # ---- sprint
    w = window(marks, "sprint_go", "slide_go")
    if w:
        s = seg(rows, me, *w)
        t0 = w[0]
        top = max((hspeed(r) for r in s), default=None)
        for secs in (0.5, 1, 2, 3):
            check(results, f"sprint.v_{secs:g}s", speed_at(s, t0, secs), unit="in/s")
        check(results, "sprint.top", top, unit="in/s")
        t95 = next(((r["tick"] - t0) * TICK_S for r in s if top and hspeed(r) >= 0.95 * top), None)
        check(results, "sprint.t_95pct_top", t95, unit="s")
        check(results, "sprint.lowered_frac",
              (sum(1 for r in s if r["pmf"] & LOWERED) / len(s)) if s else None, "weapon must stay up while running")
        data["sprint_curve"] = [(round((r["tick"] - t0) * TICK_S, 2), round(hspeed(r))) for r in s]
        fl, gs = flicker_count(s, evs, me)
        check(results, "flicker.testbox_per10s", (fl / gs * 10) if gs > 1 else None,
              f"{fl} flickers in {gs:.1f}s of running on flat floor")
    else:
        check(results, "sprint.top", None, "marks missing")

    # ---- slide
    w = window(marks, "slide_go", "slide_done")
    if w:
        s = seg(rows, me, *w)
        st = ev_in(evs, me, w[0] - 2, w[1], "SlideStart")
        en = ev_in(evs, me, w[0] - 2, w[1] + 20, "SlideEnd")
        slide_rows = [r for r in s if r["mode"] == "Slide"]
        if st and en:
            check(results, "slide.distance", dist2(st[0]["o"], en[0]["o"]), unit="in")
            check(results, "slide.start_speed", st[0]["hspeed"], unit="in/s")
            info(results, "slide.end_speed", en[0]["hspeed"], unit="in/s")
        else:
            check(results, "slide.distance", None, "no SlideStart/SlideEnd")
        check(results, "slide.duration", len(slide_rows) * TICK_S if slide_rows else None, unit="s")
        data["slide_curve"] = [round(hspeed(r)) for r in slide_rows]
        if slide_rows:
            info(results, "slide.speed_curve", " ".join(str(v) for v in data["slide_curve"][::2]), "every 0.1 s, in/s")
            info(results, "slide.eye_crouched", "yes" if any(r["mode"] == "Slide" for r in s) else "no")

    # ---- quickturn
    w = window(marks, "qt_go", "qt_done")
    if w:
        s = seg(rows, me, *w)
        if len(s) >= 3:
            y0, y1 = s[0]["yaw"], s[-1]["yaw"]
            d = wrap(y1 - y0)
            settle = next((r for r in s if abs(wrap(r["yaw"] - y1)) < 2.0), None)
            check(results, "quickturn.abs_yaw_delta", abs(d), unit="deg")
            check(results, "quickturn.time", (settle["tick"] - w[0]) * TICK_S if settle else None, unit="s")
        else:
            check(results, "quickturn.abs_yaw_delta", None)
        info(results, "quickturn.event", len(ev_in(evs, me, *w, "QuickTurn")))

    # ---- jumps
    w = window(marks, "sjump_go", "sjump_done")
    if w:
        s = seg(rows, me, *w)
        check(results, "jump.stand_height", (max(r["o"][2] for r in s) - s[0]["o"][2]) if s else None, unit="in")
    w = window(marks, "rjump_go", "rjump_done")
    if w:
        s = seg(rows, me, *w)
        jumps = ev_in(evs, me, w[0] - 1, w[1], "Jump")
        lands = ev_in(evs, me, w[0], w[1] + 10, "Land")
        if jumps and s:
            j = jumps[0]
            land = next((l for l in lands if l["tick"] >= j["tick"]), None)
            if land is None:
                # Short falls (< 16 in) are not logged: use the first Ground row after takeoff.
                air = [r for r in s if r["tick"] > j["tick"]]
                g = next((r for r in air if r["mode"] == "Ground"), None)
                land_o = g["o"] if g else None
            else:
                land_o = land["o"]
            # The Jump event is logged after the take-off tick moved: measure from the last
            # ground row before it.
            take = next((r for r in reversed(s) if r["tick"] <= j["tick"] and r["mode"] == "Ground"), None)
            o0 = take["o"] if take else j["o"]
            check(results, "jump.run_distance", dist2(o0, land_o) if land_o else None, unit="in")
            check(results, "jump.run_height", max(r["o"][2] for r in s if r["tick"] >= j["tick"]) - o0[2], unit="in")
            info(results, "jump.takeoff_speed", j["hspeed"], unit="in/s")
        else:
            check(results, "jump.run_distance", None, "no Jump event")

    # ---- wallruns
    def wallrun_rows(a, b):
        w = window(marks, a, b)
        if not w:
            return None, None
        s = seg(rows, me, *w)
        return w, [r for r in s if r["mode"] == "WallRun"]

    w, wr = wallrun_rows("wr1_go", "wr1_done")
    if w:
        starts = ev_in(evs, me, *w, "WallRunStart")
        info(results, "wallrun.started", len(starts))
        if wr:
            runs = runs_of(wr)
            main = max(runs, key=len)
            check(results, "wallrun.duration", len(main) * TICK_S, unit="s")
            check(results, "wallrun.distance", dist2(main[0]["o"], main[-1]["o"]), unit="in")
            check(results, "wallrun.height_gain", max(r["o"][2] for r in main) - main[0]["o"][2], unit="in")
            info(results, "wallrun.end_height_delta", round(main[-1]["o"][2] - main[0]["o"][2], 1), unit="in")
            info(results, "wallrun.speed", round(statistics.mean(hspeed(r) for r in main)), unit="in/s")
        else:
            check(results, "wallrun.duration", None, "no WallRun ticks")
    w, wr = wallrun_rows("wr2_go", "wr2_done")
    if w:
        check(results, "wallrun.ads_ground_control", ads_ground, "ADS frac reached on the ground with the same input")
        late = [r for r in wr if r["tick"] >= w[0] + 8]  # ADS held from +0.25 s; allow its ramp
        check(results, "wallrun.ads_max", max((r["ads"] for r in late), default=None) if late else None,
              f"{len(wr)} WallRun ticks with ADS held")
    w, wr = wallrun_rows("wr3_go", "wr3_done")
    if w:
        if wr:
            clips = [r["clip"] for r in wr]
            shots = max(clips) - min(clips)
            check(results, "wallrun.hipfire_shots", shots, f"clip {max(clips)} -> {min(clips)} during WallRun")
            info(results, "wallrun.lowered_frac", round(sum(1 for r in wr if r["pmf"] & LOWERED) / len(wr), 2))
        else:
            check(results, "wallrun.hipfire_shots", None, "no WallRun ticks")

    # ---- wallclimb + ledge
    w = window(marks, "wc_go", "wc_done")
    if w:
        s = seg(rows, me, *w)
        wc = [r for r in s if r["mode"] == "WallClimb"]
        lc = [r for r in s if r["mode"] == "LedgeClimb"]
        check(results, "wallclimb.height_gain", (max(r["o"][2] for r in wc) - wc[0]["o"][2]) if wc else None, unit="in")
        info(results, "wallclimb.duration", len(wc) * TICK_S, unit="s")
        ledge = ev_in(evs, me, *w, "LedgeClimbStart")
        check(results, "ledge.success", len(ledge))
        end = None
        if lc:
            after = [r for r in s if r["tick"] > lc[-1]["tick"] and r["mode"] == "Ground"]
            end = after[0]["o"][2] if after else None
        check(results, "ledge.end_z", end, "top of W1 (3.5 m) is 137.8", unit="in")
        busy = wc + lc
        check(results, "climb.lowered_frac", (sum(1 for r in busy if r["pmf"] & LOWERED) / len(busy)) if busy else None,
              f"{len(busy)} climb ticks")

    # ---- vault
    w = window(marks, "v_go", "v_done")
    if w:
        s = seg(rows, me, *w)
        vs = ev_in(evs, me, *w, "VaultStart")
        check(results, "vault.success", len(vs))
        if vs:
            v = vs[0]
            after = [r for r in s if r["tick"] > v["tick"] + 2 and r["mode"] == "Ground"]
            v_after = hspeed(after[0]) if after else None
            check(results, "vault.speed_retained", (v_after / v["hspeed"]) if v_after and v["hspeed"] else None,
                  f"{v['hspeed']} -> {round(v_after) if v_after else '?'} in/s")
            vr = [r for r in s if r["mode"] == "Vault"]
            check(results, "vault.lowered_frac", (sum(1 for r in vr if r["pmf"] & LOWERED) / len(vr)) if vr else None,
                  f"{len(vr)} vault ticks")
            info(results, "vault.onto", v["extra"].get("onto"))

    # ---- roll
    w = window(marks, "roll_go", "roll_done")
    if w:
        s = seg(rows, me, *w)
        ro = ev_in(evs, me, *w, "Roll")
        hl = ev_in(evs, me, *w, "HardLanding")
        check(results, "roll.success", len(ro), "HardLanding instead" if hl and not ro else "")
        if s:
            check(results, "roll.health_delta", min(r["hp"] for r in s) - s[0]["hp"], unit="hp")
        if ro:
            # FallingLandRoll: control returns at the 1.0 s move-out with the clip's speed
            out = [r for r in s if r["tick"] > ro[0]["tick"] and r["mode"] != "Roll"]
            later = [r for r in out if r["tick"] >= out[0]["tick"] + 2] if out else []
            check(results, "roll.speed_after", hspeed(later[0]) if later else None,
                  "0.1 s after the roll hands control back", unit="in/s")
            info(results, "roll.fall_height", ro[0]["extra"].get("fall_height"), unit="in")

    # ---- hard landing
    w = window(marks, "hard_go", "hard_done")
    if w:
        s = seg(rows, me, *w)
        hl = ev_in(evs, me, *w, "HardLanding")
        check(results, "hard.success", len(hl))
        if s:
            check(results, "hard.health_delta", min(r["hp"] for r in s) - s[0]["hp"], unit="hp")
        if hl:
            info(results, "hard.fall_height", hl[0]["extra"].get("fall_height"), unit="in")
            hr = [r for r in s if r["mode"] == "HardLanding"]
            info(results, "hard.lowered_frac", round(sum(1 for r in hr if r["pmf"] & LOWERED) / len(hr), 2) if hr else None)
    data["events"] = Counter(e["kind"] for e in evs if e["c"] == me)
    return data


# ---- flicker on the arena ---------------------------------------------------------------

FLICKER_SEEDS = (1, 2, 3, 4, 5, 6)
FLICKER_YAWS = (0, 90, 180, 270)


def flicker_anchor_cmds() -> str:
    # From each spawn, four 4 s straight runs (one per heading, back at the spawn each time):
    # without arena knowledge this keeps most of the time on open ground instead of against a wall.
    parts = [PREAMBLE, "god"]
    for s in FLICKER_SEEDS:
        for y in FLICKER_YAWS:
            parts += [f"force_spawn random {s}", "wait 0.6s", f"look {y} 0", f"mark fl{s}_{y}_go", "hold +forward",
                      "wait 4s", "release +forward", f"mark fl{s}_{y}_done", "wait 0.2s"]
    parts += ["finish_run"]
    return J(*parts)


def analyse_flicker_anchor(run: GameRun, results: list) -> dict:
    rows = trace_rows(run)
    evs = mec_events(run)
    marks = run.marks()
    me = local_id(run)
    total_f, total_s, per = 0, 0.0, []
    for s in FLICKER_SEEDS:
        for y in FLICKER_YAWS:
            w = window(marks, f"fl{s}_{y}_go", f"fl{s}_{y}_done")
            if not w:
                continue
            sr = seg(rows, me, *w)
            f, gs = flicker_count(sr, evs, me)
            total_f += f
            total_s += gs
            start = sr[0]["o"] if sr else None
            per.append({"seed": f"{s}/{y}", "flickers": f, "ground_run_s": round(gs, 1),
                        "start": start, "modes": dict(Counter(r["mode"] for r in sr))})
    check(results, "flicker.anchor_per10s", (total_f / total_s * 10) if total_s > 1 else None,
          f"{total_f} flickers over {total_s:.1f}s of ground running from {len(per)} spawns")
    info(results, "flicker.trace_coverage", trace_coverage(rows, me))
    info(results, "flicker.anchor_events", dict(Counter(e["kind"] for e in evs if e["c"] == me)))
    return {"per_spawn": per}


# ---- stock map (mp_rust) with mec movement ----------------------------------------------

RUST_SEEDS = (1, 2, 3, 4)
RUST_YAWS = tuple(range(0, 360, 30))


def rust_cmds() -> str:
    parts = [PREAMBLE, "god", "mark rs_go", "hold +forward", "wait 3.2s", "mark rs_jump", "press +gostand",
             "wait 1.4s", "release +forward", "mark rs_done", "wait 0.3s"]
    for s in RUST_SEEDS:
        parts += [f"force_spawn random {s}", "wait 0.5s"]
        for y in RUST_YAWS:
            parts += [f"look {y} 0", f"mark ex_{s}_{y}", "hold +forward",
                      "wait 0.9s", "press +gostand", "wait 0.9s", "hold +gostand", "wait 1.0s", "release +gostand",
                      "wait 0.6s", "press +gostand", "wait 0.8s", "release +forward", "wait 0.2s"]
    parts += ["mark ex_done", "finish_run"]
    return J(*parts)


def analyse_rust(run: GameRun, results: list) -> dict:
    rows = trace_rows(run)
    evs = mec_events(run)
    marks = run.marks()
    me = local_id(run)
    model = movement_model(run)
    check(results, "rust.movement_is_mec", 1 if model == "mec" else 0, f"log says movement: {model}")
    w = window(marks, "rs_go", "rs_jump")
    if w:
        s = seg(rows, me, *w)
        info(results, "rust.straight_run_top", max((hspeed(r) for r in s), default=None), "first 3.2 s from spawn",
             unit="in/s")
        for secs in (0.5, 1, 2, 3):
            if secs == 2:
                check(results, "rust.sprint_v_2s", speed_at(s, w[0], secs), "same ramp as the arena", unit="in/s")
            else:
                info(results, f"rust.sprint_v_{secs:g}s", speed_at(s, w[0], secs), unit="in/s")
    w = window(marks, "rs_jump", "rs_done")
    if w:
        s = seg(rows, me, *w)
        j = ev_in(evs, me, *w, "Jump")
        if j and s:
            info(results, "rust.jump_height", round(max(r["o"][2] for r in s) - j[0]["o"][2], 1), unit="in")
    first = min((m["tick"] for k, m in marks.items() if k.startswith("ex_") and "tick" in m), default=None)
    alive = [r for r in rows if r["c"] == me and r.get("o") is not None and r["mode"] == "Ground"]
    info(results, "rust.sprint_top", max((hspeed(r) for r in alive), default=None),
         "max ground speed over the whole run (stock-map runs hit cover before 3.4 s)", unit="in/s")
    last = marks.get("ex_done", {}).get("tick")
    places = defaultdict(list)
    if first is not None and last is not None:
        for e in ev_in(evs, me, first, last):
            places[e["kind"]].append(e["o"])
    check(results, "rust.wallruns", len(places.get("WallRunStart", [])), "scripted exploration from spawns")
    climbs = len(places.get("LedgeClimbStart", [])) + len(places.get("WallClimbStart", []))
    check(results, "rust.climbs", climbs, f"ledge {len(places.get('LedgeClimbStart', []))}, wallclimb {len(places.get('WallClimbStart', []))}")
    info(results, "rust.vaults", len(places.get("VaultStart", [])))
    info(results, "rust.event_counts", {k: len(v) for k, v in places.items()})
    return {"model": model, "event_places": {k: v[:12] for k, v in places.items()}}


# ============================================================================ animations

ANIM_SHOTS = ("wallrun", "climb", "ledge", "vault", "slide", "roll")


def anim_cmds() -> str:
    return J(
        PREAMBLE, "thirdperson 1", "wait 0.5s",
        # slide
        "move 354 -1100 0 90 0", "wait 0.5s", "hold +forward", "wait 3.5s", "press +movedown 1.2",
        "wait 0.25s", "screenshot anim_slide_tp", "wait 1.2s", "release +forward", "wait 0.5s",
        # wallrun
        "move -930 433 0 -100 0", "wait 0.4s", "hold +forward", "wait 1.2s", "press +gostand", "wait 0.45s",
        "screenshot anim_wallrun_tp", "wait 0.3s", "screenshot anim_wallrun2_tp", "wait 1.5s", "release +forward",
        "wait 0.4s",
        # wallclimb + ledge
        "move 945 840 0 90 0", "wait 0.4s", "hold +forward", "wait 0.4s", "hold +gostand", "wait 0.3s",
        "screenshot anim_climb_tp", "wait 0.45s", "screenshot anim_ledge_tp", "wait 0.25s",
        "screenshot anim_ledge2_tp", "wait 0.8s", "release +gostand", "release +forward", "wait 0.4s",
        # vault (time-boxed burst; one of them should land mid-vault)
        "move 945 0 0 180 0", "wait 0.4s", "hold +forward", "wait 2.25s", "screenshot anim_vault1_tp", "wait 0.1s",
        "screenshot anim_vault2_tp", "wait 0.1s", "screenshot anim_vault3_tp", "wait 0.1s",
        "screenshot anim_vault4_tp", "wait 0.8s", "release +forward", "wait 0.4s",
        # roll
        "move -650 -880 237 90 0", "wait 0.5s", "hold +forward", "wait 0.9s", "press +movedown 0.3", "wait 0.3s",
        "screenshot anim_roll1_tp", "wait 0.15s", "screenshot anim_roll2_tp", "wait 1.0s", "release +forward",
        "wait 0.5s",
        # first-person: weapon lowered while climbing
        "thirdperson 0", "move 945 840 0 90 0", "wait 0.4s", "hold +forward", "wait 0.4s", "hold +gostand",
        "wait 0.35s", "screenshot anim_climb_fp", "wait 1.2s", "release +gostand", "release +forward", "wait 0.4s",
        "finish_run",
    )


def analyse_anim(run: GameRun, results: list) -> dict:
    """Which screenshot caught which mode: the trace tick nearest each `screenshot: queued`."""
    rows = trace_rows(run)
    me = local_id(run)
    mine = [r for r in rows if r["c"] == me and r.get("o") is not None]
    shots = {}
    tick = None
    for ev in run.events:
        msg = ev["msg"]
        if msg.startswith("trace: t="):
            tick = int(msg[9 : msg.index(" ", 9)])
        elif msg.startswith("screenshot: queued") and tick is not None:
            name = msg.split("screenshots")[-1].strip("\\/ ").split(" ")[0]
            r = min(mine, key=lambda r: abs(r["tick"] - tick), default=None)
            shots[name] = {"tick": tick, "mode": r["mode"] if r else None,
                           "z": round(r["o"][2], 1) if r else None, "lowered": bool(r["pmf"] & LOWERED) if r else None}
    for name, s in shots.items():
        info(results, f"anim.{name}", f"mode={s['mode']} z={s['z']} lowered={s['lowered']}")
    info(results, "anim.files", len(run.screenshots))
    return {"shots": shots}


# ============================================================================ bots


def bots_cmds(minutes: float, bots: int, tag: str = "", screenshots: bool = True) -> str:
    parts = [PREAMBLE, "god", "wait 0.5s", f"bot add {bots}", "mark bots_added", "wait 5s", "mark match_go"]
    secs = int(minutes * 60)
    step = 30
    done = 0
    i = 0
    while done < secs:
        chunk = min(step, secs - done)
        parts += [f"wait {chunk}s", f"mark m_{done + chunk}"]
        done += chunk
        i += 1
        if screenshots and i in (2, 4):
            parts += ["thirdperson 1", "wait 0.2s", f"screenshot bots{tag}_{i}_tp", "thirdperson 0"]
    parts += ["mark match_done", "dump bots_end", "finish_run"]
    return J(*parts)


def _stall_place(alive, stalls):
    if not stalls:
        return None
    s, e, _ = max(stalls, key=lambda x: x[1] - x[0])
    r = next((r for r in alive if r["tick"] == s), None)
    return [round(x) for x in r["o"]] if r else None


def analyse_bots(run: GameRun, results: list, prefix: str) -> dict:
    rows = trace_rows(run)
    evs = mec_events(run)
    kills = kill_lines(run)
    marks = run.marks()
    me = local_id(run)
    w = window(marks, "match_go", "match_done")
    if not w:
        check(results, f"{prefix}.kills_per_min", None, "match marks missing", tkey="bots.kills_per_min")
        return {}
    t0, t1 = w
    minutes = (t1 - t0) * TICK_S / 60
    t_go_ms = marks["match_go"]["t_ms"]
    t_end_ms = marks["match_done"]["t_ms"]
    t_added_ms = marks.get("bots_added", {}).get("t_ms")
    in_match = [k for k in kills if k["t_ms"] is not None and t_go_ms <= k["t_ms"] <= t_end_ms]
    all_after_add = [k for k in kills if t_added_ms is None or (k["t_ms"] is not None and k["t_ms"] >= t_added_ms)]
    frags = [k for k in in_match if k["attacker"] is not None and k["attacker"] >= 0 and k["attacker"] != k["victim"]]
    falls = [k for k in all_after_add if k["mod"] == "MOD_FALLING"]
    ttfk = None
    if all_after_add and t_added_ms is not None:
        firsts = [k for k in all_after_add if k["attacker"] is not None and k["attacker"] >= 0 and k["attacker"] != k["victim"]]
        if firsts:
            ttfk = (firsts[0]["t_ms"] - t_added_ms) / 1000
    bots = sorted({r["c"] for r in rows if r["c"] != me})
    per_bot = {}
    perm = 0
    for b in bots:
        br = sorted((r for r in rows if r["c"] == b and t0 <= r["tick"] <= t1), key=lambda r: r["tick"])
        alive = [r for r in br if r["life"] == "Alive" and r.get("o") is not None]
        moving = sum(1 for r in alive if hspeed(r) > 20)
        # stall: alive, never further than 32 in from where the stall began, for > 3 s
        stalls, anchor, a_tick, last_tick = [], None, None, None
        for r in alive:
            if anchor is not None and last_tick is not None and r["tick"] - last_tick > 2:
                if (last_tick - a_tick) * TICK_S > 3:
                    stalls.append((a_tick, last_tick, False))
                anchor = None
            if anchor is None or dist2(r["o"], anchor) > 32 or abs(r["o"][2] - anchor[2]) > 32:
                if anchor is not None and (last_tick - a_tick) * TICK_S > 3:
                    stalls.append((a_tick, last_tick, False))
                anchor, a_tick = r["o"], r["tick"]
            last_tick = r["tick"]
        if anchor is not None and last_tick is not None and (last_tick - a_tick) * TICK_S > 3:
            stalls.append((a_tick, last_tick, last_tick >= t1 - 2))
        longest = max(((e - s) * TICK_S for s, e, _ in stalls), default=0.0)
        limit = min(60.0, max(20.0, (t1 - t0) * TICK_S / 2))
        permanent = any((e - s) * TICK_S >= limit or (open_ and (e - s) * TICK_S >= limit / 2) for s, e, open_ in stalls)
        perm += int(permanent)
        respawns, prev = 0, None
        for r in br:
            if r["life"] == "Alive" and prev is not None and prev != "Alive":
                respawns += 1
            prev = r["life"]
        bev = Counter(e["kind"] for e in evs if e["c"] == b and e["tick"] is not None and t0 <= e["tick"] <= t1)
        per_bot[b] = {
            "kills": sum(1 for k in frags if k["attacker"] == b),
            "deaths": sum(1 for k in in_match if k["victim"] == b),
            "alive_s": round(len(alive) * TICK_S, 1),
            "moving_pct": round(100 * moving / len(alive), 1) if alive else None,
            "stall_events": len(stalls),
            "longest_stall_s": round(longest, 1),
            "permanent_stuck": permanent,
            "respawns": respawns,
            "parkour": dict(bev),
            "longest_stall_at": _stall_place(alive, stalls),
        }
    deaths_by_mod = Counter(k["mod"] for k in in_match)
    check(results, f"{prefix}.fall_deaths", len(falls),
          f"MOD_FALLING: {sum(1 for k in falls if (k['damage'] or 0) < 1000)} fall damage, "
          f"{sum(1 for k in falls if (k['damage'] or 0) >= 1000)} out-of-bounds (1000 dmg)", tkey="bots.fall_deaths")
    check(results, f"{prefix}.kills", len(frags), f"in {minutes:.2f} min", tkey="bots.kills")
    kpm = len(frags) / minutes if minutes > 0 else None
    check(results, f"{prefix}.kills_per_min", kpm, f"{len(frags)} kills in {minutes:.2f} min", tkey="bots.kills_per_min")
    check(results, f"{prefix}.permanent_stuck", perm, "stalled >=60 s, or >=30 s through the end", tkey="bots.permanent_stuck")
    info(results, f"{prefix}.trace_coverage", trace_coverage([r for r in rows if t0 <= r["tick"] <= t1]))
    info(results, f"{prefix}.time_to_first_kill", ttfk, "from `bot add`", unit="s")
    info(results, f"{prefix}.deaths_by_mod", dict(deaths_by_mod))
    tot_parkour = Counter()
    for b in per_bot.values():
        tot_parkour.update(b["parkour"])
    info(results, f"{prefix}.bot_parkour_events", dict(tot_parkour))
    mv = [b["moving_pct"] for b in per_bot.values() if b["moving_pct"] is not None]
    info(results, f"{prefix}.moving_pct_mean", round(statistics.mean(mv), 1) if mv else None, unit="%")
    info(results, f"{prefix}.stall_events", sum(b["stall_events"] for b in per_bot.values()), ">3 s within 32 in while alive")
    info(results, f"{prefix}.respawns", sum(b["respawns"] for b in per_bot.values()))
    return {"minutes": round(minutes, 2), "per_bot": per_bot, "kills": len(frags), "fall_deaths": falls,
            "deaths_by_mod": dict(deaths_by_mod), "ttfk_s": ttfk}


# ============================================================================ autopilot (testbox)
# The playground (scripts/mec-testbox.py) writes the movement scenarios as autopilot routes
# (<arena>/routes/<name>.json) and the showcase line as <arena>/route.json. `autopilot` drives
# them closed-loop with one usercmd per sim tick (lockstep with the listen authority), so a
# run is a pure function of the build + arena: the same inputs on the same ticks every time.

MOVEMENT_SCENARIOS = ("sprint", "jumps", "wallrun", "wallrun_ads", "wallrun_fire", "wallclimb", "vault",
                      "roll", "hard", "wall_turn")
# caption prefixes of a scenario -> the window labels the analysis reads
SCENARIO_LABELS = {"wallrun": "wr1", "wallrun_ads": "wr2", "wallrun_fire": "wr3"}

AP_PREAMBLE = "wait world; mark world; spawn 0; mark ingame; force_match_start; wait 1s"


def route_start(arenas: str, name: str | None) -> str:
    import json as _json
    from pathlib import Path as _Path

    base = _Path(arenas) / "testbox"
    path = base / "route.json" if name is None else base / "routes" / f"{name}.json"
    return _json.loads(path.read_text(encoding="utf-8"))["start"]["move"]


def movement_autopilot_cmds(arenas: str) -> str:
    parts = [AP_PREAMBLE]
    for name in MOVEMENT_SCENARIOS:
        parts += [route_start(arenas, name), "wait 20t", f"autopilot mec:testbox/{name}", "wait 6t"]
    parts += ["finish_run"]
    return J(*parts)


def showcase_cmds(arenas: str) -> str:
    return J(AP_PREAMBLE, "god", "give weapon/iw4:masada_mp", "wait 2s", route_start(arenas, None), "wait 20t",
             "mark showcase_go", "autopilot mec:testbox", "mark showcase_done", "wait 10t", "finish_run")


_AP_START = re.compile(r"autopilot: start (\S+)")
_AP_CAPTION = re.compile(r"autopilot: (?:act|seg) \S+ \S+ tick=(\d+) .*?caption=\"([^\"]+)\"")


def autopilot_lines(run: GameRun) -> list:
    return [ev["msg"] for ev in run.events if ev["msg"].startswith("autopilot:")]


def autopilot_marks(run: GameRun) -> dict:
    """Window marks from the scenario captions: {label: {"tick": t}} (label = caption, or
    wr1_go / wr2_go / wr3_go for the three wallrun scenarios)."""
    marks, scenario = {}, None
    for msg in autopilot_lines(run):
        m = _AP_START.search(msg)
        if m:
            scenario = m.group(1).split("/")[-1]
            continue
        m = _AP_CAPTION.search(msg)
        if m:
            label = m.group(2)
            prefix = SCENARIO_LABELS.get(scenario or "")
            if prefix and label.startswith("wr_"):
                label = prefix + label[2:]
            marks.setdefault(label, {"tick": int(m.group(1))})
    return marks


def autopilot_outcome(run: GameRun) -> tuple[int, int, list]:
    lines = autopilot_lines(run)
    ok = sum(1 for l in lines if l.startswith("autopilot: done ok"))
    fails = [l for l in lines if "FAIL" in l]
    return ok, len([l for l in lines if l.startswith("autopilot: start")]), fails


def compare_runs(a: GameRun, b: GameRun) -> dict:
    """Bit-identity of two autopilot runs, route by route (tools/harness/determinism.py)."""
    import determinism as det

    if not (a.jsonl_path and b.jsonl_path):
        return {"identical": None, "note": "missing trace"}
    sa, sb = det.load_routes(a.jsonl_path, "0"), det.load_routes(b.jsonl_path, "0")
    out = {"routes": len(sa), "ticks_compared": 0, "ticks_differing": 0, "autopilot_lines_equal": True,
           "first_difference": None}
    if [x["route"] for x in sa] != [x["route"] for x in sb]:
        out.update(identical=False, note="different routes ran")
        return out
    for ra, rb in zip(sa, sb):
        cmp = det.compare(ra, rb)
        out["ticks_compared"] += cmp["compared"]
        out["ticks_differing"] += cmp["differing"]
        out["autopilot_lines_equal"] &= cmp["lines_equal"]
        if out["first_difference"] is None and (cmp["first"] or not cmp["lines_equal"]):
            out["first_difference"] = {"route": ra["route"], **(cmp["first"] or {"line": cmp["first_line"]})}
    out["identical"] = out["autopilot_lines_equal"] and out["ticks_differing"] == 0 and out["ticks_compared"] > 0
    return out


def analyse_movement_autopilot(runs: list, results: list) -> dict:
    """Movement numbers from the first run (windows from autopilot captions), determinism
    across all runs, and every scenario must end `done ok`."""
    run = runs[0]
    data = analyse_movement_testbox(run, results, marks=autopilot_marks(run))
    ok, started, fails = autopilot_outcome(run)
    check(results, "movement.scenarios_ok", ok, f"{ok}/{started} done ok; {fails[:2]}", tkey="movement.scenarios_ok")
    for i, other in enumerate(runs[1:], 1):
        cmp = compare_runs(run, other)
        data[f"determinism_{i}"] = cmp
        check(results, "movement.deterministic", 1 if cmp["identical"] else 0,
              f"run 1 vs {i + 1}: {cmp['ticks_compared']} ticks compared, {cmp['ticks_differing']} differ; "
              f"autopilot log equal={cmp.get('autopilot_lines_equal')}")
    return data


def analyse_showcase(runs: list, results: list) -> dict:
    run = runs[0]
    data = {"runs": []}
    for i, r in enumerate(runs):
        ok, started, fails = autopilot_outcome(r)
        lines = autopilot_lines(r)
        caps = [m.groups() for m in (_AP_CAPTION.search(l) for l in lines) if m]
        steps = sum(1 for l in lines if " ok mode=" in l)
        data["runs"].append({"done_ok": ok, "fails": fails, "steps_ok": steps, "captions": len(caps)})
        check(results, "showcase.done_ok", ok, f"run {i + 1}: {steps} validated steps; {fails[:1]}",
              tkey="showcase.done_ok")
    t0 = None
    for m in (_AP_CAPTION.search(l) for l in autopilot_lines(run)):
        if m:
            t0 = int(m.group(1)) if t0 is None else t0
            data.setdefault("captions", []).append({"t_s": round((int(m.group(1)) - t0) * TICK_S, 2),
                                                    "caption": m.group(2)})
    for i, other in enumerate(runs[1:], 1):
        cmp = compare_runs(run, other)
        data[f"determinism_{i}"] = cmp
        check(results, "showcase.deterministic", 1 if cmp["identical"] else 0,
              f"run 1 vs {i + 1}: {cmp['ticks_compared']} ticks compared, {cmp['ticks_differing']} differ")
    # View smoothness while the autopilot steers: from its first command to the quickturn
    # (the game's own 180 turn), at most YAW_STEP_MAX (10 deg) per 50 ms command.
    ticks = [int(m.group(1)) for m in (_AP_CAPTION.search(l) for l in autopilot_lines(run)) if m]
    qt = [int(m.group(1)) for m in (_AP_CAPTION.search(l) for l in autopilot_lines(run))
          if m and m.group(2) == "Quickturn"]
    lo, hi = (ticks[0], qt[0] if qt else 10 ** 9) if ticks else (0, 0)
    rows = trace_rows(run)
    yaws = sorted((r["tick"], r["yaw"]) for r in rows if r["c"] == local_id(run) and r.get("o") and lo <= r["tick"] < hi)
    steps = [abs(wrap(b[1] - a[1])) for a, b in zip(yaws, yaws[1:]) if b[0] - a[0] == 1]
    check(results, "showcase.max_yaw_per_tick", round(max(steps), 1) if steps else None,
          "deg per 50 ms tick while steering (quickturn excluded)", unit="deg")
    return data


def collision_coverage(arena_dir: str, results: list) -> dict:
    import subprocess
    import sys as _sys
    from pathlib import Path as _Path

    tool = _Path(__file__).resolve().parents[1] / "mec" / "check_collision_coverage.py"
    import shutil as _shutil
    uv = _shutil.which("uv")
    cmd = [uv, "run", "--python", "3.13", str(tool), arena_dir] if uv else [_sys.executable, str(tool), arena_dir]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=1200).stdout
    m = re.search(r"covered: all ([\d.]+)%\s+walls ([\d.]+)%\s+floors ([\d.]+)%", out)
    val = float(m.group(1)) if m else None
    check(results, "coverage.all_pct", val, "visible render surface with collision within 5 cm", unit="%")
    if m:
        info(results, "coverage.walls_pct", float(m.group(2)), unit="%")
        info(results, "coverage.floors_pct", float(m.group(3)), unit="%")
    return {"output": out[-2000:]}
