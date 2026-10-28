// Renders the engine sound off the game's thread. It keeps the ring buffer shared with
// `synth-bridge.js` about 3000 frames (60 ms) ahead of the audio callback, in short blocks so the
// car's state is at most a block plus the ring old when it is heard.

import init, { WebSynth } from "./synth/benilla_lambo_synth.js";

const BLOCK = 256;
const TARGET = 3072;
const LOAD_MAX = 0.6;

self.onmessage = async (e) => {
  if (e.data.type !== "init") return;
  try {
    const { sab, ring: RING, cfg } = e.data;
    await init();
    const head = new Int32Array(sab, 0, 8);
    const state = new Float32Array(sab, 32, 8);
    const ring = new Float32Array(sab, 128, RING);
    let rate = 48000;
    // The solver's cell size is the cost knob. Start at the config's tier; if a window of blocks
    // takes over LOAD_MAX of real time, rebuild one tier coarser, so a slow machine still keeps up.
    const ladder = ["balanced", "fast", "eco"];
    let tier = Math.max(0, ladder.indexOf(cfg.quality ?? "balanced"));
    const make = () => new WebSynth(cfg.engine_id ?? "v8", cfg.muffler ?? "sport", ladder[tier], cfg.burble ?? 0.85, rate);
    let synth = make();
    self.postMessage({ type: "ready" });

    const block = new Float32Array(BLOCK);
    const nap = new Int32Array(new SharedArrayBuffer(4));
    let busy = 0, rendered = 0, reported = performance.now();
    for (;;) {
      const w = Atomics.load(head, 0);
      const fill = (w - Atomics.load(head, 1)) | 0;
      if (fill >= TARGET) {
        Atomics.wait(nap, 0, 0, 2);
        continue;
      }
      const wanted = Atomics.load(head, 3);
      if (wanted && wanted !== rate) {
        rate = wanted;
        synth.set_sample_rate(rate);
      }
      const t = performance.now();
      synth.render(state[0], state[1], state[2], state[3], Atomics.exchange(head, 4, 0) === 1, block);
      busy += performance.now() - t;
      rendered += BLOCK;
      for (let i = 0; i < BLOCK; i++) ring[(w + i) & (RING - 1)] = block[i];
      Atomics.store(head, 0, (w + BLOCK) | 0);

      const now = performance.now();
      if (now - reported > 2000) {
        const load = busy / ((rendered / rate) * 1000);
        if (load > LOAD_MAX && tier < ladder.length - 1) {
          synth.free();
          tier++;
          synth = make();
        }
        busy = 0;
        rendered = 0;
        reported = now;
      }
    }
  } catch (err) {
    self.postMessage({ type: "error", message: String(err && err.message || err) });
  }
};
