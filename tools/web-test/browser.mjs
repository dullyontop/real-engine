// Plays a project's replays with the browser runtime in headless Chromium with WebGPU
// (ADR-0049) and prints each run as JSON. Usage:
//   node browser.mjs [--expect FILE [--reported]] <engine dir> <project dir> <replay path>...
// It fails when a run reports an error or its last frame is blank. With --expect, each run's state_hash must also
// equal the one in FILE, the JSON lines `cargo run -p re_web --example replay` prints;
// --reported prints a different hash without failing (until Jolt and glam are
// deterministic across platforms, WP18 PR "d2").
// The engine dir holds wasm-bindgen's `--target web --out-name real` output for re_web.
// A small server serves it with re_web's boot script under /engine/ and the project
// under /project/ (with the files.json manifest the boot script reads); the page boots
// the engine like a game page, then runs each replay with `runReplayGpu`.
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright';
import { serve } from './serve.mjs';

const args = process.argv.slice(2);
let expectFile = null;
let reportedOnly = false;
while (args[0]?.startsWith('--')) {
  const flag = args.shift();
  if (flag === '--expect') expectFile = args.shift();
  else if (flag === '--reported') reportedOnly = true;
  else args.length = 0;
}
const [engineDir, projectDir, ...replays] = args;
if (!engineDir || !projectDir || replays.length === 0 || (reportedOnly && !expectFile)) {
  console.error('usage: node browser.mjs [--expect FILE [--reported]] <engine dir> <project dir> <replay path>...');
  process.exit(2);
}
const page = `<!doctype html><meta charset="utf-8"><title>web-test</title>
<script type="importmap">{"imports":{"wasi_snapshot_preview1":"/engine/wasi.js"}}</script>
<script>Error.stackTraceLimit = 60; console.trace = (m) => console.error(m + ' STACK ' + new Error().stack);</script>
<script type="module">
import { boot, missingFeatures } from '/engine/boot.js';
const replays = ${JSON.stringify(replays)};
try {
  const missing = missingFeatures();
  if (missing.length) throw new Error('missing: ' + missing.join('; '));
  const { session } = await boot({ project: '/project/' });
  console.log('booted');
  const runs = [];
  for (const replay of replays) {
    const t0 = performance.now();
    console.log('running ' + replay);
    const run = JSON.parse(await session.runReplayGpu(replay, 60));
    runs.push({ replay, ...run, ms: Math.round(performance.now() - t0) });
  }
  window.__result = { runs };
} catch (e) {
  window.__result = { error: String(e && e.stack || e) };
}
</script>`;

const server = await serve({ engineDir, projectDir, pages: { '/': page } });
const origin = server.origin;

const browser = await chromium.launch({ args: ['--enable-unsafe-webgpu'] });
const tab = await browser.newPage();
tab.on('console', (m) => console.error(`[${m.type()}] ${m.text()}`));
tab.on('pageerror', (e) => console.error(`[pageerror] ${e.message}`));
let result;
try {
  await tab.goto(origin + '/');
  await tab.waitForFunction(() => window.__result, null, { timeout: 600000, polling: 500 });
  result = await tab.evaluate(() => window.__result);
} finally {
  await browser.close();
  server.close();
}
if (result.error) {
  console.error(result.error);
  process.exit(1);
}
const expected = new Map();
if (expectFile) {
  for (const line of readFileSync(expectFile, 'utf8').split('\n').filter(Boolean)) {
    const run = JSON.parse(line);
    expected.set(run.replay, run.state_hash);
  }
}
let failed = false;
for (const run of result.runs) {
  console.log(JSON.stringify(run));
  if (run.errors.length) failed = true;
  if (!(run.last_frame?.distinct_colors >= 16)) {
    console.error(`${run.replay}: the last frame is blank (${JSON.stringify(run.last_frame)})`);
    failed = true;
  }
  if (expectFile && expected.get(run.replay) !== run.state_hash) {
    console.error(`${run.replay}: state_hash ${run.state_hash} in the browser, ${expected.get(run.replay)} natively${reportedOnly ? ' (reported only)' : ''}`);
    if (!reportedOnly) failed = true;
  }
}
process.exit(failed ? 1 : 0);
