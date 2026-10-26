// The boot script of a Real Engine web build (ADR-0049). The page imports it with an
// import map that maps "wasi_snapshot_preview1" to ./wasi.js:
//
//   import { boot, play } from './boot.js';
//   const { session } = await boot({ onProgress });          // fetches game.rpak
//   await play(session, { onReport });                       // after a click
//   await play(session, { connect: 'ws://host:port/' });     // or joins a server
//
// `boot` checks what the browser supports, loads the engine (real.js and real_bg.wasm
// next to this file), runs its static constructors once, and hands it the game: the
// archive `real build --target web` wrote (game.rpak), or with `project` a project's
// loose files, every one its manifest (files.json: a JSON array of project paths)
// names. `play` loads the game's saved data from IndexedDB and starts it in a canvas
// appended to the page; the engine stores each later change there through
// `realEngine.persist`. With `connect`, a Roblox-style game joins the server at that
// address over a WebSocket instead (ADR-0050), and the server keeps its data.
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
  // WebGL2 is the fallback when there is no WebGPU (ADR-0049 §8).
  if (!('gpu' in navigator) && !document.createElement('canvas').getContext('webgl2')) {
    missing.push('WebGPU or WebGL2: use a current Chrome or Edge, served over HTTPS or from localhost');
  }
  return missing;
}

async function fetchBytes(url, onBytes = () => {}) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status} ${response.statusText}`);
  // Content-Length is the encoded size when the host compresses; then the bar is a guess.
  const total = Number(response.headers.get('content-length')) || 0;
  if (!response.body) return new Uint8Array(await response.arrayBuffer());
  const chunks = [];
  let done = 0;
  for (const reader = response.body.getReader(); ;) {
    const { done: end, value } = await reader.read();
    if (end) break;
    chunks.push(value);
    done += value.length;
    onBytes(done, Math.max(total, done));
  }
  const bytes = new Uint8Array(done);
  let at = 0;
  for (const c of chunks) {
    bytes.set(c, at);
    at += c.length;
  }
  return bytes;
}

/// Loads the engine and the game: the archive at `archive` (relative to the page), or
/// with `project` the loose files of the project folder at that URL. `extra` adds
/// [path, url] files above them (the test runners add replays this way).
/// `onProgress(done, total)` is called as bytes or files arrive. Returns the engine
/// module and a `Session` holding the game.
export async function boot({ archive = 'game.rpak', project = null, extra = [], onProgress = () => {} } = {}) {
  // What the engine calls before play() sets them, such as its panic hook.
  globalThis.realEngine ??= {
    persist() {},
    report: (kind, message) => (kind === 'warning' ? console.warn : console.error)(message),
  };
  const base = new URL('.', import.meta.url);
  const wasm = await init({ module_or_path: new URL('real_bg.wasm', base) });
  setMemory(wasm.memory);
  engine.init();
  const session = new engine.Session();
  const encode = (path) => path.split('/').map(encodeURIComponent).join('/');
  if (project === null) {
    session.addArchive(await fetchBytes(new URL(archive, document.baseURI), onProgress));
  } else {
    const root = new URL(project, document.baseURI);
    const paths = JSON.parse(new TextDecoder().decode(await fetchBytes(new URL('files.json', root))));
    let done = 0;
    onProgress(done, paths.length);
    await Promise.all(paths.map(async (path) => {
      session.addFile(path, await fetchBytes(new URL(encode(path), root)));
      onProgress(++done, paths.length);
    }));
  }
  for (const [path, url] of extra) session.addFile(path, await fetchBytes(new URL(url, document.baseURI)));
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
/// by default the archive's [package] identifier, or the project's name);
/// `onReport(kind, message)` receives "running" with the GPU adapter once the first
/// frame is drawn, "warning" and "problem" with a diagnostic while it runs, and
/// "error" with a message when the game stops (a network client also stops when it
/// is kicked or loses its server). `connect`, a server's `ws://host:port/` (or
/// `wss://` behind a TLS proxy, from a page served over HTTPS), joins that server as
/// player `name` (by default the server picks one).
export async function play(session, { gameId = session.gameId() || 'game', onReport = () => {}, connect = null, name = null } = {}) {
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
  session.play(connect, name);
  // winit appends the canvas; give it the keyboard.
  setTimeout(() => document.querySelector('canvas')?.focus(), 0);
}
