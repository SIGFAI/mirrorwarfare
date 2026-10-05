# Mirrorwarfare

Mirror's Edge Catalyst parkour with Call of Duty: Modern Warfare 2 (2009) guns, in a
free-for-all with bots. Mirrorwarfare is a fork of [iw4L](https://github.com/vladtrc/iw4L),
an open-source MW2 runtime written in Rust with [Bevy](https://bevy.org/). It reads both
games from installations you already own: nothing from either game is in this repository.

**What's in it**

* **Catalyst movement** (`crates/movement_mec`): sprint momentum, wallrun and wallrun
  chains, wall jumps, wallclimb and ledge grabs, the wall turn (quickturn on the wall and
  kick off), vaults, springboards, slides, coil, landing rolls and hard-landing tiers. Speeds,
  heights and timings come from Faith's animation state graph decoded from Catalyst's own
  data; each tuning value records where it came from.
* **MW2 combat**: weapons, classes, killstreaks, the FFA game mode and bots from iw4L, with
  per-move weapon rules (hip fire during wallruns, weapon lowered while climbing, vaulting
  and rolling).
* **Faith's third-person animations**, decoded from Catalyst and retargeted onto the MW2
  body, so other players and bots run, wallrun, vault and roll like Catalyst.
* **The Parkour Playground** (`testbox`): an 80 × 80 m arena built for the move set, with
  four chained set-pieces, a 12 m tower, 14 spawns and collision that matches what you see.
  Arenas can also be cut from Catalyst's city with the extractor in `tools/mec`.
* **A deterministic autopilot and test harness**: routes are generated from the arena
  geometry, an in-engine autopilot drives them from game state, repeated runs are
  tick-identical, and the harness checks movement, load, performance, bots and stability.

Catalyst movement also runs on stock MW2 maps with `IW4L_MOVEMENT=mec`.

## Requirements

Windows, with:

* Call of Duty: Modern Warfare 2 (2009) multiplayer, installed through Steam.
* Mirror's Edge Catalyst, installed through Steam or the EA app. Only needed for Catalyst's
  third-person animations and for cutting city arenas; the Parkour Playground does not need it.
* [Rust](https://rustup.rs/) with the MSVC build tools, [uv](https://docs.astral.sh/uv/) and git.
* About 45 GB free next to the checkout if you extract Catalyst data.

## Quick start

```powershell
cargo build --profile play -p launcher         # ~15 min the first time, ~2 min after
uv run scripts/mec-testbox.py                  # writes the Parkour Playground to ..\mec-arenas\testbox
.\Mirrorwarfare.cmd -Arena testbox -Bots 0     # practise on your own
.\Mirrorwarfare.cmd -Arena testbox -Bots 7     # FFA with bots
```

`Mirrorwarfare.cmd` runs this checkout's own build (`target\play\iw4l.exe`, or `-Exe`),
finds MW2 through Steam and starts a match. `-NoSound`, `-Spawn` (skip class select),
`-Menu` and `-List` are also available. From the game's menu: Private Match → Game Setup
→ Change Map → **CATALYST**, with a **BOTS** row in Game Setup.

**Catalyst data (optional):** `powershell -ExecutionPolicy Bypass -File tools\mec\setup.ps1`
finds Catalyst, dumps its data next to the checkout, decodes Faith's animations into
`..\mec-anims` and cuts the city arenas. It is resumable; `-DryRun` shows the plan. Without
the animation pack the game falls back to stock MW2 animations.

## Controls

| Input | Action |
|---|---|
| W | Run; speed builds up over about 3 s (there is no sprint key) |
| Space | Up: jump, wallrun (into a wall at an angle), wallclimb (straight at a wall), ledge grab, vault, springboard |
| C | Down: slide while running, coil in the air, landing roll when held at touchdown |
| Q | 180° quickturn, also on a wall during a wallclimb to kick off backwards |
| Mouse | MW2 weapons: fire, aim (not during wallruns), reload R, melee V, grenades F / G |

Type `thirdperson` in the console (`~`) to watch the body animations.

## How it was built

* **Engine:** iw4L provides MW2 asset loading, rendering, netcode with server authority and
  client prediction, the game scripts (GSC) and bots. Mirrorwarfare adds arena packages
  (`crates/asset_mec`) grafted onto a stock map for its sky, sun and materials.
* **Catalyst data:** Frostbite superbundles are dumped and parsed (EBX, mesh sets,
  textures, Havok and world placement) by the Python tools in `tools/mec`. The ANT
  animation banks are decoded by `tools/mec/antbank.py` and `antanim.py`: the state graph,
  timing windows, root motion and clips.
* **Movement and animation:** `movement_mec` implements Faith's move set from the decoded
  data, runs inside the simulation step and travels in snapshots. `mec_anim` and
  `render_anim` retarget and play Catalyst clips on remote bodies.
* **Testing:** `tools/harness` (`run.py`, `showcase.py`, `determinism.py`) drives the game
  with the `autopilot` console command. See [MIRRORWARFARE.md](MIRRORWARFARE.md) for the
  movement details, data provenance and limitations, and [docs/RUN.md](docs/RUN.md) for
  console commands.

This project is written almost entirely by an LLM, like the iw4L it builds on.

## Known limitations

* Third person is not fully 1:1: quickturn and the long vault use stand-ins (their clips
  use a codec that is not decoded yet), there is no hand or foot IK, and the rifle swings
  with the hand during full-body moves.
* Catalyst's city arenas carry base colour, normal and specular maps with baked light only;
  Catalyst's own materials, glass and global illumination are approximated.
* Only free-for-all is tuned. Bots find parkour routes by trial rather than planning them.
* Online play between machines is untested; all peers must run the same build.

## Legal

Mirrorwarfare is an unofficial fan project. It is not affiliated with or endorsed by
Electronic Arts, DICE, Activision or Infinity Ward. Mirror's Edge, Call of Duty and Modern
Warfare are trademarks of their owners. You need your own copies of both games; this
repository contains no game code, assets or extracted data, and the tools read your
installations without modifying them. Catalyst data is read from installed files only; no
DRM is bypassed or modified.

## Credits and license

Built on [iw4L](https://github.com/vladtrc/iw4L), which credits
[OpenAssetTools](https://github.com/Laupetin/OpenAssetTools) and its [iw4x-x64
fork](https://github.com/iw4x-x64/oat), [IW4x](https://github.com/iw4x/iw4x-client),
[KisakCOD](https://github.com/SwagSoftware/KisakCOD) and
[Ghidra](https://github.com/NationalSecurityAgency/ghidra). The Catalyst tooling builds on
[Frostbite-Scripts](https://github.com/NicknineTheEagle/Frostbite-Scripts) (MIT),
[FrostyToolsuite](https://github.com/CadeEvs/FrostyToolsuite),
[AssetBankPlugin](https://github.com/marv7000/AssetBankPlugin) and
[IceBloc](https://github.com/marv7000/icebloc).

Licensed under [Apache 2.0](LICENSE) like iw4L, except where a file states otherwise:
`tools/mec/antanim.py` and `tools/mec/antbank.py` are GPL-3.0-only
([tools/mec/LICENSE-GPL-3.0](tools/mec/LICENSE-GPL-3.0)) because the animation decoder is
ported from AssetBankPlugin, and `tools/mec/fbebx.py` is MIT, derived from Frostbite-Scripts.
These are standalone Python tools; the game itself does not include them. Preserve required
attribution and bundled font license texts when redistributing; see [NOTICE](NOTICE).
