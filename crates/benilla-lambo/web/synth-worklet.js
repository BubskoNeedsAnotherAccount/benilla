// The audio-thread half of the engine sound: it copies the samples `synth-worker.js` has rendered
// out of the shared ring (layout in `synth-bridge.js`). It runs on the browser's audio thread, so
// the game freezing for a load or a long frame cannot starve it; a callback with too few samples
// plays silence for the rest and is counted in header slot 5.

class EngineOut extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const { sab, ring } = options.processorOptions;
    this.head = new Int32Array(sab, 0, 8);
    this.ring = new Float32Array(sab, 128, ring);
    this.mask = ring - 1;
    this.filled = false; // the first full callback: the ring is empty before the worker starts
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const left = out[0];
    const head = this.head;
    const r = Atomics.load(head, 1);
    const n = Math.min((Atomics.load(head, 0) - r) | 0, left.length);
    for (let i = 0; i < n; i++) left[i] = this.ring[(r + i) & this.mask];
    Atomics.store(head, 1, (r + n) | 0);
    if (n < left.length) {
      left.fill(0, n);
      if (this.filled) Atomics.add(head, 5, 1);
    } else {
      this.filled = true;
    }
    for (let c = 1; c < out.length; c++) out[c].set(left);
    return true;
  }
}

registerProcessor("benilla-engine", EngineOut);
