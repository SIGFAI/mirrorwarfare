# Mirrorwarfare

Mirror's Edge Catalyst parkour with Modern Warfare 2 guns: wallrun, vault and slide through a free-for-all against bots on MW2's own assets.

**Mirrorwarfare is made by [r614](https://github.com/r614).** All credit for the mod goes to them. It is derived from [vladtrc/iw4L](https://github.com/vladtrc/iw4L), by vladtrc.

- Original project: https://github.com/r614/mirrorwarfare
- Report bugs and ask questions there: https://github.com/r614/mirrorwarfare/issues
- Upstream version packaged here: 0.1.0 (commit [`10d3697`](https://github.com/r614/mirrorwarfare/tree/10d36974ca0ae4131bb5e59b2421c49dc7ac6c4f))
- **Built by SIGF from commit [`10d36974ca0ae4131bb5e59b2421c49dc7ac6c4f`](https://github.com/r614/mirrorwarfare/tree/10d36974ca0ae4131bb5e59b2421c49dc7ac6c4f)**, on a disposable build machine (AWS EC2 i-0022be8314df37721 (c6i.4xlarge, Windows Server 2022, terminated after the build)). The app installs these SIGF builds, not binaries from the author.

> **Beta.** Nobody at SIGF has played this build yet. Back up your saves.
> Bugs in the mod itself go to the author's issue tracker above; problems with the one-click install go to this repository's issues.

## What you need

- **Call of Duty: Modern Warfare 2 (2009)** ([Steam](https://store.steampowered.com/app/10190/)): MW2 (2009) multiplayer, Steam.
- **Mirror's Edge Catalyst** ([Steam](https://store.steampowered.com/app/1233570/)) (movement re-made in Rust; Faith's animations and city arenas only when the player extracts them from their own install (optional)).
- Windows and the [SIGF app](https://sigf.ai). The app installs  for you.

## Install

In the SIGF app, open **Mirrorwarfare** in the catalog, press **Install**, then **Play**. **Restore** puts your game folders back exactly as they were.
The app follows `mashup.json` in this repository: every download is pinned by sha256. The files come from the release [`v0.1.0`](../../releases/tag/v0.1.0).

### Good to know

- You need Call of Duty: Modern Warfare 2 (2009) multiplayer installed through Steam. Mirror's Edge Catalyst is optional (only for Faith's animations and the city arenas). Windows only.
- The app adds iw4l.exe (Mirrorwarfare's own engine, built by SIGF from the upstream source), the Parkour Playground arena (mec-arenas\testbox) and the licenses to the MW2 folder; Restore removes them. iw4l.exe writes its logs and caches to iw4l-artifacts\ beside it: delete that folder after Restore if you want the MW2 folder exactly as before.
- To play, start iw4l.exe in the MW2 folder with: map mec:testbox --cmds "wait world; bot add 7" (free-for-all with 7 bots; "bot add 0" to practise alone, or "menu" for Private Match > Game Setup > Change Map > CATALYST). Do not start MW2 itself from Steam: that is the official game, not the mod.
- Controls: W runs (speed builds up over about 3 s), Space wallruns, wallclimbs, vaults and grabs ledges, C slides and rolls, Q turns 180 degrees, mouse fires MW2 weapons. Type thirdperson in the console (~) to watch the body.
- Optional Catalyst data: upstream's tools\mec\setup.ps1 (from the upstream repo, about 45 GB free space) extracts Faith's animations and city arenas from your own Catalyst install; without it the stock MW2 animations are used.
- Offline against bots, or private matches on iw4L's own servers; it never touches official MW2 servers. Only free-for-all is tuned; online play between machines is untested upstream.
- SIGF build of 10d3697 (no upstream release). Beta: report bugs to the author on the upstream issue tracker.

## What this repository holds

1. The upstream source tree at commit [`10d36974ca0ae4131bb5e59b2421c49dc7ac6c4f`](https://github.com/r614/mirrorwarfare/tree/10d36974ca0ae4131bb5e59b2421c49dc7ac6c4f), every file unchanged (same git blobs). Upstream's own `README.md` is there, unchanged; GitHub shows this file (`.github/README.md`) first.
2. Added by SIGF in the same commit: this file, and `sigf/` (the scripts that built the release assets, for reference: they run inside the SIGF repository).
3. `mashup.json`, the SIGF app recipe (the next commit).
4. The release `v0.1.0` (its tag is the first commit):

| Asset | Size | sha256 | What it is |
|---|---|---|---|
| `mirrorwarfare-mw2.zip` | 35644502 B | `af21678dc87a8c64231bf99d2591b916351e27ec5b33b365aeb9107167bad2ee` | the SIGF build of `iw4l.exe` from the pinned commit, the `mec-arenas/testbox` arena and routes from the tree, and upstream's LICENSE, NOTICE and THIRD-PARTY-LICENSES.txt under `mirrorwarfare-licenses/`. |

The sha256 of every file inside the zips is in `mashup.json` (`contents`).

## Licenses

| Part | License | Where |
|---|---|---|
| mirrorwarfare (all of the upstream tree, and the SIGF build) | Apache-2.0, with NOTICE (derived from vladtrc/iw4L, Apache-2.0) | `LICENSE`, `NOTICE`, `THIRD-PARTY-LICENSES.txt` |

## Why this repository exists

The SIGF app (https://sigf.ai) installs mods from recipes (`mashup.json`) whose downloads are pinned release files. This repository makes Mirrorwarfare installable in one click, credited to r614. If you are the author and want anything changed or taken down, open an issue here.
