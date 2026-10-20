// Runs the wasm build of this crate's checks under Node and compares them with the native
// run's output (ADR-0049). Usage:
//   node --experimental-wasm-exnref smoke.mjs [--reported PREFIX]... <wasm-bindgen out dir> <native report file>
// A line starting with a --reported prefix is printed when it differs but does not fail the
// run, as long as both sides end in a 16-digit hash (an error still fails); the pile hash is reported only until Jolt's deterministic build is on (WP18 PR "d2").
// The out dir holds wasm-bindgen's `--target web` output for web_test. Its glue imports
// the WASI functions from the bare module "wasi_snapshot_preview1", which a page maps to
// ./wasi.js with an import map; here the import is rewritten to this directory's wasi.js.
import { readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const reported = [];
while (args[0] === '--reported' && args.length > 1) {
  reported.push(args[1]);
  args.splice(0, 2);
}
const [outDir, nativeFile] = args;
if (!outDir || !nativeFile || args.length !== 2) {
  console.error('usage: node smoke.mjs [--reported PREFIX]... <wasm-bindgen out dir> <native report file>');
  process.exit(2);
}
const here = new URL('.', import.meta.url);
const glue = join(outDir, 'web_test.js');
const wasiUrl = new URL('wasi.js', here).href;
const patched = join(outDir, 'web_test.node.mjs');
writeFileSync(patched, readFileSync(glue, 'utf8').replaceAll('from "wasi_snapshot_preview1"', `from ${JSON.stringify(wasiUrl)}`));

const { default: init, init: initCtors, report } = await import(pathToFileURL(resolve(patched)).href);
const { setMemory } = await import(wasiUrl);
const wasm = await init({ module_or_path: readFileSync(join(outDir, 'web_test_bg.wasm')) });
setMemory(wasm.memory);
initCtors();

const t0 = performance.now();
const got = report();
const ms = performance.now() - t0;
const want = readFileSync(nativeFile, 'utf8');
process.stdout.write(got);
console.log(`(wasm report in ${ms.toFixed(0)} ms)`);
const a = got.split('\n'), b = want.split('\n');
let differ = 0;
for (let i = 0; i < Math.max(a.length, b.length); i++) {
  if (a[i] !== b[i]) {
    const hashed = (line, p) => line?.startsWith(p) && /: [0-9a-f]{16}$/.test(line);
    const onlyReported = reported.some((p) => hashed(a[i], p) && hashed(b[i], p));
    if (!onlyReported) differ++;
    console.error(`${onlyReported ? 'differs (reported only)' : 'differs'} from native:\n  wasm:   ${a[i]}\n  native: ${b[i]}`);
  }
}
process.exit(differ ? 1 : 0);
