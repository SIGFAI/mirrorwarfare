# /// script
# requires-python = ">=3.12"
# dependencies = ["psutil>=5.9"]
# ///
"""Mirrorwarfare end-to-end harness: plays the game from console scripts, reports numbers.

    uv run tools/harness/run.py --suite all [--exe path\\to\\iw4l.exe]
    uv run tools/harness/run.py --suite movement,bots --bot-minutes 2
    uv run tools/harness/run.py --rerender <mirrorwarfare>/harness-runs/<stamp>

Suites: load, movement, showcase, coverage, perf, bots, stability (all = these, bots last;
`anim` and `--rust` are opt-in). movement and showcase run twice and must be bit-identical
(autopilot + fixed-step input); bots is a short smoke match. Every launch sets
IW4L_SOUND=off unless --sound (then the load suite also measures sound on). Output goes to
<mirrorwarfare>/harness-runs/<stamp>/ (report.md, report.json, logs/,
screenshots/, traces/). One game at a time; only processes this harness started are ever killed.
Needs a build with the console trace hook (IW4L_TRACE_PLAYER=1, crates/console/src/trace_player.rs)
for the per-tick numbers; without it the movement/bot numbers come out MISS.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import platform
import shutil
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import game  # noqa: E402
import suites as S  # noqa: E402
from report import render  # noqa: E402

OUT_ROOT = game.WORK_ROOT / "harness-runs"
WARM = OUT_ROOT / "_warm-artifacts"
GAMES = "C:/Program Files (x86)/Steam/steamapps/common"
ARENAS = str(game.WORK_ROOT / "mec-arenas")
ALL = ["load", "movement", "showcase", "coverage", "perf", "bots", "stability"]
OPT_IN = ["anim"]
# Sound-on load times measured before the harness went sound-off by default (lead's figures,
# 2026-10-04, process start -> in game). Cited in the report; not re-measured unless --sound.
PRIOR_SOUND_ON_LOAD = "mec:mec_anchor_1 warm ~25 s, cold ~64 s; mp_boneyard warm ~20 s (prior measurement, sound on)"
LOAD_ZONES = ["mec:testbox", "mp_boneyard"]


def harness_arenas(echo) -> str:
    """IW4L_MEC_ARENAS for the harness: junctions to every arena in mec-arenas, plus a testbox
    freshly generated from scripts/mec-testbox.py (the course: wallrun wall, ledge wall, vault box,
    drop tower). mec-arenas/testbox itself may predate the course, so it is never used."""
    import subprocess

    root = OUT_ROOT / "_arenas"
    root.mkdir(parents=True, exist_ok=True)
    src = Path(ARENAS)
    for entry in src.iterdir():
        if entry.name == "testbox" or not entry.is_dir():
            continue
        link = root / entry.name
        if not link.exists():
            subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(entry)], check=True, capture_output=True)
    script = Path(__file__).resolve().parents[2] / "scripts" / "mec-testbox.py"
    r = subprocess.run([sys.executable, str(script), str(root / "testbox")], capture_output=True, text=True)
    echo(f"arenas {root}: testbox regenerated ({r.stdout.strip() or r.stderr.strip()})")
    return str(root)


def sha256(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Harness:
    def __init__(self, args):
        self.args = args
        self.replay = None
        if args.reanalyse:
            # Re-run every analysis on the logs of a finished run, with the current thresholds.
            self.out = Path(args.reanalyse)
            old = json.loads((self.out / "report.json").read_text(encoding="utf-8"))
            self.replay = {r["name"]: r for r in old["runs"]}
            self.exe = self.out / "bin" / "iw4l.exe"
            self.runs = []
            self.report = {"meta": {**old["meta"], "reanalysed": dt.datetime.now().isoformat(timespec="seconds")},
                           "suites": {}, "runs": []}
            self.logf = open(self.out / "harness.log", "a", encoding="utf-8")
            self.arenas = old["meta"].get("arenas", ARENAS)
            self.exe_src = Path(old["meta"]["exe"])
            return
        self.exe = Path(args.exe) if args.exe else game.find_exe()
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        self.out = Path(args.out) if args.out else OUT_ROOT / stamp
        self.out.mkdir(parents=True, exist_ok=True)
        self.runs: list[game.GameRun] = []
        # Run a private copy: a build that lands mid-run must be able to replace the source exe,
        # and every suite of this report runs the same binary.
        self.exe_src = self.exe
        (self.out / "bin").mkdir(exist_ok=True)
        self.exe = self.out / "bin" / "iw4l.exe"
        shutil.copy2(self.exe_src, self.exe)
        self.report = {
            "meta": {
                "started": dt.datetime.now().isoformat(timespec="seconds"),
                "exe": str(self.exe_src),
                "exe_mtime": dt.datetime.fromtimestamp(self.exe_src.stat().st_mtime).isoformat(timespec="seconds"),
                "exe_sha256": sha256(self.exe),
                "host": platform.node(),
                "args": vars(args),
                "foreign_iw4l_pids_at_start": game.foreign_game_pids(),
                "out": str(self.out),
            },
            "suites": {},
            "runs": [],
        }
        self.report["meta"]["sound"] = bool(args.sound)
        if not args.sound:
            self.report["meta"]["prior_sound_on_load"] = PRIOR_SOUND_ON_LOAD
        self.logf = open(self.out / "harness.log", "a", encoding="utf-8")
        self.arenas = harness_arenas(self.echo)
        self.report["meta"]["arenas"] = self.arenas

    def echo(self, msg):
        line = f"{dt.datetime.now().strftime('%H:%M:%S')} {msg}"
        print(line, flush=True)
        self.logf.write(line + "\n")
        self.logf.flush()

    def play(self, name, zone, cmds, *, timeout, sound=None, bench=False, movement=None, artifacts=None):
        if self.replay is not None:
            if name not in self.replay:
                raise KeyError(f"--reanalyse: no run named {name}")
            run = game.load_run(self.out, self.replay[name], self.exe)
            self.report["runs"].append(self.replay[name])
            self.runs.append(run)
            return run
        env = game.base_env(GAMES, self.arenas)
        if sound is None:
            sound = self.args.sound
        if not sound:
            env["IW4L_SOUND"] = "off"
        if bench:
            env["IW4L_BENCH"] = "1"
        if movement:
            env["IW4L_MOVEMENT"] = movement
        foreign = game.foreign_game_pids()
        if foreign:
            self.echo(f"note: other iw4l.exe running (not ours, left alone): {foreign}")
        run = game.run_game(
            name=name, exe=self.exe, zone=zone, cmds=cmds, out_dir=self.out, env=env,
            artifacts_dir=artifacts or WARM, timeout_s=timeout, echo=self.echo,
        )
        summary = run.summary()
        summary["foreign_iw4l_pids"] = foreign
        self.report["runs"].append(summary)
        self.runs.append(run)
        self.save()
        return run

    def suite(self, name):
        return self.report["suites"].setdefault(name, {"checks": [], "data": {}})

    def save(self):
        (self.out / "report.json").write_text(json.dumps(self.report, indent=2, default=str), encoding="utf-8")
        (self.out / "report.md").write_text(render(self.report, self.out), encoding="utf-8")

    # ------------------------------------------------------------------ suites

    def movement(self):
        su = self.suite("movement")
        runs = [self.play(f"move_testbox_{i + 1}", "mec:testbox", S.movement_autopilot_cmds(self.arenas), timeout=900)
                for i in range(2)]
        su["data"]["testbox"] = S.analyse_movement_autopilot(runs, su["checks"])
        self.save()
        if self.args.rust:
            r = self.play("move_rust_mec", "mp_rust", S.rust_cmds(), timeout=1500, movement="mec")
            su["data"]["rust"] = S.analyse_rust(r, su["checks"])
            self.save()

    def showcase(self):
        su = self.suite("showcase")
        runs = [self.play(f"showcase_{i + 1}", "mec:testbox", S.showcase_cmds(self.arenas), timeout=900, bench=True)
                for i in range(2)]
        su["data"] = S.analyse_showcase(runs, su["checks"])
        self.perf_from(runs[0], "showcase")
        self.save()

    def coverage(self):
        su = self.suite("coverage")
        su["data"] = S.collision_coverage(str(Path(self.arenas) / "testbox"), su["checks"])
        self.save()

    def anim(self):
        su = self.suite("anim")
        r = self.play("anim_testbox", "mec:testbox", S.anim_cmds(), timeout=900)
        su["data"] = S.analyse_anim(r, su["checks"])
        su["data"]["screenshots"] = [str(p.relative_to(self.out)) for p in r.screenshots]
        self.save()

    def bots(self):
        su = self.suite("bots")
        minutes, n = self.args.bot_minutes, self.args.bots
        timeout = minutes * 60 + 900
        for zone, key in (("mec:testbox", "testbox"),):
            r = self.play(f"bots_{key}", zone, S.bots_cmds(minutes, n, f"_{key}"), timeout=timeout, bench=True)
            su["data"][key] = S.analyse_bots(r, su["checks"], f"bots_{key}")
            su["data"][key]["screenshots"] = [str(p.relative_to(self.out)) for p in r.screenshots]
            self.perf_from(r, key)
        self.save()

    def perf_from(self, run, key):
        su = self.suite("perf")
        b = game.parse_bench(run.bench_path)
        su["data"][key] = {**b, "peak_rss_mb": round(run.peak_rss_mb, 1), "peak_wset_mb": round(run.peak_wset_mb, 1)}
        marks = run.marks()
        rss = [run.rss_at(m["s"]) for k, m in marks.items() if k.startswith("m_")]
        rss = [round(x) for x in rss if x]
        if rss:
            su["data"][key]["rss_mib_during_match"] = rss
        S.check(su["checks"], f"perf.{key}.fps_avg", b.get("fps_avg"), "IW4L_BENCH=1 gameplay frames", tkey="perf.fps_avg")
        S.check(su["checks"], f"perf.{key}.frame_ms_p99", b.get("frame_ms_p99"), unit="ms", tkey="perf.frame_ms_p99")
        for k in ("frame_ms_avg", "frame_ms_p50", "frame_ms_p95", "frame_ms_max"):
            S.info(su["checks"], f"perf.{key}.{k}", b.get(k), unit="ms")
        S.check(su["checks"], f"perf.{key}.peak_rss_mb", round(run.peak_rss_mb), unit="MiB", tkey="perf.peak_rss_mb")

    def perf(self):
        if "perf" not in self.report["suites"] or not self.report["suites"]["perf"]["data"]:
            # perf alone: the showcase route under IW4L_BENCH=1
            r = self.play("perf_showcase", "mec:testbox", S.showcase_cmds(self.arenas), timeout=900, bench=True)
            self.perf_from(r, "showcase")
        self.save()

    def load(self):
        su = self.suite("load")
        rows = []
        n = self.args.load_runs
        zones = self.args.load_zones.split(",") if self.args.load_zones else LOAD_ZONES
        for zone in zones:
            tag = zone.replace(":", "_")
            # Sound is off unless --sound; with --sound the load suite measures both variants.
            for sound in ((True, False) if self.args.sound else (False,)):
                st = "sound" if sound else "nosound"
                for i in range(n):
                    if self.replay is not None and f"load_{tag}_{st}_cold{i}" not in self.replay:
                        continue
                    cold = self.out / "cold" / f"{tag}_{st}_{i}"
                    r = self.play(f"load_{tag}_{st}_cold{i}", zone, S.load_cmds(), timeout=self.args.cold_timeout,
                                  sound=sound, bench=True, artifacts=cold)
                    rows.append({"zone": zone, "sound": sound, "cache": "cold", "i": i, **S.analyse_load(r)})
                    if self.replay is None:
                        shutil.rmtree(cold, ignore_errors=True)  # our own throwaway cache
                    self.save()
                prime = WARM / f"_primed_{tag}_{st}"
                need_prime = (f"load_{tag}_{st}_prime" in self.replay) if self.replay is not None else not prime.exists()
                if need_prime:
                    r = self.play(f"load_{tag}_{st}_prime", zone, S.load_cmds(), timeout=self.args.cold_timeout,
                                  sound=sound, bench=True)
                    rows.append({"zone": zone, "sound": sound, "cache": "prime", "i": 0, **S.analyse_load(r)})
                    prime.write_text("primed\n")
                for i in range(n):
                    if self.replay is not None and f"load_{tag}_{st}_warm{i}" not in self.replay:
                        continue
                    r = self.play(f"load_{tag}_{st}_warm{i}", zone, S.load_cmds(), timeout=900, sound=sound, bench=True)
                    rows.append({"zone": zone, "sound": sound, "cache": "warm", "i": i, **S.analyse_load(r)})
                    su["data"]["runs"] = rows
                    self.save()
        su["data"]["runs"] = rows
        su["data"]["agg"] = aggregate_load(rows)
        for a in su["data"]["agg"]:
            S.info(su["checks"], f"load.{a['zone']}.{'sound' if a['sound'] else 'nosound'}.{a['cache']}.ingame_s",
                   a["ingame_s"]["median"] if a["ingame_s"] else None,
                   f"median of {a['n']} (min {a['ingame_s']['min'] if a['ingame_s'] else '-'}, max {a['ingame_s']['max'] if a['ingame_s'] else '-'})",
                   unit="s")
        failed = [r for r in rows if not r["ok"]]
        su["checks"].append({"key": "load.all_reached_ingame", "value": len(rows) - len(failed), "range": f"[{len(rows)} .. {len(rows)}]",
                             "status": "PASS" if not failed else "FAIL", "unit": "", "note": ", ".join(f"{r['zone']}/{r['cache']}{r['i']}" for r in failed)})
        self.save()

    def stability(self):
        su = self.suite("stability")
        pan, unclean, timeouts, errs, pred = [], [], [], {}, {}
        for r in self.runs:
            p = game.panics(r)
            if p:
                pan.append({"run": r.name, "lines": p})
            code = game.process_exit_code(r)
            if r.returncode != 0 or code not in (0, None) or (code is None and not r.timed_out):
                unclean.append({"run": r.name, "returncode": r.returncode, "logged_exit": code})
            if r.timed_out:
                timeouts.append(r.name)
            for e in game.gsc_errors(r):
                k = f"{e['at']} {e['function']}: {e['fault']}"
                errs.setdefault(k, {"count": 0, "runs": set()})
                errs[k]["count"] += 1
                errs[k]["runs"].add(r.name)
            pp = game.prediction_probe(r)
            if pp:
                pred[r.name] = pp
        S.check(su["checks"], "stability.panics", sum(len(p["lines"]) for p in pan), f"{len(self.runs)} runs")
        S.check(su["checks"], "stability.unclean_exits", len(unclean), "; ".join(f"{u['run']} rc={u['returncode']} log={u['logged_exit']}" for u in unclean))
        S.check(su["checks"], "stability.timeouts", len(timeouts), ", ".join(timeouts))
        S.info(su["checks"], "stability.gsc_runtime_errors_total", sum(v["count"] for v in errs.values()))
        S.info(su["checks"], "stability.gsc_runtime_errors_distinct", len(errs))
        S.info(su["checks"], "stability.prediction_deviations",
               {k: v["dev"] for k, v in pred.items()} if pred else "not observable (no PROBE prediction lines in this build)")
        su["data"] = {
            "panics": pan, "unclean": unclean, "timeouts": timeouts,
            "gsc_errors": sorted(({"error": k, "count": v["count"], "runs": len(v["runs"])} for k, v in errs.items()),
                                 key=lambda x: -x["count"]),
            "prediction": pred,
        }
        self.save()


def aggregate_load(rows):
    out = []
    keys = sorted({(r["zone"], r["sound"], r["cache"]) for r in rows if r["cache"] != "prime"})
    for zone, sound, cache in keys:
        rs = [r for r in rows if (r["zone"], r["sound"], r["cache"]) == (zone, sound, cache)]
        agg = {"zone": zone, "sound": sound, "cache": cache, "n": len(rs)}
        for m in ("first_stdout_s", "renderer_up_s", "world_s", "ingame_s", "bench_command_to_playable_s",
                  "bench_match_installed_s", "peak_rss_mb", "wall_s"):
            vals = [r[m] for r in rs if r.get(m) is not None]
            agg[m] = {"median": round(statistics.median(vals), 2), "min": round(min(vals), 2), "max": round(max(vals), 2)} if vals else None
        out.append(agg)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", default="all", help="comma list of " + ",".join(ALL + OPT_IN) + " or all")
    ap.add_argument("--exe", help="iw4l.exe (default: newest in iw4L/target/play, iw4l-target-*/play)")
    ap.add_argument("--out", help="output dir (default harness-runs/<stamp>)")
    ap.add_argument("--bots", type=int, default=4)
    ap.add_argument("--bot-minutes", type=float, default=1.0)
    ap.add_argument("--rust", action="store_true", help="movement: also explore mp_rust with IW4L_MOVEMENT=mec")
    ap.add_argument("--sound", action="store_true",
                    help="play with sound (default: every launch sets IW4L_SOUND=off; load then measures on and off)")
    ap.add_argument("--load-runs", type=int, default=2, help="measured runs per (map, sound, cache) cell")
    ap.add_argument("--load-zones", help="comma list (default " + ",".join(LOAD_ZONES) + ")")
    ap.add_argument("--cold-timeout", type=float, default=1800)
    ap.add_argument("--reanalyse", help="re-run the analyses (current thresholds) on an existing run dir's logs; no game")
    ap.add_argument("--rerender", help="rebuild report.md of an existing run dir (picks up animation_review.json)")
    args = ap.parse_args()

    if args.rerender:
        out = Path(args.rerender)
        rep = json.loads((out / "report.json").read_text(encoding="utf-8"))
        (out / "report.md").write_text(render(rep, out), encoding="utf-8")
        print(out / "report.md")
        return

    if args.reanalyse and args.suite == "all":
        old = json.loads((Path(args.reanalyse) / "report.json").read_text(encoding="utf-8"))
        args.suite = ",".join(old["suites"].keys()) or "all"
    want = ALL if args.suite == "all" else [s.strip() for s in args.suite.split(",")]
    h = Harness(args)
    h.echo(f"exe {h.exe_src} ({h.report['meta']['exe_mtime']}); out {h.out}; suites {want}")
    t0 = time.time()
    for name in ALL + OPT_IN:
        if name in want and name != "stability":
            try:
                getattr(h, name)()
            except Exception as e:  # keep going: one broken suite must not hide the others
                import traceback

                h.echo(f"suite {name} crashed: {e!r}")
                h.suite(name)["error"] = traceback.format_exc()
                h.save()
    if "stability" in want or args.suite == "all":
        h.stability()
    h.report["meta"]["finished"] = dt.datetime.now().isoformat(timespec="seconds")
    h.report["meta"]["duration_min"] = round((time.time() - t0) / 60, 1)
    h.save()
    h.echo(f"done in {h.report['meta']['duration_min']} min -> {h.out / 'report.md'}")


if __name__ == "__main__":
    main()
