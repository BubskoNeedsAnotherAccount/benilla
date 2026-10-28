// Build the static page into web/dist: `node web/build.mjs [--dev | --ship]`.
//
// Needs the wasm target (`rustup target add wasm32-unknown-unknown`) and a `wasm-bindgen` CLI
// whose version matches the lockfile's `wasm-bindgen` crate
// (`cargo install wasm-bindgen-cli --version <that> --locked`). `wasm-opt` (binaryen) is used when
// it is on PATH.

import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const dist = join(here, "dist");
const mode = process.argv.includes("--ship") ? "ship" : process.argv.includes("--dev") ? "dev" : "release";

function run(cmd, args, opts = {}) {
  console.log(`> ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd: repo, shell: process.platform === "win32", ...opts });
  if (r.status !== 0) {
    console.error(`${cmd} failed (${r.status ?? r.error})`);
    process.exit(1);
  }
}

// No `dev` feature: it points the install resolver at the source tree, which a browser lacks.
const profileArgs = mode === "dev" ? [] : ["--profile", mode];
run("cargo", ["build", "-p", "benilla-lambo", "--target", "wasm32-unknown-unknown", "--no-default-features", ...profileArgs]);
const wasm = join(repo, "target/wasm32-unknown-unknown", mode === "dev" ? "debug" : mode, "benilla-lambo.wasm");

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
// The name section is half the release wasm (137 to 71 MiB), and only a debugger reads it.
const strip = mode === "dev" ? [] : ["--remove-name-section", "--remove-producers-section"];
run("wasm-bindgen", ["--target", "web", "--no-typescript", ...strip, "--out-dir", dist, "--out-name", "benilla-lambo", wasm]);

const bg = join(dist, "benilla-lambo_bg.wasm");
if (mode !== "dev" && !names && spawnSync("wasm-opt", ["--version"], { shell: true }).status === 0) {
  run("wasm-opt", ["-O2", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--enable-simd", "-o", bg, bg]);
}

// The engine-sound worker's own wasm: the synth alone, a few hundred KiB.
run("cargo", ["build", "-p", "benilla-lambo-synth", "--target", "wasm32-unknown-unknown", ...profileArgs]);
const synthWasm = join(repo, "target/wasm32-unknown-unknown", mode === "dev" ? "debug" : mode, "benilla_lambo_synth.wasm");
run("wasm-bindgen", ["--target", "web", "--no-typescript", ...strip, "--out-dir", join(dist, "synth"), "--out-name", "benilla_lambo_synth", synthWasm]);
const synthBg = join(dist, "synth/benilla_lambo_synth_bg.wasm");
if (mode !== "dev" && !names && spawnSync("wasm-opt", ["--version"], { shell: true }).status === 0) {
  run("wasm-opt", ["-O3", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "-o", synthBg, synthBg]);
}

for (const f of ["index.html", "io-bridge.js", "io-worker.js", "synth-bridge.js", "synth-worker.js", "coi-sw.js", "_headers"]) {
  cpSync(join(here, f), join(dist, f));
}
const assets = join(repo, "crates/benilla-lambo/assets");
mkdirSync(join(dist, "assets/sounds"), { recursive: true });
cpSync(join(assets, "car.glb"), join(dist, "assets/car.glb"));
cpSync(join(assets, "lambo_v10.json"), join(dist, "assets/lambo_v10.json"));
for (const s of ["skid_loop.wav", "nitro.wav"]) cpSync(join(assets, "sounds", s), join(dist, "assets/sounds", s));

console.log(`\nbuilt ${dist} (${(statSync(bg).size / 2 ** 20).toFixed(1)} MiB wasm)`);
