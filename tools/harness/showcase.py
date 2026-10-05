# /// script
# requires-python = ">=3.13"
# ///
"""Record one continuous showcase run and render it as a first/third-person video.

1. Live: run the route script once on the arena while recording a demo.
2. Playback twice (thirdperson 0, then 1): only the game window is captured, by
   handle, with ffmpeg gfxcapture (Windows Graphics Capture) at 60 fps, so other
   windows on screen never end up in the video.
3. Both captures are trimmed to the demo's first frame (engine trace timestamps),
   captioned from the move timeline, and joined: the whole run in first person,
   then the same run in third person.

  uv run --python 3.13 tools/harness/showcase.py --cmds tools/harness/showcase.cmds \
      [--moves tools/harness/showcase.json] [--arena testbox] [--exe PATH] [--out DIR]

The route file holds console commands (one per line or ';'-separated) that run after
spawning; it must not teleport mid-run. The optional moves json is a list of
{"t": seconds_from_record_start, "label": "Wallrun"} captions.
"""

import argparse
import ctypes
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MW = ROOT.parent
FFMPEG = MW.parent / "tools" / "ffmpeg-9.0.2-essentials_build" / "bin" / "ffmpeg.exe"
GAMES = "C:/Program Files (x86)/Steam/steamapps/common"
W, H = 1920, 1080
FOV = 80

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetProcessDPIAware()
EnumProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def find_window(pid: int):
    found = []

    def cb(hwnd, _):
        p = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(p))
        if p.value == pid and user32.IsWindowVisible(hwnd):
            r = wintypes.RECT()
            user32.GetClientRect(hwnd, ctypes.byref(r))
            if r.right >= W and r.bottom >= H:
                found.append(hwnd)
        return True

    user32.EnumWindows(EnumProc(cb), 0)
    return found[0] if found else None


def newest_exe() -> Path:
    cands = [ROOT / "target" / "play" / "iw4l.exe", *MW.glob("iw4l-target-*/play/iw4l.exe")]
    return max((c for c in cands if c.exists()), key=lambda p: p.stat().st_mtime)


def env(run_dir: Path) -> dict:
    e = dict(os.environ)
    e.update(
        IW4L_SOUND="off",
        IW4L_GAMES=e.get("IW4L_GAMES", GAMES),
        IW4L_MEC_ARENAS=e.get("IW4L_MEC_ARENAS", str(MW / "mec-arenas")),
        IW4L_ARTIFACTS_DIR=str(run_dir / "artifacts"),
        IW4L_TRACES_DIR=str(run_dir / "traces"),
    )
    return e


def run_game(exe: Path, args: list[str], run_dir: Path, log: Path, timeout: float):
    with open(log, "w") as fh:
        p = subprocess.Popen([str(exe), *args], cwd=exe.parent, env=env(run_dir), stdout=fh, stderr=subprocess.STDOUT)
        try:
            p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            raise SystemExit(f"game timed out: {args}")
    return p.returncode


def mark_ms(trace: Path, name: str) -> int | None:
    for line in trace.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg = json.dumps(row)
        if "mark" in msg and name in msg:
            return int(row.get("t", 0))
    return None


def autopilot_captions(log_text: str) -> list[dict]:
    """Caption timeline from `autopilot: seg N <id> tick=T ... caption="..."` lines (20 Hz ticks)."""
    import re
    rows = []
    for line in log_text.splitlines():
        m = re.search(r"autopilot: (?:seg|act) \S+ \S+ tick=(\d+).*?caption=\"([^\"]*)\"", line)
        if m and m.group(2):
            rows.append((int(m.group(1)), m.group(2)))
    if not rows:
        return []
    t0 = rows[0][0]
    out = []
    for tick, label in rows:
        if out and out[-1]["label"] == label:
            continue
        out.append({"t": round((tick - t0) / 20.0, 2), "label": label})
    return out


def newest(dir_: Path, pattern: str) -> Path:
    return max(dir_.glob(pattern), key=lambda p: p.stat().st_mtime)


def capture_playback(exe: Path, zone: str, demo: str, third: int, dur: float, run_dir: Path, tag: str):
    cmds = f"wait world; ui 0; thirdperson {third}; mark sc_start; wait {dur:.1f}s; finish_run"
    traces = run_dir / "traces"
    before = set(traces.glob("*.jsonl")) if traces.exists() else set()
    log = open(run_dir / f"{tag}.log", "w")
    proc = subprocess.Popen(
        [str(exe), "play", demo, "--zone", zone, "--cmds", cmds],
        cwd=exe.parent, env=env(run_dir), stdout=log, stderr=subprocess.STDOUT,
    )
    t_proc = time.time()
    hwnd = None
    for _ in range(600):
        hwnd = find_window(proc.pid)
        if hwnd or proc.poll() is not None:
            break
        time.sleep(0.5)
    if not hwnd:
        raise SystemExit("game window never appeared")
    raw = run_dir / f"{tag}_raw.mp4"
    ff = subprocess.Popen(
        [str(FFMPEG), "-y", "-loglevel", "error", "-f", "lavfi", "-i",
         f"gfxcapture=hwnd={int(hwnd)}:capture_cursor=0:max_framerate=60,hwdownload,format=bgra,"
         f"fps=60,scale={W}:{H}",
         "-c:v", "h264_nvenc", "-preset", "p5", "-cq", "18", "-pix_fmt", "yuv420p", str(raw)],
        stdin=subprocess.PIPE,
    )
    t_ff = time.time()
    proc.wait()
    ff.communicate(b"q", timeout=30)
    trace = next(iter(set(traces.glob("*.jsonl")) - before), None) or newest(traces, "*.jsonl")
    start = mark_ms(trace, "sc_start")
    if start is None:
        raise SystemExit(f"no sc_start mark in {trace}")
    offset = (t_proc + start / 1000.0) - t_ff
    return raw, max(offset, 0.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cmds", type=Path)
    ap.add_argument("--autopilot", action="store_true",
                    help="drive the arena's route.json with the engine autopilot instead of --cmds")
    ap.add_argument("--recompose", type=Path, help="rebuild the video from an existing run dir")
    ap.add_argument("--moves", type=Path)
    ap.add_argument("--arena", default="testbox")
    ap.add_argument("--map", help="stock map zone (e.g. mp_highrise); forces Catalyst movement")
    ap.add_argument("--route", type=Path, help="autopilot route file (with --autopilot); default the arena's route.json")
    ap.add_argument("--third-person", action="store_true", help="also append a third-person replay")
    ap.add_argument("--exe", type=Path)
    ap.add_argument("--out", type=Path)
    ap.add_argument(
        "--spawn",
        default="spawn 0; force_match_start; god; give weapon/iw4:masada_mp; wait 8s",
        help="commands after the world loads and before recording starts",
    )
    a = ap.parse_args()

    if a.recompose:
        run_dir = a.recompose.resolve()
        meta = json.loads((run_dir / "capture.json").read_text())
        tp = Path(meta["tp"]) if meta.get("tp") and a.third_person else None
        compose(run_dir, Path(meta["fp"]), meta["off_fp"], tp, meta.get("off_tp", 0.0), meta["dur"], a.moves)
        return
    if not a.cmds and not a.autopilot:
        ap.error("--cmds or --autopilot is required unless --recompose is given")
    sys.path.insert(0, str(Path(__file__).parent))
    import exclusive

    exclusive.LOCK.touch(exist_ok=True)
    lock_fh = open(exclusive.LOCK, "r+b")
    exclusive.acquire(lock_fh)
    exclusive.LOCK.with_suffix(".owner").write_text(f"pid {os.getpid()} showcase recording")
    src_exe = (a.exe or newest_exe()).resolve()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = (a.out or MW / "showcase-runs" / stamp).resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    # Run a private copy so a rebuild of the source exe is never blocked by the lock.
    exe = run_dir / "bin" / "iw4l.exe"
    exe.parent.mkdir(exist_ok=True)
    import shutil
    shutil.copy2(src_exe, exe)
    artifacts = run_dir / "artifacts"
    artifacts.mkdir(exist_ok=True)
    (artifacts / "settings.cfg").write_text(
        f"// IW4L user settings v2\nresolution={W}x{H}\nfullscreen=false\nvsync=true\nfov={FOV}\n"
        "third_person=false\nshadows=true\ndepth_of_field=true\nbloom=true\n"
    )
    spawn = a.spawn
    if a.autopilot:
        arenas = Path(env(run_dir)["IW4L_MEC_ARENAS"])
        route_file = a.route.resolve() if a.route else arenas / a.arena / "route.json"
        start = json.loads(route_file.read_text())["start"]["move"]
        spawn = f"{spawn}; {start}; wait 20t"
        route = f"autopilot {route_file.as_posix()} exit" if a.route else f"autopilot mec:{a.arena} exit"
    else:
        route = "; ".join(l.strip() for l in a.cmds.read_text().splitlines() if l.strip() and not l.strip().startswith("#"))
    demo = f"showcase_{stamp.replace('-', '_')}"

    print(f"exe {exe}\nrun {run_dir}")
    live = f"wait world; {spawn}; record {demo}; {route}; stoprecord; wait 1s; finish_run"
    zone = a.map or f"mec:{a.arena}"
    if a.map:
        os.environ["IW4L_MOVEMENT"] = "mec"
    code = run_game(exe, ["map", zone, "--cmds", live], run_dir, run_dir / "live.log", 900)
    log_text = (run_dir / "artifacts" / "logs" / "latest.log").read_text(errors="replace")
    if a.autopilot:
        if code != 0 or "autopilot: done ok" not in log_text:
            fails = [l for l in log_text.splitlines() if "autopilot: FAIL" in l]
            raise SystemExit(f"autopilot run failed (exit {code}): {fails[:3]}")
        a.moves = run_dir / "moves.json"
        a.moves.write_text(json.dumps(autopilot_captions(log_text)))
    demo_file = newest(run_dir / "artifacts" / "demos", "*.iw4ldemo")
    ticks = None
    for line in (run_dir / "artifacts" / "logs" / "latest.log").read_text(errors="replace").splitlines():
        if "stoprecord:" in line and "ticks" in line:
            ticks = int(line.split("stoprecord:")[1].split("ticks")[0].strip())
    dur = (ticks or 1200) / 20.0 + 1.5
    print(f"demo {demo_file.name}: {ticks} ticks, {dur:.1f}s")

    fp, off_fp = capture_playback(exe, zone, demo_file.stem, 0, dur, run_dir, "fp")
    tp, off_tp = (None, 0.0)
    if a.third_person:
        tp, off_tp = capture_playback(exe, zone, demo_file.stem, 1, dur, run_dir, "tp")
    print(f"offsets fp={off_fp:.2f}s tp={off_tp:.2f}s")
    (run_dir / "capture.json").write_text(json.dumps(
        {"fp": str(fp), "off_fp": off_fp, "tp": str(tp) if tp else None, "off_tp": off_tp, "dur": dur}))
    compose(run_dir, fp, off_fp, tp, off_tp, dur, a.moves)


def compose(run_dir: Path, fp: Path, off_fp: float, tp: Path | None, off_tp: float, dur: float, moves_path: Path | None):
    def esc(s: str) -> str:
        return s.replace("\\", "\\\\").replace(":", "\\:").replace("'", "\\'")

    font = "C\\:/Windows/Fonts/segoeuib.ttf"
    moves = json.loads(moves_path.read_text()) if moves_path and moves_path.exists() else []
    seg = dur - 1.0

    def segment(idx: int, offset: float, title: str, out_label: str) -> list[str]:
        chain = [
            f"[{idx}:v]trim=start={offset:.3f}:duration={seg:.3f},setpts=PTS-STARTPTS,"
            f"drawtext=fontfile='{font}':text='{title}':x=36:y=30:fontsize=40:fontcolor=white:"
            f"box=1:boxcolor=black@0.45:boxborderw=10[{out_label}0]"
        ]
        last = f"[{out_label}0]"
        for i, m in enumerate(moves):
            t0 = float(m["t"])
            t1 = float(moves[i + 1]["t"]) if i + 1 < len(moves) else t0 + 3.0
            nxt = f"[{out_label}{i + 1}]"
            fade = 0.15
            alpha = (
                f"if(lt(t,{t0 + fade:.2f}),(t-{t0:.2f})/{fade},if(gt(t,{t1 - fade:.2f}),({t1:.2f}-t)/{fade},1))"
            )
            chain.append(
                f"{last}drawtext=fontfile='{font}':text='{esc(m['label'])}':x=(w-text_w)/2:y=h-110:fontsize=56:"
                f"fontcolor=white:borderw=4:bordercolor=black@0.8:alpha='{alpha}':"
                f"enable='gte(t,{t0:.2f})*lt(t,{t1:.2f})'{nxt}"
            )
            last = nxt
        chain.append(f"{last}null[{out_label}]")
        return chain

    if tp:
        filters = [
            *segment(0, off_fp, "FIRST PERSON", "a"),
            *segment(1, off_tp, "THIRD PERSON", "b"),
            "[a][b]concat=n=2:v=1:a=0[v]",
        ]
        inputs = ["-i", str(fp), "-i", str(tp)]
    else:
        filters = [*segment(0, off_fp, "FIRST PERSON", "a"), "[a]null[v]"]
        inputs = ["-i", str(fp)]
    out = run_dir / "showcase.mp4"
    subprocess.run(
        [str(FFMPEG), "-y", "-loglevel", "error", *inputs, "-filter_complex", ";".join(filters),
         "-map", "[v]", "-c:v", "h264_nvenc", "-preset", "p6", "-cq", "20", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(out)],
        check=True,
    )
    print(f"video {out}")


if __name__ == "__main__":
    sys.exit(main())
