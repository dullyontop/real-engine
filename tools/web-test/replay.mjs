// Plays a project's replay with the browser runtime (re_web) under Node and prints the
// run as JSON (ADR-0049). Usage:
//   node --experimental-wasm-exnref replay.mjs <wasm-bindgen out dir> <project dir> <replay path>...
// The out dir holds wasm-bindgen's `--target web --out-name real` output for re_web. Files whose
// project-relative path starts with "." (such as .real/) are not handed over.
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

const [outDir, project, ...replays] = process.argv.slice(2);
if (!outDir || !project || replays.length === 0) {
  console.error('usage: node replay.mjs <wasm-bindgen out dir> <project dir> <replay path>...');
  process.exit(2);
}
const here = new URL('.', import.meta.url);
const wasiUrl = new URL('../../crates/re_web/web/wasi.js', here).href;
const patched = join(outDir, 'real.node.mjs');
writeFileSync(patched, readFileSync(join(outDir, 'real.js'), 'utf8').replaceAll('from "wasi_snapshot_preview1"', `from ${JSON.stringify(wasiUrl)}`));

const glue = await import(pathToFileURL(resolve(patched)).href);
const { setMemory } = await import(wasiUrl);
const wasm = await glue.default({ module_or_path: readFileSync(join(outDir, 'real_bg.wasm')) });
setMemory(wasm.memory);
glue.init();

const session = new glue.Session();
const walk = (dir) => {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const rel = relative(project, path).split(sep).join('/');
    if (rel.startsWith('.')) continue;
    if (entry.isDirectory()) walk(path);
    else session.addFile(rel, readFileSync(path));
  }
};
walk(project);
for (const replay of replays) {
  const t0 = performance.now();
  const run = JSON.parse(session.runReplay(replay));
  console.log(JSON.stringify({ replay, ...run, ms: Math.round(performance.now() - t0) }));
}
