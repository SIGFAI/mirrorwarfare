"""Launch one iw4l.exe, capture everything it says, and parse the facts back out.

One instance at a time: `run_game` blocks until the process it started exits (or its own
timeout kills it). It never touches any other iw4l.exe.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

import psutil

import exclusive


def acquire_game_lock(what: str, echo=print):
    """Hold the machine-wide one-game lock (tools/harness/exclusive.py) for a launch."""
    exclusive.LOCK.touch(exist_ok=True)
    fh = open(exclusive.LOCK, "r+b")
    if not _try_lock(fh):
        echo(f"waiting for the game lock ({exclusive.holder()})")
        exclusive.acquire(fh)
    exclusive.LOCK.with_suffix(".owner").write_text(
        f"pid {os.getpid()} since {time.strftime('%H:%M:%S')}: {what[:200]}")
    return fh


def _try_lock(fh) -> bool:
    import msvcrt

    try:
        fh.seek(0)
        msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
        return True
    except OSError:
        return False


def release_game_lock(fh) -> None:
    import msvcrt

    try:
        fh.seek(0)
        msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
    except OSError:
        pass
    fh.close()

# Folder that holds the checkout plus mec-arenas/, harness-runs/ and side build dirs.
WORK_ROOT = Path(os.environ.get("MIRRORWARFARE_ROOT", Path(__file__).resolve().parents[3]))
TICK_S = 0.05  # sim::MATCH_TICK_MS = 50


def find_exe() -> Path:
    cands = [WORK_ROOT / "iw4L/target/play/iw4l.exe"]
    cands += sorted(WORK_ROOT.glob("iw4l-target-*/play/iw4l.exe"))
    cands = [c for c in cands if c.exists()]
    if not cands:
        raise SystemExit("no iw4l.exe under iw4L/target/play or iw4l-target-*/play; pass --exe")
    return max(cands, key=lambda p: p.stat().st_mtime)


def foreign_game_pids(ours: set[int] | None = None) -> list[int]:
    ours = ours or set()
    out = []
    for p in psutil.process_iter(["name", "pid"]):
        try:
            if (p.info["name"] or "").lower() == "iw4l.exe" and p.info["pid"] not in ours:
                out.append(p.info["pid"])
        except psutil.Error:
            pass
    return out


# ----------------------------------------------------------------------------- run


@dataclass
class GameRun:
    name: str
    zone: str
    cmds: str
    env: dict
    out_dir: Path
    exe: Path
    started_at: float = 0.0
    wall_s: float = 0.0
    returncode: int | None = None
    timed_out: bool = False
    stdout: list = field(default_factory=list)  # (t_s, line)
    peak_rss_mb: float = 0.0
    peak_wset_mb: float = 0.0
    rss_series: list = field(default_factory=list)  # (t_s, rss_mb)
    log_path: Path | None = None
    jsonl_path: Path | None = None
    bench_path: Path | None = None
    screenshots: list = field(default_factory=list)
    events: list = field(default_factory=list)  # expanded jsonl records {t, ch, msg}

    # ---- derived lookups -------------------------------------------------------------

    def first_stdout_s(self, prefix: str = "") -> float | None:
        for t, line in self.stdout:
            if line.startswith(prefix):
                return t
        return None

    def msgs(self, pattern: str | re.Pattern):
        rx = re.compile(pattern) if isinstance(pattern, str) else pattern
        for ev in self.events:
            m = rx.search(ev["msg"])
            if m:
                yield ev, m

    def marks(self) -> dict:
        out = {}
        rx = re.compile(r"benchmark-mark: pid=\d+ seq=\d+ ns=(\d+) label=(\S+)(.*)")
        for ev, m in self.msgs(rx):
            rest = m.group(3)
            d = {"ns": int(m.group(1)), "s": int(m.group(1)) / 1e9, "t_ms": ev["t"]}
            for key in ("rss_mib", "heap_mib", "tick", "kills", "deaths", "alive", "clients", "local_id"):
                mm = re.search(rf"\b{key}=(\d+)", rest)
                if mm:
                    d[key] = int(mm.group(1))
            out[m.group(2)] = d
        return out

    def rss_at(self, t_s: float | None) -> float | None:
        """psutil RSS sample nearest a process-relative time (harness clock ~= process clock)."""
        if t_s is None or not self.rss_series:
            return None
        t, v = min(self.rss_series, key=lambda x: abs(x[0] - t_s))
        return v if abs(t - t_s) < 2 else None

    def summary(self) -> dict:
        return {
            "name": self.name,
            "zone": self.zone,
            "cmds": self.cmds,
            "env": {k: v for k, v in self.env.items() if k.startswith("IW4L_")},
            "wall_s": round(self.wall_s, 2),
            "returncode": self.returncode,
            "timed_out": self.timed_out,
            "peak_rss_mb": round(self.peak_rss_mb, 1),
            "peak_wset_mb": round(self.peak_wset_mb, 1),
            "log": str(self.log_path) if self.log_path else None,
            "bench": str(self.bench_path) if self.bench_path else None,
            "screenshots": [str(s) for s in self.screenshots],
        }


def base_env(games: str, arenas: str) -> dict:
    env = dict(os.environ)
    for k in list(env):
        # Start every run from a known state: no stale toggles from the caller's shell.
        if k in ("IW4L_SOUND", "IW4L_MOVEMENT", "IW4L_BENCH", "IW4L_PERF", "IW4L_BOTS", "IW4L_EXE"):
            env.pop(k)
    env["IW4L_GAMES"] = games
    env["IW4L_MEC_ARENAS"] = arenas
    env["IW4L_GAMETYPE"] = "dm"
    env["IW4L_TRACE_PLAYER"] = "1"
    return env


def run_game(
    *,
    name: str,
    exe: Path,
    zone: str,
    cmds: str,
    out_dir: Path,
    env: dict,
    artifacts_dir: Path,
    timeout_s: float,
    echo=print,
) -> GameRun:
    out_dir.mkdir(parents=True, exist_ok=True)
    logs = out_dir / "logs"
    logs.mkdir(exist_ok=True)
    state = out_dir / "state" / name
    state.mkdir(parents=True, exist_ok=True)
    traces = out_dir / "traces" / name
    traces.mkdir(parents=True, exist_ok=True)
    artifacts_dir.mkdir(parents=True, exist_ok=True)

    env = dict(env)
    env["IW4L_ARTIFACTS_DIR"] = str(artifacts_dir)
    env["IW4L_TRACES_DIR"] = str(traces)
    # Private settings/profile/account per run: thirdperson and binds never leak into the
    # user's own settings, and every run starts from defaults.
    env["IW4L_SETTINGS_PATH"] = str(state / "settings.cfg")
    env["IW4L_PROFILE_PATH"] = str(state / "profile.cfg")
    env["IW4L_ACCOUNT_PATH"] = str(state / "account.dat")

    run = GameRun(name=name, zone=zone, cmds=cmds, env=env, out_dir=out_dir, exe=exe)
    (logs / f"{name}.cmds.txt").write_text(f"{exe} map {zone} --cmds \"{cmds}\"\n", encoding="utf-8")
    bench_dir = artifacts_dir / "bench"
    bench_before = set(bench_dir.glob("*.txt")) if bench_dir.exists() else set()

    lock = acquire_game_lock(f"harness {name}: {exe} map {zone}", echo)
    echo(f"[{name}] launch {zone} (timeout {timeout_s:.0f}s)")
    t0 = time.monotonic()
    run.started_at = time.time()
    proc = subprocess.Popen(
        [str(exe), "map", zone, "--cmds", cmds],
        cwd=str(exe.parent),
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        creationflags=getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0),
    )
    stdout_file = open(logs / f"{name}.stdout.txt", "w", encoding="utf-8")

    def reader():
        for raw in iter(proc.stdout.readline, b""):
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            t = time.monotonic() - t0
            run.stdout.append((t, line))
            stdout_file.write(f"{t:9.3f}  {line}\n")
            stdout_file.flush()

    th = threading.Thread(target=reader, daemon=True)
    th.start()

    try:
        ps = psutil.Process(proc.pid)
    except psutil.Error:
        ps = None
    last_echo = 0.0
    while proc.poll() is None:
        t = time.monotonic() - t0
        if ps is not None:
            try:
                procs = [ps] + ps.children(recursive=True)
                rss = sum(p.memory_info().rss for p in procs) / 2**20
                mi = ps.memory_info()
                wset = getattr(mi, "peak_wset", mi.rss) / 2**20
                run.peak_rss_mb = max(run.peak_rss_mb, rss)
                run.peak_wset_mb = max(run.peak_wset_mb, wset)
                run.rss_series.append((round(t, 2), round(rss, 1)))
            except psutil.Error:
                pass
        if t > timeout_s:
            run.timed_out = True
            echo(f"[{name}] TIMEOUT after {t:.0f}s - killing our own pid {proc.pid}")
            try:
                for c in psutil.Process(proc.pid).children(recursive=True):
                    c.kill()
            except psutil.Error:
                pass
            proc.kill()
            break
        if t - last_echo > 30:
            last_echo = t
            tail = run.stdout[-1][1][:100] if run.stdout else ""
            echo(f"[{name}] {t:5.0f}s rss={run.peak_rss_mb:.0f}MiB  {tail}")
        time.sleep(0.25)
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    release_game_lock(lock)
    try:
        proc.stdin.close()
    except OSError:
        pass
    th.join(timeout=10)
    stdout_file.close()
    run.wall_s = time.monotonic() - t0
    run.returncode = proc.returncode

    # ---- collect files ---------------------------------------------------------------
    for _, line in run.stdout:
        m = re.match(r"log: (\S+)", line)
        if m:
            p = Path(m.group(1))
            if not p.is_absolute():
                p = exe.parent / p
            if p.exists():
                run.log_path = logs / f"{name}.log"
                shutil.copyfile(p, run.log_path)
            break
    jsonls = sorted(traces.glob("*.jsonl"), key=lambda p: p.stat().st_mtime)
    if jsonls:
        run.jsonl_path = jsonls[-1]
        run.events = load_jsonl(run.jsonl_path)
    elif run.log_path:
        run.events = [{"t": None, "ch": "", "msg": l} for l in expand_log(run.log_path)]
    if bench_dir.exists():
        new = [p for p in bench_dir.glob("*.txt") if p not in bench_before]
        if new:
            src = max(new, key=lambda p: p.stat().st_mtime)
            run.bench_path = logs / f"{name}.bench.txt"
            shutil.copyfile(src, run.bench_path)
    shots = out_dir / "screenshots"
    for ev, m in run.msgs(r"screenshot: wrote (.+\.png)"):
        p = Path(m.group(1).strip())
        if not p.is_absolute():
            p = exe.parent / p
        if p.exists():
            shots.mkdir(exist_ok=True)
            dst = shots / p.name
            shutil.copyfile(p, dst)
            run.screenshots.append(dst)
    (logs / f"{name}.rss.json").write_text(json.dumps(run.rss_series), encoding="utf-8")
    echo(
        f"[{name}] exit={run.returncode} wall={run.wall_s:.1f}s peak_rss={run.peak_rss_mb:.0f}MiB "
        f"events={len(run.events)} shots={len(run.screenshots)}{' TIMEOUT' if run.timed_out else ''}"
    )
    return run


def load_run(out_dir: Path, summary: dict, exe: Path) -> GameRun:
    """Rebuild a finished GameRun from what run_game left on disk (for --reanalyse)."""
    name = summary["name"]
    logs = out_dir / "logs"
    run = GameRun(name=name, zone=summary["zone"], cmds=summary["cmds"], env=summary.get("env", {}),
                  out_dir=out_dir, exe=exe)
    run.wall_s, run.returncode, run.timed_out = summary["wall_s"], summary["returncode"], summary["timed_out"]
    run.peak_rss_mb, run.peak_wset_mb = summary["peak_rss_mb"], summary.get("peak_wset_mb", 0.0)
    so = logs / f"{name}.stdout.txt"
    if so.exists():
        for line in so.read_text(encoding="utf-8", errors="replace").splitlines():
            t, _, rest = line.strip(" ").partition("  ")
            try:
                run.stdout.append((float(t), rest))
            except ValueError:
                pass
    rs = logs / f"{name}.rss.json"
    if rs.exists():
        run.rss_series = [tuple(x) for x in json.loads(rs.read_text())]
    if (logs / f"{name}.log").exists():
        run.log_path = logs / f"{name}.log"
    jsonls = sorted((out_dir / "traces" / name).glob("*.jsonl"))
    if jsonls:
        run.jsonl_path = jsonls[-1]
        run.events = load_jsonl(run.jsonl_path)
    elif run.log_path:
        run.events = [{"t": None, "ch": "", "msg": l} for l in expand_log(run.log_path)]
    if (logs / f"{name}.bench.txt").exists():
        run.bench_path = logs / f"{name}.bench.txt"
    run.screenshots = [Path(p) for p in summary.get("screenshots", [])]
    return run


_REPEAT = re.compile(r"^\S+\s+\S+\s+↑ repeated (\d+)× — (.*)$")
_PREFIX = re.compile(r"^(\S+)\s+(\S+)\s+(.*)$")


def load_jsonl(path: Path) -> list:
    out = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = rec.get("msg", "")
            m = _REPEAT.match(msg)
            if m:
                # The banner follows the first copy: n counts that copy too.
                for _ in range(int(m.group(1)) - 1):
                    out.append({"t": rec.get("t"), "ch": rec.get("ch"), "msg": m.group(2), "repeat": True})
                continue
            out.append({"t": rec.get("t"), "ch": rec.get("ch"), "msg": msg})
    return out


def expand_log(path: Path) -> list:
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        m = _REPEAT.match(line)
        if m:
            out += [m.group(2)] * (int(m.group(1)) - 1)
            continue
        m = _PREFIX.match(line)
        out.append(m.group(3) if m else line)
    return out


# ------------------------------------------------------------------------ parsing

_TRACE = re.compile(
    r"trace: t=(\d+) c=(\d+) L=(\w+) o=(-?[\d.]+),(-?[\d.]+),(-?[\d.]+) v=(-?\d+),(-?\d+),(-?\d+) "
    r"hp=(-?\d+) ang=(-?[\d.]+),(-?[\d.]+) g=(-?\d+) m=(\S+) mom=([\d.]+) pmf=([0-9a-f]+) ws=(-?\d+) "
    r"ads=(-?[\d.]+) clip=(-?\d+) k=(-?\d+) d=(-?\d+) god=(\d)"
)
_TRACE_DEAD = re.compile(r"trace: t=(\d+) c=(\d+) L=(\w+) k=(-?\d+) d=(-?\d+)$")


def trace_rows(run: GameRun) -> list:
    rows = []
    for ev in run.events:
        msg = ev["msg"]
        if not msg.startswith("trace: "):
            continue
        m = _TRACE.match(msg)
        if m:
            g = m.groups()
            rows.append(
                {
                    "tick": int(g[0]), "c": int(g[1]), "life": g[2],
                    "o": (float(g[3]), float(g[4]), float(g[5])),
                    "v": (float(g[6]), float(g[7]), float(g[8])),
                    "hp": int(g[9]), "pitch": float(g[10]), "yaw": float(g[11]), "ground": int(g[12]),
                    "mode": g[13], "mom": float(g[14]), "pmf": int(g[15], 16), "ws": int(g[16]),
                    "ads": float(g[17]), "clip": int(g[18]), "k": int(g[19]), "d": int(g[20]),
                    "god": g[21] == "1", "t_ms": ev["t"],
                }
            )
            continue
        m = _TRACE_DEAD.match(msg)
        if m:
            rows.append({"tick": int(m.group(1)), "c": int(m.group(2)), "life": m.group(3), "o": None,
                         "k": int(m.group(4)), "d": int(m.group(5)), "t_ms": ev["t"]})
    return rows


_MEC = re.compile(
    r"mec client=(\d+) (\w+)(?: \{ ([^}]*) \})? mode=(\w+) at \[(-?\d+), (-?\d+), (-?\d+)\] "
    r"hspeed=(-?\d+) vz=(-?\d+) momentum=([\d.]+)"
)


def mec_events(run: GameRun) -> list:
    """Movement events with the latest trace tick seen before them (the log is ordered)."""
    out = []
    tick = None
    for ev in run.events:
        msg = ev["msg"]
        if msg.startswith("trace: t="):
            tick = int(msg[9 : msg.index(" ", 9)])
            continue
        m = _MEC.search(msg)
        if m:
            g = m.groups()
            extra = {}
            if g[2]:
                for kv in g[2].split(","):
                    if ":" in kv:
                        k, v = kv.split(":", 1)
                        try:
                            extra[k.strip()] = float(v)
                        except ValueError:
                            extra[k.strip()] = v.strip()
            out.append({
                "c": int(g[0]), "kind": g[1], "extra": extra, "mode": g[3],
                "o": (int(g[4]), int(g[5]), int(g[6])), "hspeed": int(g[7]), "vz": int(g[8]),
                "mom": float(g[9]), "tick": tick, "t_ms": ev["t"],
            })
    return out


def kill_lines(run: GameRun) -> list:
    """`gsc log: K;...` lines (MW2 PlayerKilled logPrint), with the latest trace tick."""
    out = []
    tick = None
    for ev in run.events:
        msg = ev["msg"]
        if msg.startswith("trace: t="):
            tick = int(msg[9 : msg.index(" ", 9)])
            continue
        if msg.startswith("gsc log: K;"):
            f = msg[len("gsc log: K;"):].split(";")
            if len(f) < 12:
                continue
            out.append({
                "victim": int(f[1]) if f[1].lstrip("-").isdigit() else None, "victim_name": f[3],
                "attacker": int(f[5]) if f[5].lstrip("-").isdigit() else None, "attacker_name": f[7],
                "weapon": f[8], "damage": int(f[9]) if f[9].lstrip("-").isdigit() else None,
                "mod": f[10], "hitloc": f[11], "tick": tick, "t_ms": ev["t"],
            })
    return out


def gsc_errors(run: GameRun) -> list:
    out = []
    for _, m in run.msgs(r'gsc: runtime_error pid=\d+ ns=\d+ at=(\S+) function=(\S+) fault="(.*)"'):
        out.append({"at": m.group(1), "function": m.group(2), "fault": m.group(3)})
    return out


def panics(run: GameRun) -> list:
    out = []
    for _, line in run.stdout:
        if "panicked at" in line or "RUST_BACKTRACE" in line:
            out.append(line[:300])
    for ev in run.events:
        if "panicked at" in ev["msg"]:
            out.append(ev["msg"][:300])
    return sorted(set(out))


def prediction_probe(run: GameRun) -> dict | None:
    last = None
    for _, m in run.msgs(r"PROBE prediction .*?\bdev=(\d+).*?\breplay=(\d+)"):
        last = {"dev": int(m.group(1)), "replay": int(m.group(2))}
    return last


def process_exit_code(run: GameRun) -> int | None:
    code = None
    for _, m in run.msgs(r"lifecycle: process_exit pid=\d+ ns=\d+ code=(-?\d+)"):
        code = int(m.group(1))
    return code


def movement_model(run: GameRun) -> str | None:
    for _, m in run.msgs(r"movement: (\w+)"):
        return m.group(1)
    return None


def parse_bench(path: Path | None) -> dict:
    if not path or not path.exists():
        return {}
    text = path.read_text(encoding="utf-8", errors="replace")
    out = {}
    m = re.search(r"command → playable: ([\d.]+)s", text)
    if m:
        out["command_to_playable_s"] = float(m.group(1))
    for key in ("load requested", "match installed", "world spawned", "loading screen down", "in game", "first drawn frame"):
        m = re.search(rf"^\s+{re.escape(key)}\s+([\d.]+)s", text, re.M)
        if m:
            out[key.replace(" ", "_") + "_s"] = float(m.group(1))
    m = re.search(r"(\d+) frames over ([\d.]+)s of gameplay, (\d+) fps average", text)
    if m:
        out["frames"], out["gameplay_s"], out["fps_avg"] = int(m.group(1)), float(m.group(2)), int(m.group(3))
    m = re.search(r"frame ms\s+avg ([\d.]+)\s+p50 ([\d.]+)\s+p95 ([\d.]+)\s+p99 ([\d.]+)\s+max ([\d.]+)\s+min ([\d.]+)", text)
    if m:
        for k, v in zip(("avg", "p50", "p95", "p99", "max", "min"), m.groups()):
            out[f"frame_ms_{k}"] = float(v)
    m = re.search(r"peak[^\n]*?(\d+)\s*MiB", text, re.I)
    if m:
        out["bench_peak_mib"] = int(m.group(1))
    return out
