// Renders the engine sound off the game's thread. It keeps the ring buffer shared with
// `synth-bridge.js` about 3000 frames (60 ms) ahead of the audio callback, in short blocks so the
// car's state is at most a block plus the ring old when it is heard.

import init, { WebSynth } from "./synth/benilla_lambo_synth.js";

const BLOCK = 256;
const TARGET = 3072;

// The solver's cell size is the cost knob, so the worker adapts the tier to the machine. Tiers are
// ordered finest first; `quality` in the config is the finest it will ever use, and it starts no
// finer than `balanced`, stepping finer once a tier has shown the room.
const LADDER = ["high", "balanced", "fast", "eco"];
const START = 1;
// Cost of tier i against tier i + 1, measured on the wasm build (v10, idle to full-throttle redline):
// the junction solves scale with the step count, about 1/dx, so finer is cheaper than 1/dx^2.
const COST = [1.55, 1.45, 1.3];

const WINDOW_MS = 2000;
const MIN_BLOCKS = 32; // a window with fewer blocks rendered (audio not pulling) says nothing
const WARMUP_WINDOWS = 2; // the wasm is still tiering up, and the engine settling, for the first seconds of sound
const LOAD_BAD = 0.85; // median block time over its real time: no headroom left
const STARVE_LOAD = 0.5; // a dry ring is the synth's fault only if it is also busy; else it was descheduled
const BAD_WINDOWS = 2; // consecutive, before stepping a tier coarser
const UPGRADE_HEADROOM = 0.6; // the finer tier's estimated peak load must stay under this
const GOOD_WINDOWS = 10; // consecutive, before trying a finer tier
const PROBATION_MS = 30000; // a finer tier that fails within this is not tried again

self.onmessage = async (e) => {
  if (e.data.type !== "init") return;
  try {
    const { sab, ring: RING, cfg } = e.data;
    await init();
    const head = new Int32Array(sab, 0, 8);
    const state = new Float32Array(sab, 32, 8);
    const ring = new Float32Array(sab, 128, RING);
    let rate = 48000;

    // `"adaptive": false` in the config pins the tier.
    const adaptive = cfg.adaptive !== false;
    const named = cfg.quality === "ultra" ? 0 : LADDER.indexOf(cfg.quality ?? "balanced");
    let finest = named < 0 ? START : named;
    let tier = adaptive ? Math.max(finest, START) : finest;
    const make = () => new WebSynth(cfg.engine_id ?? "v8", cfg.muffler ?? "sport", LADDER[tier], cfg.burble ?? 0.85, rate);
    let synth = make();
    self.postMessage({ type: "ready" });
    console.info(`engine sound: ${LADDER[tier]}${adaptive ? "" : " (pinned)"}`);

    const block = new Float32Array(BLOCK);
    const times = new Float32Array(Math.ceil((WINDOW_MS / 1000) * 48000 * 2 / BLOCK) + 16);
    const nap = new Int32Array(new SharedArrayBuffer(4));
    let n = 0, starved = 0, dry = false, windows = 0, bad = 0, good = 0, windowStart = performance.now();
    let upgradedAt = -Infinity, settleUntil = WARMUP_WINDOWS;
    // The highest window load each tier has shown: the engine at redline costs about twice idle,
    // so a tier's room is judged on its worst, not on the window the player happens to be idling in.
    const peak = new Float32Array(LADDER.length);

    const switchTier = (to, why) => {
      console.info(`engine sound: ${LADDER[tier]} -> ${LADDER[to]} (${why})`);
      synth.free();
      tier = to;
      synth = make();
      bad = 0;
      good = 0;
      settleUntil = windows + 1; // the new engine starts cold
    };

    for (;;) {
      const w = Atomics.load(head, 0);
      const fill = (w - Atomics.load(head, 1)) | 0;
      if (fill >= TARGET) {
        Atomics.wait(nap, 0, 0, 2);
        continue;
      }
      // Count dry spells, not blocks: one hitch leaves several blocks short while the ring refills.
      if (fill < BLOCK) {
        if (!dry) starved++;
        dry = true;
      } else {
        dry = false;
      }
      const wanted = Atomics.load(head, 3);
      if (wanted && wanted !== rate) {
        rate = wanted;
        synth.set_sample_rate(rate);
      }
      const t = performance.now();
      synth.render(state[0], state[1], state[2], state[3], Atomics.exchange(head, 4, 0) === 1, block);
      if (n < times.length) times[n++] = performance.now() - t;
      for (let i = 0; i < BLOCK; i++) ring[(w + i) & (RING - 1)] = block[i];
      Atomics.store(head, 0, (w + BLOCK) | 0);

      const now = performance.now();
      if (now - windowStart < WINDOW_MS) continue;
      windowStart = now;
      const blocks = n, starves = starved;
      n = 0;
      starved = 0;
      // A suspended context renders nothing, and is no part of the warm-up either.
      if (blocks < MIN_BLOCKS) continue;
      windows++;
      if (!adaptive || windows <= settleUntil) continue;

      // The median, not the mean: one descheduled block is not the machine being too slow.
      const sorted = times.subarray(0, blocks).sort();
      const load = sorted[blocks >> 1] / ((BLOCK / rate) * 1000);
      peak[tier] = Math.max(peak[tier], load);

      if (load > LOAD_BAD || (starves > 0 && load > STARVE_LOAD)) {
        good = 0;
        if (++bad >= BAD_WINDOWS && tier < LADDER.length - 1) {
          // A finer tier that did not hold is not offered again this session.
          if (now - upgradedAt < PROBATION_MS) finest = tier + 1;
          switchTier(tier + 1, `load ${load.toFixed(2)}, ${starves} starved`);
        }
      } else {
        bad = 0;
        const room = tier > finest && peak[tier] * COST[tier - 1] < UPGRADE_HEADROOM && starves === 0;
        good = room ? good + 1 : 0;
        if (good >= GOOD_WINDOWS) {
          upgradedAt = now;
          switchTier(tier - 1, `load ${load.toFixed(2)}, room to spare`);
        }
      }
    }
  } catch (err) {
    self.postMessage({ type: "error", message: String(err && err.message || err) });
  }
};
