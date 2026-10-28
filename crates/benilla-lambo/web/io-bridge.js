// The main-thread half of the file bridge. The engine's wasm calls `globalThis.__benillaIo`
// (see `src/web.rs`) to read the picked `Data` files; each read is handed to the I/O worker and
// polled for, because the main thread may not block on `Atomics.wait`.

const CAP = 1 << 20;
const SPIN_LIMIT_MS = 60000;

/**
 * @param {{name: string, size: number, blob?: Blob, url?: string}[]} files
 * @param {Map<string, Uint8Array>} assets files fetched ahead of the start, by path
 */
export async function installIo(files, assets) {
  const sab = new SharedArrayBuffer(64 + CAP);
  const ctrl = new Int32Array(sab, 0, 4);
  const offset = new Float64Array(sab, 16, 1);
  const data = new Uint8Array(sab, 64, CAP);

  const worker = new Worker(new URL("./io-worker.js", import.meta.url));
  await new Promise((resolve, reject) => {
    worker.onmessage = (e) => (e.data.type === "ready" ? resolve() : reject(new Error("io worker")));
    worker.onerror = (e) => reject(new Error(e.message || "io worker failed"));
    worker.postMessage({
      type: "init",
      sab,
      files: files.map((f) => ({ blob: f.blob, url: f.url })),
    });
  });

  globalThis.__benillaIo = {
    count: () => files.length,
    name: (i) => files[i].name,
    size: (i) => files[i].size,
    asset: (path) => assets.get(path),
    read(i, at, out) {
      let done = 0;
      while (done < out.length) {
        const n = Math.min(out.length - done, CAP);
        offset[0] = at + done;
        ctrl[1] = i;
        ctrl[2] = n;
        Atomics.store(ctrl, 0, 1);
        Atomics.notify(ctrl, 0);
        const t0 = performance.now();
        let spins = 0;
        while (Atomics.load(ctrl, 0) !== 2) {
          if ((++spins & 0xfffff) === 0 && performance.now() - t0 > SPIN_LIMIT_MS) return -1;
        }
        const got = ctrl[3];
        if (got < 0) {
          Atomics.store(ctrl, 0, 0);
          return -1;
        }
        out.set(data.subarray(0, got), done);
        Atomics.store(ctrl, 0, 0);
        done += got;
        if (got < n) break;
      }
      return done;
    },
  };
}
