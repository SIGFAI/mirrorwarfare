// Mirrorwarfare (r614 and the iw4L contributors, Apache-2.0): Mirror's Edge Catalyst movement with Modern Warfare 2
// (2009) weapons, maps and bots, on iw4L's own Rust engine that loads MW2's assets from the player's Steam install.
// No upstream release: SIGF built iw4l.exe (`cargo build --locked --profile play -p launcher`) and generated the
// default "Parkour Playground" arena (`scripts/mec-testbox.py`, procedural, no game data) from the pinned commit on a
// disposable AWS builder (library/QC.md section 4; source.json "built"). Catalyst's own data (optional city arenas and
// Faith's animations) is extracted on the player's PC by upstream's tools and is not part of this recipe.
//   SIGF_LIBRARY_BUILDS=<dir> node library/mirrorwarfare/build.mjs      (outputs: library/lib.mjs)
import { card, dl, emit, zipAsset } from '../lib.mjs';
import { builtArtifacts, builtField, sourceOf } from '../um-gta5-passthrough/sigf-build.mjs';

const ID = 'mirrorwarfare', VERSION = '0.1.0', NAME = 'Mirrorwarfare';
const SRC = sourceOf(ID);
const UP = { repo: SRC.repo, commit: SRC.commit, authors: ['r614', 'vladtrc'] };
const TAGLINE = "Mirror's Edge Catalyst parkour with Modern Warfare 2 guns: wallrun, vault and slide through a free-for-all against bots on MW2's own assets.";
const LAUNCH_ARGS = ['map', 'mec:testbox', '--cmds', 'wait world; bot add 7']; // Mirrorwarfare.ps1 -Arena testbox -Bots 7

const files = builtArtifacts(ID);
// iw4l.exe is portable: next to MW2's zone/ folder it uses that install as its games root (asset_transport
// discover.rs: IW4L_GAMES unset -> the exe's folder), and finds mec-arenas/ beside itself (asset_mec).
const game = zipAsset(`${ID}-mw2.zip`, [
  { name: 'iw4l.exe', data: files.get('iw4l.exe') },
  ...[...files.keys()].filter(n => n.startsWith('mec-arenas/')).map(n => ({ name: n, data: files.get(n) })),
  { name: 'mirrorwarfare-licenses/LICENSE', data: files.get('LICENSE') },
  { name: 'mirrorwarfare-licenses/NOTICE', data: files.get('NOTICE') },
  { name: 'mirrorwarfare-licenses/THIRD-PARTY-LICENSES.txt', data: files.get('THIRD-PARTY-LICENSES.txt') },
]);
const assets = [game];

const make = (urls, set) => ({
  id: `sigf/${ID}`,
  version: VERSION,
  name: NAME,
  tagline: TAGLINE,
  kind: 'mashup',
  games: [
    { game: 'mw2', role: 'host', label: 'Call of Duty: Modern Warfare 2 (2009)', engine: 'iw4L (Rust, wgpu): its own engine reading MW2 multiplayer assets from the player\'s install', apps: { steam: '10190' }, runtime: 'MW2 (2009) multiplayer, Steam' },
    { game: 'mirrorsedge-catalyst', role: 'guest', label: "Mirror's Edge Catalyst", uses: "movement re-made in Rust; Faith's animations and city arenas only when the player extracts them from their own install (optional)", apps: { steam: '1233570' } },
  ],
  requires: [],
  install: [
    { game: 'mw2', strategy: 'game-dir-snapshot', files: [
      { src: game.name, dst: '{game}', unpack: true, contents: game.contents, ...dl(game, urls) },
    ] },
  ],
  // iw4l.exe in the MW2 folder, never the store's launch (that is the official iw4mp.exe). `exe` is not read by the app
  // yet (PLATFORM-SPEC section 4): until then the notes tell the player.
  launch: [{ game: 'mw2', exe: 'iw4l.exe', args: LAUNCH_ARGS }],
  files: set.map(a => ({ name: a.name, ...dl(a, urls) })),
  source: {
    repo: UP.repo, license: 'Apache-2.0', upstream_license: SRC.license, commit: UP.commit,
    hosted: `https://github.com/SIGFAI/${ID}`,
    derived_from: { repo: 'https://github.com/vladtrc/iw4L', license: 'Apache-2.0' },
    built: builtField(ID),
  },
  media: {},
  built_by: { author: UP.authors[0], authors: UP.authors, packaged_by: 'SIGF' },
  idea_by: UP.authors[0],
  built_at: '2026-10-05T00:00:00.000Z',
  ...card(UP.repo),
  notes: [
    "You need Call of Duty: Modern Warfare 2 (2009) multiplayer installed through Steam. Mirror's Edge Catalyst is optional (only for Faith's animations and the city arenas). Windows only.",
    'The app adds iw4l.exe (Mirrorwarfare\'s own engine, built by SIGF from the upstream source), the Parkour Playground arena (mec-arenas\\testbox) and the licenses to the MW2 folder; Restore removes them. iw4l.exe writes its logs and caches to iw4l-artifacts\\ beside it: delete that folder after Restore if you want the MW2 folder exactly as before.',
    'To play, start iw4l.exe in the MW2 folder with: map mec:testbox --cmds "wait world; bot add 7" (free-for-all with 7 bots; "bot add 0" to practise alone, or "menu" for Private Match > Game Setup > Change Map > CATALYST). Do not start MW2 itself from Steam: that is the official game, not the mod.',
    'Controls: W runs (speed builds up over about 3 s), Space wallruns, wallclimbs, vaults and grabs ledges, C slides and rolls, Q turns 180 degrees, mouse fires MW2 weapons. Type thirdperson in the console (~) to watch the body.',
    "Optional Catalyst data: upstream's tools\\mec\\setup.ps1 (from the upstream repo, about 45 GB free space) extracts Faith's animations and city arenas from your own Catalyst install; without it the stock MW2 animations are used.",
    'Offline against bots, or private matches on iw4L\'s own servers; it never touches official MW2 servers. Only free-for-all is tuned; online play between machines is untested upstream.',
    `SIGF build of ${UP.commit.slice(0, 7)} (no upstream release). Beta: report bugs to the author on the upstream issue tracker.`,
  ],
});

emit({ slug: ID, version: VERSION, assets, make });
