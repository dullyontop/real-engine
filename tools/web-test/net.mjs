// Plays a Roblox-style project's replays in headless Chromium as a lockstep client of a
// native dedicated server, over a WebSocket (ADR-0050 §7 and its amendment for PR
// "web"), and prints each run as JSON. Usage:
//   node net.mjs [--expect FILE [--reported]] <real> <web build dir> <project dir> <replay path>...
// For each replay, <real> serves the build's game.rpak (`real run --package`) on a free
// port of 127.0.0.1 for the replay's frames, and the page, which boots the same
// game.rpak, joins it with `Session.netRun`. Each server and each client must run all
// the frames with no error (a lockstep client runs the frame it waited for even when the
// server stops right after it). With --expect, the server's state_hash must also equal
// the one in FILE, the JSON lines `cargo run -p re_web --example replay` prints;
// --reported prints a different hash without failing.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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
const [real, engineDir, projectDir, ...replays] = args;
if (!real || !engineDir || !projectDir || replays.length === 0 || (reportedOnly && !expectFile)) {
  console.error('usage: node net.mjs [--expect FILE [--reported]] <real> <web build dir> <project dir> <replay path>...');
  process.exit(2);
}
if (!existsSync(join(engineDir, 'game.rpak'))) {
  console.error(`${engineDir} has no game.rpak: build it with \`real build --target web\``);
  process.exit(2);
}
const encode = (path) => path.split('/').map(encodeURIComponent).join('/');
const bootOptions = { archive: '/engine/game.rpak', extra: replays.map((r) => [r, `/project/${encode(r)}`]) };
const page = `<!doctype html><meta charset="utf-8"><title>web-test net</title>
<script type="importmap">{"imports":{"wasi_snapshot_preview1":"/engine/wasi.js"}}</script>
<script type="module">
try {
  // Imported here so a module that fails to load lands in __ready.
  const { boot, missingFeatures } = await import('/engine/boot.js');
  const missing = missingFeatures();
  if (missing.length) throw new Error('missing: ' + missing.join('; '));
  const { session } = await boot(${JSON.stringify(bootOptions)});
  // A frame that waits for the server returns, and the page's event loop delivers
  // the server's messages before the next step. The player keeps the server's
  // default name (Player1), as in the in-process run.
  window.__netRun = async (replay, url) => {
    const run = session.netRun(replay, url, null);
    try {
      while (!run.step()) await new Promise((resolve) => setTimeout(resolve, 0));
      return JSON.parse(run.finish());
    } finally {
      run.free();
    }
  };
  window.__ready = true;
} catch (e) {
  window.__ready = String(e && e.stack || e);
}
</script>`;

/// Starts a server for `replay`: resolves `url` to its WebSocket address once it
/// serves, and `done` to its exit code, stdout and stderr once it exits.
function startServer(replay) {
  const frames = JSON.parse(readFileSync(join(projectDir, replay), 'utf8')).frames;
  const dataDir = mkdtempSync(join(tmpdir(), 'real-web-net-'));
  const child = spawn(real, [
    'run', '--package', engineDir, '--data-dir', dataDir,
    '--server', '--lockstep', '--bind', '127.0.0.1', '--port', '0',
    '--frames', String(frames), '--json',
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  let err = '';
  child.stdout.on('data', (d) => { out += d; });
  const url = new Promise((resolve, reject) => {
    child.stderr.on('data', (d) => {
      err += d;
      const m = /browsers join over a WebSocket at (ws:\/\/\S+)/.exec(err);
      if (m) resolve(m[1]);
    });
    child.on('error', reject);
    child.on('close', (code) => reject(new Error(`the server exited (${code}) before it served:\n${err}`)));
  });
  const done = new Promise((resolve) => child.on('close', (code) => {
    rmSync(dataDir, { recursive: true, force: true });
    resolve({ code, out, err });
  }));
  // Settled by `done`; nobody else may be waiting on `url` when it rejects.
  url.catch(() => {});
  return { frames, url, done, kill: () => child.kill() };
}

const within = (promise, ms, what) => Promise.race([
  promise,
  new Promise((_, reject) => setTimeout(() => reject(new Error(`${what} took over ${ms / 1000} s`)), ms)),
]);

const server = await serve({ engineDir, projectDir, pages: { '/': page } });
const browser = await chromium.launch({ args: ['--enable-unsafe-webgpu'] });
const tab = await browser.newPage();
tab.on('console', (m) => console.error(`[${m.type()}] ${m.text()}`));
const pageErrors = [];
tab.on('pageerror', (e) => {
  console.error(`[pageerror] ${e.message}`);
  pageErrors.push(e.message);
});
const runs = [];
let failure = null;
try {
  await tab.goto(server.origin + '/');
  await tab.waitForFunction(() => window.__ready, null, { timeout: 600000, polling: 200 });
  const ready = await tab.evaluate(() => window.__ready);
  if (ready !== true) throw new Error(ready);
  for (const replay of replays) {
    const s = startServer(replay);
    let client;
    try {
      const url = await within(s.url, 120000, 'starting the server');
      console.error(`running ${replay} against ${url}`);
      client = await within(tab.evaluate(([r, u]) => window.__netRun(r, u), [replay, url]), 600000, `the client's ${replay}`);
    } catch (e) {
      client = { error: String(e && e.message || e) };
      s.kill();
    }
    let ended;
    try {
      ended = await within(s.done, 60000, 'stopping the server');
    } catch (e) {
      s.kill();
      ended = await s.done;
    }
    let report = null;
    try {
      report = JSON.parse(ended.out);
    } catch {
      // No report: the server failed before it ran (its stderr says why).
    }
    runs.push({ replay, frames: s.frames, client, code: ended.code, report, stderr: ended.err });
  }
} catch (e) {
  failure = String(e && e.stack || e);
} finally {
  await browser.close();
  server.close();
}
if (failure) {
  console.error(failure);
  process.exit(1);
}
const expected = new Map();
if (expectFile) {
  for (const line of readFileSync(expectFile, 'utf8').split('\n').filter(Boolean)) {
    const run = JSON.parse(line);
    expected.set(run.replay, run.state_hash);
  }
}
let failed = pageErrors.length > 0;
for (const { replay, frames, client, code, report, stderr } of runs) {
  const serverErrors = (report?.diagnostics ?? []).filter((d) => d.severity === 'error').map((d) => `${d.code}: ${d.message}`);
  console.log(JSON.stringify({
    replay,
    transport: 'websocket',
    frames: report?.frames ?? null,
    server_hash: report?.state_hash ?? null,
    expected: expected.get(replay) ?? null,
    server_errors: serverErrors,
    client,
  }));
  if (!report) {
    console.error(`${replay}: the server printed no report (exit ${code}):\n${stderr}`);
    failed = true;
    continue;
  }
  if (report.frames !== frames || serverErrors.length) {
    console.error(`${replay}: the server ran ${report.frames} of ${frames} frames${serverErrors.length ? ': ' + serverErrors.join('; ') : ''}`);
    failed = true;
  }
  const clientErrors = client.error ? [client.error] : (client.errors ?? []);
  if (client.frames !== frames || clientErrors.length) {
    console.error(`${replay}: the client ran ${client.frames ?? 0} of ${frames} frames${clientErrors.length ? ': ' + clientErrors.join('; ') : ''}`);
    failed = true;
  }
  if (expectFile && expected.get(replay) !== report.state_hash) {
    console.error(`${replay}: state_hash ${report.state_hash} on the server of a browser client, ${expected.get(replay)} in process${reportedOnly ? ' (reported only)' : ''}`);
    if (!reportedOnly) failed = true;
  }
}
process.exit(failed ? 1 : 0);
