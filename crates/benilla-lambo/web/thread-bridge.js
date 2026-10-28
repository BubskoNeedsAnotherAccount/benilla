// The main-thread half of the worker pool of the threaded wasm build (`build.mjs`). The engine asks
// for it at start (`src/web.rs`, `pool`): the module and its shared memory go to each worker, which
// runs the jobs the engine queues (`thread-worker.js`). A build without threads has a plain memory
// and gets no workers; the engine then does the work itself, a frame at a time.

/** Workers on this machine: leave a core each for the page and the I/O worker. */
function poolSize() {
  return Math.max(1, Math.min(3, (navigator.hardwareConcurrency || 4) - 2));
}

export function installThreads() {
  globalThis.__benillaThreads = {
    /** @returns {number} how many workers started */
    spawn(module, memory) {
      if (typeof SharedArrayBuffer === "undefined" || !(memory.buffer instanceof SharedArrayBuffer)) return 0;
      const n = poolSize();
      for (let i = 0; i < n; i++) {
        const worker = new Worker(new URL("./thread-worker.js", import.meta.url), { type: "module" });
        worker.onerror = (e) => console.error("pool worker failed:", e.message || e);
        worker.postMessage({ module, memory });
      }
      return n;
    },
  };
}
