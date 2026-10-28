// A static server for the built page: `node web/serve.mjs [--port 8080] [--data <WoW Data dir>] [--plain]`.
// It sends the COOP/COEP headers the file bridge needs (a real host does the same, see
// `_headers`). With `--data`, the archives are also served at /__data/ with range support, so a
// run can be scripted at `/?devdata` without a folder picker. For development only.

import { createServer } from "node:http";
import { createReadStream, readdirSync, statSync } from "node:fs";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const root = resolve(opt("--root", join(fileURLToPath(import.meta.url), "../dist")));
const port = Number(opt("--port", 8080));
const dataDir = opt("--data") && resolve(opt("--data"));
const plain = args.includes("--plain");

const types = {
  ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".mjs": "text/javascript",
  ".wasm": "application/wasm", ".json": "application/json", ".glb": "model/gltf-binary",
  ".wav": "audio/wav", ".css": "text/css",
};

function send(req, res, file, status = 200) {
  let size;
  try {
    size = statSync(file).size;
  } catch {
    res.writeHead(404).end("not found");
    return;
  }
  const headers = {
    "Content-Type": types[extname(file).toLowerCase()] ?? "application/octet-stream",
    "Accept-Ranges": "bytes",
    // `--plain` sends no isolation headers, as a host that cannot, to exercise `coi-sw.js`.
    ...(plain ? {} : { "Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "require-corp" }),
    "Cache-Control": "no-store",
  };
  const range = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range ?? "");
  if (range) {
    const start = range[1] === "" ? Math.max(0, size - Number(range[2])) : Number(range[1]);
    const end = range[1] === "" || range[2] === "" ? size - 1 : Math.min(Number(range[2]), size - 1);
    if (start >= size || start > end) {
      res.writeHead(416, { ...headers, "Content-Range": `bytes */${size}` }).end();
      return;
    }
    res.writeHead(206, { ...headers, "Content-Range": `bytes ${start}-${end}/${size}`, "Content-Length": end - start + 1 });
    createReadStream(file, { start, end }).pipe(res);
    return;
  }
  res.writeHead(status, { ...headers, "Content-Length": size });
  createReadStream(file).pipe(res);
}

createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  const path = decodeURIComponent(url.pathname);
  if (dataDir && path === "/__data/manifest.json") {
    const list = readdirSync(dataDir)
      .filter((n) => n.toLowerCase().endsWith(".mpq"))
      .map((name) => ({ name, size: statSync(join(dataDir, name)).size }));
    res.writeHead(200, { "Content-Type": "application/json", "Cross-Origin-Resource-Policy": "same-origin" }).end(JSON.stringify(list));
    return;
  }
  if (dataDir && path.startsWith("/__data/")) {
    const file = join(dataDir, normalize(path.slice("/__data/".length)).replace(/^(\.\.[\\/])+/, ""));
    if (!file.startsWith(dataDir + sep)) return void res.writeHead(403).end();
    return send(req, res, file);
  }
  const rel = normalize(path === "/" ? "/index.html" : path);
  const file = join(root, rel);
  if (!file.startsWith(root)) return void res.writeHead(403).end();
  send(req, res, file);
}).listen(port, () => {
  console.log(`serving ${root} at http://localhost:${port}/${dataDir ? ` (data: ${dataDir}, open /?devdata)` : ""}`);
});
