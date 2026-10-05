// Helpers of the library recipes whose binaries SIGF builds from source (library/QC.md section 4): the outputs of a
// disposable AWS builder run, checked against the sha256 recorded in library/<slug>/source.json ("built"), and the
// GTA V passthrough layout shared by the rehan-remade/universal-modder descendants (um-gta5-passthrough,
// mc-in-gta-enhanced, witherstorm-gta5). library/lib.mjs is imported, not edited.
//
// Where the build outputs are read from: $SIGF_LIBRARY_BUILDS/<slug>/ (the files of
// s3://sigf-studio-954976316699-eu-west-1/work/library-builds/<slug>/<commit>/), default
// orchestrator/test/out/library-builds/<slug>/.
import fs from 'node:fs';
import path from 'node:path';
import { sha256, zip } from '../../orchestrator/src/recipe.js';
import { ROOT, asset, zipAsset } from '../lib.mjs';

/** source.json of a slug. */
export const sourceOf = (slug) => JSON.parse(fs.readFileSync(path.join(ROOT, 'library', slug, 'source.json'), 'utf8'));

/**
 * The SIGF-built files of a slug, as recorded in its source.json `built.artifacts` (name -> sha256): a Map
 * name -> Buffer. Throws when a file is missing or differs from the recorded hash.
 */
export function builtArtifacts(slug) {
  const { built } = sourceOf(slug);
  if (!built?.artifacts) throw new Error(`${slug}: source.json has no built.artifacts`);
  const dir = path.join(process.env.SIGF_LIBRARY_BUILDS ?? path.join(ROOT, 'orchestrator', 'test', 'out', 'library-builds'), slug);
  const out = new Map();
  for (const [name, expect] of Object.entries(built.artifacts)) {
    const file = path.join(dir, ...name.split('/'));
    if (!fs.existsSync(file)) throw new Error(`${slug}: ${file} missing (SIGF build of ${built.commit}; set SIGF_LIBRARY_BUILDS)`);
    const data = fs.readFileSync(file);
    if (sha256(data) !== expect) throw new Error(`${slug}: ${name} sha256 ${sha256(data)}, built ${expect}`);
    out.set(name, data);
  }
  return out;
}

/** The `source.built` field of a recipe: what SIGF built, from which commit. */
export const builtField = (slug) => {
  const { built } = sourceOf(slug);
  return { by: 'SIGF', commit: built.commit, builder: built.builder, artifacts: built.artifacts };
};

// ---------------------------------------------------------------------------------------------------------------
// GTA V passthrough (ScriptHookV ASI + ReShade add-on, linked to a Minecraft mod over 127.0.0.1:25599).

/** ReShade 6.8.0 with add-on support (BSD-3-Clause), ReShade64.dll unchanged from the official setup. */
export const RESHADE = {
  id: 'reshade', version: '6.8.0', license: 'BSD-3-Clause', repo: 'https://github.com/crosire/reshade',
  commit: '18deaa52de0c425a78b329e9cb3c497281cd00ec', // tag v6.8.0
  setup: { file: 'ReShade_Setup_6.8.0_Addon.exe', url: 'https://reshade.me/downloads/ReShade_Setup_6.8.0_Addon.exe', sha256: 'afe4c8f13048306307983b8b3d41d5bf00a86820440b0e57dea10950e1176445' },
  shaders: { repo: 'https://github.com/crosire/reshade-shaders', commit: 'fd0022170615ce0d8162d219bff07232fa6dd84f', license: 'CC0-1.0' }, // branch slim
  page: 'https://reshade.me/',
};
export const SCRIPTHOOKV = {
  id: 'scripthookv', page: 'https://www.dev-c.com/gtav/scripthookv/',
  license: 'Alexander Blade: free, not redistributable',
};

// ReShade.ini as upstream's gta/install.sh writes it: relative to the game folder, depth set up for GTA V.
export const RESHADE_INI = '[GENERAL]\r\nEffectSearchPaths=.\\reshade-shaders\\Shaders\\\r\nTextureSearchPaths=.\\reshade-shaders\\Textures\\\r\nPresetPath=.\\ReShadePreset.ini\r\n'
  + 'PreprocessorDefinitions=RESHADE_DEPTH_INPUT_IS_REVERSED=1,RESHADE_DEPTH_INPUT_IS_UPSIDE_DOWN=0,RESHADE_DEPTH_INPUT_IS_LOGARITHMIC=0,RESHADE_DEPTH_LINEARIZATION_FAR_PLANE=1000\r\n\r\n'
  + '[OVERLAY]\r\nTutorialProgress=4\r\nShowClock=0\r\nShowFPS=0\r\n\r\n[SCREENSHOT]\r\nSavePath=.\\\r\n';
export const RESHADE_PRESET = 'Techniques=MCPassthrough@MCPassthrough.fx\r\nTechniqueSorting=MCPassthrough@MCPassthrough.fx\r\n';

/** reshade-6.8.0-addon.zip, unpacked into {game}: ReShade64.dll as ReShade64.asi (loaded by ScriptHookV's ASI loader;
 *  a dxgi.dll proxy never loads in GTA V) and the two CC0 headers MCPassthrough.fx includes. */
export function reshadeAsset(files) {
  return zipAsset('reshade-6.8.0-addon.zip', [
    { name: 'ReShade64.asi', data: files.get('reshade/ReShade64.dll') },
    { name: 'reshade-shaders/Shaders/ReShade.fxh', data: files.get('reshade/ReShade.fxh') },
    { name: 'reshade-shaders/Shaders/ReShadeUI.fxh', data: files.get('reshade/ReShadeUI.fxh') },
    { name: 'reshade-shaders/LICENSE-ReShade.md', data: files.get('reshade/ReShade-LICENSE.md') },
  ]);
}

/** <id>-gta5.zip, unpacked into {game}: the SIGF-built MCPassthrough.asi, its effect, ReShade config, args.txt. */
export function gtaAsset(id, files, { args, preset = RESHADE_PRESET, extra = [] }) {
  return zipAsset(`${id}-gta5.zip`, [
    { name: 'MCPassthrough.asi', data: files.get('MCPassthrough.asi') },
    { name: 'reshade-shaders/Shaders/MCPassthrough.fx', data: files.get('MCPassthrough.fx') },
    { name: 'ReShade.ini', data: Buffer.from(RESHADE_INI) },
    { name: 'ReShadePreset.ini', data: Buffer.isBuffer(preset) ? preset : Buffer.from(preset) },
    { name: 'args.txt', data: Buffer.from(args) },
    ...extra,
  ]);
}

/** A Modrinth pack for Forge (recipe.js mrpack() writes Fabric ones): index + overrides, same zip writer. */
export function forgePack({ name, summary, versionId, mc, forge, downloads = [], overrides = [] }) {
  const index = {
    formatVersion: 1, game: 'minecraft', versionId, name: String(name).slice(0, 100), summary: String(summary).slice(0, 300),
    files: downloads, dependencies: { minecraft: mc, forge },
  };
  return zip([{ name: 'modrinth.index.json', data: Buffer.from(JSON.stringify(index, null, 2)) }, ...overrides]);
}

/** The requires[] of a GTA V passthrough: ScriptHookV linked (not redistributable), ReShade shipped. */
export function gtaRequires(reshade, urls, { shvNote }) {
  return [
    { id: SCRIPTHOOKV.id, page: SCRIPTHOOKV.page, license: SCRIPTHOOKV.license, note: shvNote },
    { id: RESHADE.id, version: `${RESHADE.version} (add-on)`, license: `${RESHADE.license}, shipped unchanged`, page: RESHADE.page,
      note: 'installed into the GTA V folder by the app as ReShade64.asi', source: { url: urls[reshade.name], sha256: reshade.sha256 } },
  ];
}

export const reshadeBundled = () => ({ name: 'ReShade', version: RESHADE.version, repo: RESHADE.repo, commit: RESHADE.commit, license: RESHADE.license,
  file: `ReShade64.dll from ${RESHADE.setup.file} (sha256 ${RESHADE.setup.sha256})`, shaders: RESHADE.shaders });

export { asset };
