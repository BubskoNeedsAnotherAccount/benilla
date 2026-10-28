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
