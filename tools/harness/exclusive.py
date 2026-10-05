# /// script
# requires-python = ">=3.13"
# ///
"""Run a command while holding the machine-wide game lock, so only one iw4l.exe runs at a time.

  uv run --python 3.13 tools/harness/exclusive.py -- <command> [args...]

Blocks until the lock is free, then runs the command and releases the lock when it exits.
The lock is <mirrorwarfare>/.game.lock (msvcrt byte lock, released automatically if the
holder dies). `--status` prints the current holder.
"""

import msvcrt
import os
import subprocess
import sys
import time
from pathlib import Path

LOCK = Path(__file__).resolve().parents[3] / ".game.lock"


def acquire(fh) -> None:
    waited = False
    while True:
        try:
            fh.seek(0)
            msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
            return
        except OSError:
            if not waited:
                print(f"exclusive: waiting for game lock ({holder()})", file=sys.stderr, flush=True)
                waited = True
            time.sleep(2)


def holder() -> str:
    info = LOCK.with_suffix(".owner")
    return info.read_text().strip() if info.exists() else "unknown holder"


def main() -> int:
    args = sys.argv[1:]
    if args[:1] == ["--status"]:
        print(holder())
        return 0
    if args[:1] == ["--"]:
        args = args[1:]
    if not args:
        print(__doc__)
        return 2
    LOCK.touch(exist_ok=True)
    with open(LOCK, "r+b") as fh:
        acquire(fh)
        LOCK.with_suffix(".owner").write_text(f"pid {os.getpid()} since {time.strftime('%H:%M:%S')}: {' '.join(args)[:200]}")
        try:
            return subprocess.call(args)
        finally:
            fh.seek(0)
            msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)


if __name__ == "__main__":
    sys.exit(main())
