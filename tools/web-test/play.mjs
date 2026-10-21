// Opens a project's game page (re_web's index.html) in headless Chromium with WebGPU,
// clicks to start, presses a key, and checks that the game runs in its canvas for a few
// seconds with no error and no page error (ADR-0049). Usage:
//   node play.mjs <engine dir> <project dir> [screenshot.png]
import { chromium } from 'playwright';
import { serve } from './serve.mjs';

const [engineDir, projectDir, shot] = process.argv.slice(2);
if (!engineDir || !projectDir) {
  console.error('usage: node play.mjs <engine dir> <project dir> [screenshot.png]');
  process.exit(2);
}
const server = await serve({ engineDir, projectDir });
const browser = await chromium.launch({ args: ['--enable-unsafe-webgpu', '--autoplay-policy=user-gesture-required'] });
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
try {
  await tab.goto(`${server.origin}/engine/index.html`);
  await tab.getByText('Click to start').waitFor({ timeout: 120000 });
  await tab.click('#overlay');
  await tab.waitForFunction(() => document.body.dataset.state, null, { timeout: 120000 });
  await tab.keyboard.press('KeyW');
  await tab.waitForTimeout(3000);
  state = await tab.evaluate(() => document.body.dataset.state);
  if (shot) await tab.screenshot({ path: shot });
} finally {
  await browser.close();
  server.close();
}
console.log(JSON.stringify({ state, problems }));
process.exit(state === 'running' && problems.length === 0 ? 0 : 1);
