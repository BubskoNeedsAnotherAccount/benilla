# benilla-lambo in the browser

A static page that runs `benilla-lambo` as WebAssembly. The player picks the `Data` folder of
their own WoW 1.12.1 install; the files are read in the tab and go nowhere. The page holds no
game data, so it can be hosted anywhere that serves static files.

## Build

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the wasm-bindgen in Cargo.lock> --locked
node crates/benilla-lambo/web/build.mjs          # release; --ship for fat LTO, --dev for a fast debug build
```

`web/dist/` is the whole site: `index.html`, the wasm and its JS glue, the I/O bridge, and the
car and sounds. `wasm-opt` (binaryen) is run over the wasm when it is on `PATH`. The release wasm
is about 71 MiB, 16 MiB gzipped and 11 MiB brotli'd: serve it compressed. Hosts that cap a file at
25 MiB (Cloudflare Pages) cannot take it; GitHub Pages, Netlify and Vercel can.

## Host

Any static host. The page needs `SharedArrayBuffer`, which needs cross-origin isolation:

- Netlify and Cloudflare Pages read `_headers` from the site root, already in `dist/`.
- GitHub Pages and the like cannot set headers: `coi-sw.js` sets them from a service worker and
  the page reloads once. This needs HTTPS (or `localhost`).

## Play with friends

GitHub Pages only serves files, so the page talks to a small relay (`server/server.mjs`, Node, one
dependency) that you host separately. It runs no game logic: each browser drives its own car and the
relay forwards car states to everyone else connected to it (up to 8). Everyone still picks their own
`Data` folder. "Drive with friends" is off by default and single player does not touch the network.

1. Host the relay anywhere that runs Node and gives you `wss://` (Fly, Render, Railway, a VPS, or
   `cloudflared tunnel --url http://localhost:8787` from your own machine):
   ```sh
   cd crates/benilla-lambo/server && npm install && ORIGINS=https://<you>.github.io node server.mjs
   ```
   `ORIGINS` limits which pages may connect; leave it unset to accept any. A `Dockerfile` is included.
2. On the page tick "Drive with friends" and type the relay's `wss://` address. The address is
   never stored, so the site needs no update when it changes.
3. Press "Copy invite link" and send it to friends: it opens the page with the server filled in.
   They choose their game files and join. Everyone on the same server drives in the same world.

An https page can only open `wss://` (plain `ws://` is allowed for `localhost`).

## Run locally

```sh
node crates/benilla-lambo/web/serve.mjs                       # serves dist/ with the headers
node crates/benilla-lambo/web/serve.mjs --data <WoW>/Data     # also serves the archives: open /?devdata
```

`--plain` serves without the isolation headers, to test the service-worker path. `/?devdata` skips the folder picker and reads the archives over HTTP range requests, for scripted
runs. `?trace` logs why WebGPU rejects a shader and keeps errors in `window.__log`.

## How it works

- The engine reads MPQs through `benilla_mpq::Source`. On the web, `benilla_formats::virtual_data`
  mounts the picked files as a virtual `Data` folder, which `wow_data()` then names and
  `Chain::open` mounts in the reference's order.
- The engine reads synchronously and a browser's main thread cannot wait on a file, so each read
  goes to a worker (`io-worker.js`, `FileReaderSync`) over a `SharedArrayBuffer` and the main thread
  polls for the answer (`io-bridge.js`, `src/web.rs`). Reads are cached in 128 KiB chunks.
- Rendering is WebGPU (BC textures and the engine's bindings are beyond WebGL2).

## Limits

- A WebGPU browser: Chrome or Edge 113+, Safari 26+, Firefox 141+.
- Folder picking uses `showDirectoryPicker` (Chromium); other browsers fall back to a folder
  `<input>`, which lists every file under the folder and so is slower to start.
- One thread: tile loading shares the frame with rendering, so a cold start hitches more than native.
