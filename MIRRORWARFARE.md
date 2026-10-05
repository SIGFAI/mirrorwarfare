# Mirrorwarfare

Mirror's Edge Catalyst (2016) movement and maps with Modern Warfare 2 (2009) weapons,
free-for-all. Built as a fork of [iw4L](https://github.com/vladtrc/iw4L) (remote `upstream`),
which already provides MW2 weapons, rendering, netcode, the GSC runtime (`dm` is FFA) and bots.

Bring your own games: nothing from either game is committed. Reference checkouts and research
live under `context/` (gitignored, see `CONTEXT.md`).

## Play Mirrorwarfare

**Requirements** (Windows): MW2 (2009) multiplayer and Mirror's Edge Catalyst installed through
Steam (or EA app; pass `-CatalystDir`), Rust via rustup, [uv](https://docs.astral.sh/uv/), git,
~45 GB free next to the checkout.

**Build** (once, ~15 min; later builds ~2 min):

```powershell
cargo build --profile play -p launcher
```

**Set up the arenas** (once; resumable, safe to rerun):

```powershell
powershell -ExecutionPolicy Bypass -File tools\mec\setup.ps1
```

It finds Catalyst through Steam's `libraryfolders.vdf`, fetches Frostbite-Scripts at a pinned
commit into `..\mec-tools`, dumps the game into `..\mec-dump` (~25 min, ~40 GB, once), builds
the caches (~8 min, once) and cuts `mec_anchor_1`, `mec_west_1` and `mec_north_1` into
`..\mec-arenas` (~3–5 min each). `-DryRun` shows the plan, `-Arenas mec_anchor_1` builds one,
`-SkipDump` never starts the dumper. `..` is the folder holding this checkout (`-Root`).

**Play:**

```powershell
.\Mirrorwarfare.cmd                              # FFA, random arena, 7 bots
.\Mirrorwarfare.cmd -Arena mec_west_1 -Bots 11   # -Spawn skips class select, -NoSound, -List
.\Mirrorwarfare.cmd -Menu                        # main menu
```

The script runs this checkout's own build (`IW4L_EXE`, else `target\play`, then
`target\release`; `-Exe` overrides), i.e. `iw4l.exe map mec:<arena> --cmds "wait world; bot add 7"`; MW2 is found through
Steam as usual. On Linux: `make mirrorwarfare ARENA=mec_anchor_1 BOTS=7`. From the menu:
Private Match → Game Setup → Change Map → **CATALYST** lists the arenas by title; the **BOTS**
row cycles 0/1/3/5/7/11/15 (default 7, or `IW4L_BOTS`) and the bots join when the match loads.
The mode defaults to Free-for-all.

Arena folder precedence (game and script): `IW4L_MEC_ARENAS`, then
`iw4l-artifacts\mec-arenas` (or `IW4L_ARTIFACTS_DIR\mec-arenas`), then the first `mec-arenas`
beside `iw4l.exe` or any parent folder, then the same walk from the working directory. An
arena's menu name is `title` in its `arena.json`.

**Controls:** WASD; Space = up (jump, wallrun, wallclimb, ledge grab); C = down (slide, coil,
roll before landing); Q = 180° quickturn; Mouse1 fire, Mouse2 toggles aim, R reload, V melee, F frag,
G tactical, Mouse3 next weapon, Tab scores. MW2 classes and killstreaks; the class
menu opens on join. Sprint and prone do nothing on Catalyst arenas.

**Known limitations:** arenas carry base-colour textures with baked sky/sun light only (no
Catalyst materials or glass; sky and sun come from the donor map `mp_highrise`); the map preview panel is blank for arenas; first load of
an arena takes about a minute; bots navigate the arenas roughly (they get stuck on some roof
routes); team modes load but only FFA is tuned; the Catalyst movement is an approximation
(see Movement); `maps/mp/_matchdata` logs a harmless GSC error on every match.

## Catalyst constraints

* Frostbite 3: data lives in superbundles / cas-cat; content is typed EBX plus `res`/`chunk`
  payloads. Frosty Toolsuite has a first-class Catalyst profile; its SDK type info is
  generated from the running game.
* The Steam build ships with Denuvo and needs the EA app. This project does not bypass or
  modify DRM: research uses the installed data files, read-only observation of the running
  game, and video capture.
* The world is one streamed open city, not discrete levels. FFA arenas are carved out of it
  as bounded regions with authored spawns.
* Rendering is D3D11 SM5, so iw4L's D3D9 shader translator does not apply; ME materials get
  their own approximation from exported textures.

## New work

| Piece | Where it plugs in |
|---|---|
| Frostbite reader | new crates: toc/sb/cas-cat, EBX (typed by the Catalyst SDK), mesh-set and texture resources |
| Catalyst asset lane | `assets::lane` adapter → world draw surfaces, clipmap collision, arena + spawn sidecar |
| Collision | Havok physics data if decodable; otherwise simplified collision from render meshes |
| Catalyst movement | new `movement_mec` crate behind `movement_iw4::CollisionBackend`, selected per mode in `sim::step`; tuning values from EBX |
| Telemetry | read-only per-tick runner state + video → parity fixtures |
| Gun × parkour rules | per-move weapon gating (ADS, fire, lower) wired into the sprint/weapon interlock |

## Movement

`map mec:<arena>` moves live players (humans and bots) with `movement_mec`; stock maps keep
IW4 pmove. `IW4L_MOVEMENT=mec|iw4` forces one model on any map (read when the match is
installed). Dead, spectating, frozen, linked and remote-missile players always use IW4 pmove.

* State: `MecMoveState` lives in the client match state and travels in
  `ClientSnapshotMeta::mec` (and on the wire), so authority, prediction and replay run the
  same machine. `movement_mec::mec_pmove_iw4` is the one call from `sim::step`; it keeps the
  IW4 ADS / breath / view-height / footstep bookkeeping.
* Controls: jump (`+gostand`) = up (jump, wallrun, wallclimb, ledge), crouch (`+movedown`) =
  down (slide, coil, roll), `+quickturn` (default `Q`) = 180° turn. Sprint and prone do nothing.
* Weapons: wallrun allows hip fire only; climbs, vaults, rolls and hard landings lower the
  weapon through the sprint-lowered state. Falls hurt with `MOD_FALLING`.
* Numbers come from Faith's decoded ANT graph
  (`context/artifacts/2026-10-04-mec-ant/README.md`, provenance per field in `tuning.rs`):
  gravity 19.64 m/s², step 0.24 m, walk/crouch 2 m/s, sprint speed curve (2 → 4 m/s at
  0.25 s → 6.7 at 1 s → 7.2 at 3 s), jump table by entry speed (top-speed jump 8.04 m/s,
  1.2 m), wallrun 80 t scripted arc (+1.2 m at 32 t), wallclimb 2.6 m in 1 s, vault reach
  `0.2·h + 0.8 + 0.6·v/7.2` m, slide curve with abort at 4 m/s, quickturn move-out 32 t.
  Vault, ledge climb and roll follow the decoded root-motion curves (`rootmotion.rs`).
* Unlocks: FFA runs Faith's full move set (`MecTuning::DEFAULT` = `BASE` +
  `MecUnlocks::ALL`): ExtendedWallrun (2 wallruns, 2 wallclimbs, 3 wall moves per
  airtime), LongSlide (5 s), Coil, QuickTurn. Shift and FastSkillroll are flags only (not
  implemented). Wallruns attach to a wall the speed towards it closes within the 16 t align
  time; a wall jump goes where the camera looks when that is away from the wall.
* Springboard vs vault: a jump pressed up to 0.5 s (`Springboard.Timestamp` sighting
  window) before a 0.9–1.5 m obstacle is held and becomes the springboard at the take-off
  reach; a press during the vault branches into it in the vault's SpringboardBranch window
  (VaultOverFast 16–36 t, VaultOnto 20–30 t). No press: the auto-vault.
* Landing (drop from the jump start): > 2 m stumble 0.63 s (walk speed, no damage);
  4–6 m fail, control back at 2.03 s, 20–35 damage; 6–10 m fail, control at 2.95 s,
  35–75 damage; > 10 m death. Crouch held at touchdown with > 1 m drop rolls instead
  (1.0 s move-out, 4.4 m clip) and takes no damage below 10 m. FFA choice: the data has
  no damage numbers, so the fail tiers stay non-lethal from full health.
* Feel: first-person camera/viewmodel offsets per move (`movement_mec::camera_motion`,
  applied in `render_anim` view kick / fpv placement). Third person plays Faith's own
  Catalyst clips (wallrun L/R, wallclimb, vaults, ledge climb, roll, hard landings, slide,
  jump / fall / coil, landing, sprint) retargeted onto the MW2 body (`mec_anim` crate,
  `render_anim::anim::mec_body`): timed moves map their progress onto the whole clip,
  0.15 s crossfades, weapon-lowering moves drive the full body and the rest leave the arms
  on the MW2 weapon hold. The clips are decoded by setup step 5 (`tools/mec/antanim.py`)
  into `..\mec-anims` (`IW4L_MEC_ANIMS` overrides; `IW4L_MEC_ANIMS_OFF=1` disables); without
  them the stock MW2 clips (mantle / crouch / sprint, wallrun lean) are used.
* Ground: a body stays supported while walkable floor lies under its footprint (centre or a
  ring at 0.7 of the half width, 0.85 once grounded), so a rounded-hull contact on a roof
  edge or seam neither drops it nor flickers it between ground and air.
* Bots jump when they stall against geometry or the route steps up (the movement turns that
  into a vault, ledge climb or wallclimb) and roll out of long drops. On these maps the nav
  bake adds Climb edges (walls up to 150 in) and drops nodes on beams and rails; grounded
  bots brake before an edge their route does not drop from.

## Milestones

1. iw4L builds and runs `mp_boneyard` on Windows against an owned MW2 install. (Build: done.)
2. Catalyst EBX dumped and indexed; movement tuning assets and one district's meshes located.
3. One district arena loads: walk it with MW2 movement, shoot MW2 weapons.
4. `movement_mec` reaches parity with recorded fixtures on that arena.
5. FFA on the arena: spawns, weapon gating, 2–8 players on LAN.

## References (`context/externals`)

FrostyToolsuite, Frost4, Frostbite-Scripts (Python, run via `uv`), Runnervision (s&box
Mirror's Edge movement recreation — feel reference only), grid-leak blaze/gateway and
pamplona-future (Catalyst online services, network-level), plus iw4L's own references.

## Risks

Collision/Havok extraction and native movement logic behind Denuvo are the hard parts; the
open world needs careful arena cuts; upstream iw4L changes APIs often, so ME code stays in
separate crates.
