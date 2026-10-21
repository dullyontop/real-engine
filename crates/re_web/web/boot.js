// The boot script of a Real Engine web build (ADR-0049). The page imports it with an
// import map that maps "wasi_snapshot_preview1" to ./wasi.js:
//
//   import { boot, play } from './boot.js';
//   const { session } = await boot({ project: 'project/', onProgress });
//   await play(session, { gameId: 'my-game', onReport });   // after a click
//
// `boot` checks what the browser supports, loads the engine (real.js and real_bg.wasm
// next to this file), runs its static constructors once, and hands it every file the
// project's manifest (files.json: a JSON array of project paths) names. `play` loads
// the game's saved data from IndexedDB and starts it in a canvas appended to the page;
// the engine stores each later change there through `realEngine.persist`.
import init, * as engine from './real.js';
import { setMemory } from './wasi.js';

// The smallest module with a `try_table` block: browsers without exnref exception
// handling, which Luau needs, refuse it.
const EXNREF_PROBE = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
  0x03, 0x02, 0x01, 0x00, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x1f, 0x40, 0x00, 0x0b, 0x0b,
]);

/// What this browser lacks, as messages for the page; empty when it can run the engine.
export function missingFeatures() {
  const missing = [];
  if (typeof WebAssembly !== 'object' || !WebAssembly.validate(EXNREF_PROBE)) {
    missing.push('WebAssembly exception handling (exnref): use a current Chrome or Edge');
  }
  if (!('gpu' in navigator)) {
    missing.push('WebGPU: use a current Chrome or Edge, served over HTTPS or from localhost');
  }
  return missing;
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status} ${response.statusText}`);
  return new Uint8Array(await response.arrayBuffer());
}

/// Loads the engine and the project. `onProgress(done, total)` is called as files
/// arrive. Returns the engine module and a `Session` holding the project.
export async function boot({ project = 'project/', onProgress = () => {} } = {}) {
  // What the engine calls before play() sets them, such as its panic hook.
  globalThis.realEngine ??= {
    persist() {},
    report: (kind, message) => (kind === 'warning' ? console.warn : console.error)(message),
  };
  const base = new URL('.', import.meta.url);
  const wasm = await init({ module_or_path: new URL('real_bg.wasm', base) });
  setMemory(wasm.memory);
  engine.init();
  const root = new URL(project, document.baseURI);
  const paths = JSON.parse(new TextDecoder().decode(await fetchBytes(new URL('files.json', root))));
  const session = new engine.Session();
  let done = 0;
  onProgress(done, paths.length);
  await Promise.all(paths.map(async (path) => {
    const url = new URL(path.split('/').map(encodeURIComponent).join('/'), root);
    session.addFile(path, await fetchBytes(url));
    onProgress(++done, paths.length);
  }));
  return { engine, session };
}

const DB_VERSION = 1;
const STORE = 'files';

/// Opens the game's IndexedDB database, or resolves to null when the browser refuses
/// (private windows, storage turned off): the game then keeps its saves in memory.
function openDb(gameId) {
  return new Promise((resolve) => {
    let request;
    try {
      request = indexedDB.open(`real-engine:${gameId}`, DB_VERSION);
    } catch (e) {
      console.warn('saved data stays in memory:', e);
      resolve(null);
      return;
    }
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => {
      console.warn('saved data stays in memory:', request.error);
      resolve(null);
    };
  });
}

function readAll(db) {
  return new Promise((resolve) => {
    const out = [];
    const cursor = db.transaction(STORE).objectStore(STORE).openCursor();
    cursor.onsuccess = () => {
      const c = cursor.result;
      if (!c) return resolve(out);
      out.push([c.key, new Uint8Array(c.value)]);
      c.continue();
    };
    cursor.onerror = () => resolve(out);
  });
}

/// Starts the game. `gameId` names its saved data (one database per game and site;
/// the project's name from real.toml by default);
/// `onReport(kind, message)` receives "running" with the GPU adapter once the first
/// frame is drawn, "warning" and "problem" with a diagnostic while it runs, and
/// "error" with a message when the game stops.
export async function play(session, { gameId = session.projectName(), onReport = () => {} } = {}) {
  const db = await openDb(gameId);
  if (db) for (const [key, bytes] of await readAll(db)) session.addSaved(key, bytes);
  globalThis.realEngine = {
    // Called from inside the engine, so it never throws: an exception would unwind
    // through Rust frames.
    persist(key, bytes) {
      if (!db) return;
      try {
        const tx = db.transaction(STORE, 'readwrite');
        tx.onerror = () => console.warn(`could not save ${key}:`, tx.error);
        if (bytes === undefined || bytes === null) tx.objectStore(STORE).delete(key);
        else tx.objectStore(STORE).put(bytes, key);
      } catch (e) {
        console.warn(`could not save ${key}:`, e);
      }
    },
    report: onReport,
  };
  session.play();
  // winit appends the canvas; give it the keyboard.
  setTimeout(() => document.querySelector('canvas')?.focus(), 0);
}
