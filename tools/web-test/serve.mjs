// A small static server for the browser checks (ADR-0049): the engine under /engine/
// (wasm-bindgen's output with re_web's web/ files beside it) and a project under
// /project/, with the files.json manifest the boot script reads. `pages` maps extra
// paths to HTML.
import { createServer } from 'node:http';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { extname, join, relative, resolve, sep } from 'node:path';

export const webDir = new URL('../../crates/re_web/web/', import.meta.url).pathname;

/// Project-relative paths of every file under `projectDir`, except those starting
/// with "." (such as .real/).
export function projectFiles(projectDir) {
  const files = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      const rel = relative(projectDir, path).split(sep).join('/');
      if (rel.startsWith('.')) continue;
      if (entry.isDirectory()) walk(path);
      else files.push(rel);
    }
  };
  walk(projectDir);
  return files;
}

const TYPES = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json', '.html': 'text/html' };

/// Starts the server on a free port of 127.0.0.1 and resolves to { origin, close }.
export async function serve({ engineDir, projectDir, pages = {} }) {
  const files = JSON.stringify(projectFiles(projectDir));
  const project = resolve(projectDir);
  const server = createServer((req, res) => {
    const url = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    let body = null;
    let type = TYPES[extname(url)] || 'application/octet-stream';
    if (url in pages) {
      body = pages[url];
      type = 'text/html';
    } else if (url === '/project/files.json' || url === '/engine/project/files.json') {
      body = files;
    } else {
      let path = null;
      if (url.startsWith('/engine/project/') || url.startsWith('/project/')) {
        path = resolve(project, url.replace(/^\/(engine\/)?project\//, ''));
        if (!path.startsWith(project + sep)) path = null;
      } else if (url.startsWith('/engine/')) {
        const name = url.slice('/engine/'.length) || 'index.html';
        path = existsSync(join(engineDir, name)) ? join(engineDir, name) : join(webDir, name);
        if (name.endsWith('.html')) type = 'text/html';
      }
      if (path && existsSync(path) && statSync(path).isFile()) body = readFileSync(path);
    }
    if (body === null) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-type': type });
    res.end(body);
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}
