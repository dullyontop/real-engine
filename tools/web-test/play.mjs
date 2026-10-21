// Opens the game page (index.html) of a folder `real build --target web` wrote in
// headless Chromium with WebGPU, clicks to start, presses a key, and checks that the
// game runs in its canvas for a few seconds with no error and no page error
// (ADR-0049). With --webgl, the browser has no WebGPU, so the game must fall back to
// WebGL2 (ADR-0049 §8). Usage:
//   node play.mjs [--webgl] <web build dir> [screenshot.png]
import { chromium } from 'playwright';
import { serve } from './serve.mjs';

const args = process.argv.slice(2);
const webgl = args[0] === '--webgl';
if (webgl) args.shift();
const [engineDir, shot] = args;
if (!engineDir) {
  console.error('usage: node play.mjs [--webgl] <web build dir> [screenshot.png]');
  process.exit(2);
}
const server = await serve({ engineDir });
const gpuArgs = webgl
  ? ['--disable-features=WebGPU', '--use-angle=swiftshader', '--enable-unsafe-swiftshader']
  : ['--enable-unsafe-webgpu'];
const browser = await chromium.launch({ args: [...gpuArgs, '--autoplay-policy=user-gesture-required'] });
const tab = await browser.newPage({ viewport: { width: 1280, height: 720 } });
const problems = [];
tab.on('console', (m) => {
  console.error(`[${m.type()}] ${m.text()}`);
  if (m.type() === 'error') problems.push(m.text());
});
tab.on('pageerror', (e) => {
  console.error(`[pageerror] ${e.message}`);
  problems.push(e.message);
});
let state;
let adapter;
try {
  await tab.goto(`${server.origin}/engine/index.html`);
  await tab.getByText('Click to start').waitFor({ timeout: 120000 });
  await tab.click('#overlay');
  await tab.waitForFunction(() => document.body.dataset.state, null, { timeout: 120000 });
  await tab.keyboard.press('KeyW');
  await tab.waitForTimeout(3000);
  state = await tab.evaluate(() => document.body.dataset.state);
  adapter = await tab.evaluate(() => document.body.dataset.adapter);
  if (shot) await tab.screenshot({ path: shot });
} finally {
  await browser.close();
  server.close();
}
console.log(JSON.stringify({ state, adapter, problems }));
const backendOk = !webgl || /Gl\b/.test(adapter ?? '');
process.exit(state === 'running' && problems.length === 0 && backendOk ? 0 : 1);
