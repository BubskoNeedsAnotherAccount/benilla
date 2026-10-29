// The main-thread half of the engine sound. The synth (a pipe-flow solver) renders in
// `synth-worker.js` into a ring buffer in a SharedArrayBuffer, and an AudioWorklet on the audio
// thread (`synth-worklet.js`) plays it. The game only writes the car's state here each frame
// (`src/web.rs`, `push_synth`), so a frame that stalls for a load cannot starve the sound: it
// holds the last state until the game catches up.

const RING = 1 << 14; // mono frames; a power of two
// Int32 header: 0 write count, 1 read count, 3 output rate, 4 pending shift pop, 5 underruns.
// Float32 state at byte 32: rpm, throttle, volume, speed. The ring starts at byte 128.

/** @param {boolean} enabled whether to start the worker at all */
export async function installSynth(enabled) {
  const silent = { params() {}, master() {} };
  globalThis.__benillaSynth = silent;
  if (!enabled) return;

  const cfg = await fetch("./assets/lambo_v10.json").then((r) => (r.ok ? r.json() : {})).catch(() => ({}));
  const sab = new SharedArrayBuffer(128 + RING * 4);
  const head = new Int32Array(sab, 0, 8);
  const state = new Float32Array(sab, 32, 8);

  // The synth renders at the context's rate, so the worker is told it before it starts.
  const ctx = new AudioContext({ latencyHint: "interactive" });
  Atomics.store(head, 3, ctx.sampleRate);

  const worker = new Worker(new URL("./synth-worker.js", import.meta.url), { type: "module" });
  try {
    await new Promise((resolve, reject) => {
      worker.onmessage = (e) => (e.data.type === "ready" ? resolve() : reject(new Error(e.data.message || "synth worker")));
      worker.onerror = (e) => reject(new Error(e.message || "synth worker failed"));
      worker.postMessage({ type: "init", sab, ring: RING, cfg });
    });
    await ctx.audioWorklet.addModule(new URL("./synth-worklet.js", import.meta.url));
  } catch (e) {
    console.warn("engine sound unavailable:", e);
    worker.terminate();
    ctx.close();
    return;
  }
  worker.onmessage = null;

  const node = new AudioWorkletNode(ctx, "benilla-engine", {
    numberOfInputs: 0,
    outputChannelCount: [2],
    processorOptions: { sab, ring: RING },
  });
  const master = ctx.createGain();
  node.connect(master).connect(ctx.destination);
  ctx.resume().catch(() => {});

  globalThis.__benillaSynth = {
    params(rpm, throttle, volume, speed, shifting) {
      state[0] = rpm;
      state[1] = throttle;
      state[2] = volume;
      state[3] = speed;
      if (shifting) Atomics.store(head, 4, 1);
    },
    // The settings' master volume, which the engine no longer gets through the game's mixer.
    master(volume) {
      master.gain.value = volume;
    },
  };
}
