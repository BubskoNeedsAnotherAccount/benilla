// The main-thread half of the engine sound. The synth (a pipe-flow solver) renders in
// `synth-worker.js` into a ring buffer in a SharedArrayBuffer; the game's audio callback hands the
// worker the car's state and takes the samples that are ready (`src/web.rs`, `pull_synth`).
// Nothing here waits: a callback with too few samples gets silence for the rest.

const RING = 1 << 14; // mono frames; a power of two
// Int32 header: 0 write count, 1 read count, 3 output rate, 4 pending shift pop.
// Float32 state at byte 32: rpm, throttle, volume, speed. The ring starts at byte 128.

/** @param {boolean} enabled whether to start the worker at all */
export async function installSynth(enabled) {
  const silent = { params() {}, read: () => 0 };
  globalThis.__benillaSynth = silent;
  if (!enabled) return;

  const cfg = await fetch("./assets/lambo_v10.json").then((r) => (r.ok ? r.json() : {})).catch(() => ({}));
  const sab = new SharedArrayBuffer(128 + RING * 4);
  const head = new Int32Array(sab, 0, 8);
  const state = new Float32Array(sab, 32, 8);
  const ring = new Float32Array(sab, 128, RING);

  const worker = new Worker(new URL("./synth-worker.js", import.meta.url), { type: "module" });
  try {
    await new Promise((resolve, reject) => {
      worker.onmessage = (e) => (e.data.type === "ready" ? resolve() : reject(new Error(e.data.message || "synth worker")));
      worker.onerror = (e) => reject(new Error(e.message || "synth worker failed"));
      worker.postMessage({ type: "init", sab, ring: RING, cfg });
    });
  } catch (e) {
    console.warn("engine sound unavailable:", e);
    worker.terminate();
    return;
  }
  worker.onmessage = null;

  globalThis.__benillaSynth = {
    params(rpm, throttle, volume, speed, shifting, rate) {
      state[0] = rpm;
      state[1] = throttle;
      state[2] = volume;
      state[3] = speed;
      if (shifting) Atomics.store(head, 4, 1);
      Atomics.store(head, 3, rate);
    },
    read(out) {
      const r = Atomics.load(head, 1);
      const n = Math.min((Atomics.load(head, 0) - r) | 0, out.length);
      for (let i = 0; i < n; i++) out[i] = ring[(r + i) & (RING - 1)];
      Atomics.store(head, 1, (r + n) | 0);
      return n;
    },
  };
}
