// Shared helpers of the library build scripts (library/<slug>/build.mjs): pinned downloads, release assets with their
// zip contents, and the three outputs of one recipe. Recipe shape: docs/PLATFORM-SPEC.md section 4 ("Upstream
// fusions", "Upstream fetch"); the fusion reference is orchestrator/scripts/package-fusion.mjs (imported, not edited).
//
// Outputs of `node library/<slug>/build.mjs [--fixture]`:
//   library/<slug>/mashup.json                      planned URLs (SIGFAI/<slug> release assets, or upstream release files)
//   orchestrator/test/out/library/<slug>/           file:// variant + every asset, for `cargo run --example install`
//   app/src-tauri/tests/fixtures/<slug>/ (--fixture) file:// variant with an offline pack, only when the assets are
//                                                     under 2 MB in total (FIXTURE_MAX)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { sha256, unzip, zip } from '../orchestrator/src/recipe.js';
import { FIXTURE_MAX } from '../orchestrator/scripts/package-fusion.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
export const ROOT = path.resolve(here, '..');
export const CACHE = path.join(ROOT, 'orchestrator', 'test', 'out', 'library-cache');
const UA = { 'User-Agent': 'SIGFAI/mod-orchestrator (sigf.ai)' };

// BepInEx 5 (MIT; its bundled UnityDoorstop winhttp.dll is LGPL-2.1), the official x64 build, shipped unchanged as a release asset of the mashup's SIGFAI repo.
export const BEPINEX = {
  id: 'bepinex', version: '5.4.23.5', file: 'BepInEx_win_x64_5.4.23.5.zip',
  url: 'https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.5/BepInEx_win_x64_5.4.23.5.zip',
  sha256: '82f9878551030f54657792c0740d9d51a09500eeae1fba21106b0c441e6732c4', // GitHub release digest, checked 2026-10-05
  repo: 'https://github.com/BepInEx/BepInEx', commit: '57f1fb859bd4d0264cd2a59074d0e96c6a492a33', license: 'MIT AND LGPL-2.1',
};

/** A pinned file: from the cache when its sha256 matches, else downloaded and checked. */
export async function pinned(url, expect, name = url.split('/').pop()) {
  const file = path.join(CACHE, name);
  if (fs.existsSync(file) && sha256(fs.readFileSync(file)) === expect) return fs.readFileSync(file);
  const res = await fetch(url, { headers: UA, redirect: 'follow' });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  const data = Buffer.from(await res.arrayBuffer());
  const got = sha256(data);
  if (got !== expect) throw new Error(`${name}: sha256 ${got}, pinned ${expect}`);
  fs.mkdirSync(CACHE, { recursive: true });
  fs.writeFileSync(file, data);
  return data;
}

/** A file of an upstream repo at a pinned commit (LICENSE, notices). */
export async function rawAt(repo, commit, file) {
  const res = await fetch(`https://raw.githubusercontent.com/${repo.slice('https://github.com/'.length)}/${commit}/${file}`, { headers: UA });
  if (!res.ok) throw new Error(`${file} at ${commit}: HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

/** Every file entry of a zip with its sha256 (folders left out, as the app's engine does). */
export const contentsOf = (data) => unzip(data).filter(e => !e.name.endsWith('/')).map(e => ({ path: e.name.replace(/\\/g, '/'), sha256: sha256(e.data) }));

/** A release asset. `zipped`: it is a zip the app unpacks, so its contents are listed. upstream: the URL it is
 *  fetched from instead of the SIGFAI release (source.fetch "upstream"). */
export function asset(name, data, { zipped = false, upstream = null } = {}) {
  return { name, data, sha256: sha256(data), size: data.length, ...(zipped ? { contents: contentsOf(data) } : {}), ...(upstream ? { upstream } : {}) };
}

/** Our own zip of [{ name, data }], entries sorted. */
export function zipAsset(name, entries) {
  return asset(name, zip([...entries].sort((a, b) => a.name.localeCompare(b.name))), { zipped: true });
}

/** Replaces fields of a .mrpack's modrinth.index.json (name, summary), every other entry unchanged. */
export function renamePack(data, set) {
  return zip(unzip(data).map(e => e.name === 'modrinth.index.json'
    ? { name: e.name, data: Buffer.from(JSON.stringify({ ...JSON.parse(e.data.toString('utf8')), ...set }, null, 2)) }
    : e));
}

/** The download fields of an asset for a URL map. */
export const dl = (a, urls) => ({ url: urls[a.name], sha256: a.sha256, size: a.size });

/**
 * Writes the outputs of one recipe. make(urls, assets) returns the recipe for asset name -> URL. assets: the planned
 * assets; fixtureAssets: the same set built offline (no Modrinth downloads in a pack), or null for no fixture.
 */
export function emit({ slug, version, assets, fixtureAssets = null, make, argv = process.argv.slice(2) }) {
  const hosted = `https://github.com/SIGFAI/${slug}`;
  const planned = Object.fromEntries(assets.map(a => [a.name, a.upstream ?? `${hosted}/releases/download/v${version}/${a.name}`]));
  const json = (r) => JSON.stringify(r, null, 2) + '\n';
  const lib = path.join(ROOT, 'library', slug);
  fs.writeFileSync(path.join(lib, 'mashup.json'), json(make(planned, assets)));

  const local = (dir, set) => {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    const urls = {};
    for (const a of set) {
      fs.writeFileSync(path.join(dir, a.name), a.data);
      urls[a.name] = pathToFileURL(path.join(dir, a.name)).href;
    }
    const r = make(urls, set);
    r.built_at = '2026-10-05T00:00:00.000Z';
    fs.writeFileSync(path.join(dir, 'mashup.json'), json(r));
  };
  const out = path.join(ROOT, 'orchestrator', 'test', 'out', 'library', slug);
  local(out, assets);
  const total = assets.reduce((n, a) => n + a.size, 0);
  console.log(`${slug}: ${assets.map(a => `${a.name} (${a.size} B${a.upstream ? ', upstream' : ''})`).join(', ')}; ${total} B in total`);
  console.log(`  library/${slug}/mashup.json (planned URLs), ${path.relative(ROOT, out)}/ (file:// variant)`);
  if (argv.includes('--fixture') && fixtureAssets) {
    const size = fixtureAssets.reduce((n, a) => n + a.size, 0);
    if (size >= FIXTURE_MAX) console.log(`  no fixture: ${size} B is over ${FIXTURE_MAX} B`);
    else {
      local(path.join(ROOT, 'app', 'src-tauri', 'tests', 'fixtures', slug), fixtureAssets);
      console.log(`  app/src-tauri/tests/fixtures/${slug}/ (${size} B, offline pack)`);
    }
  }
}

/** Card fields every library recipe carries (the app ignores them today). */
export const card = (upstreamRepo) => ({ status: 'beta', issues: `${upstreamRepo}/issues` });
