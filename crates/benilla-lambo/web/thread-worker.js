// A worker of the threaded wasm build: a second instance of the game's module on the page's shared
// memory, which does nothing but run the jobs the engine queues (`src/web.rs`, `thread_entry`,
// which does not return). Archive reads are among those jobs, so the worker reads the picked `Data`
// files itself: a worker may block, so a plain synchronous read does, with no bridge to the page.

/** @param {{blob?: Blob, url?: string}[]} files */
function installIo(files) {
  const reader = new FileReaderSync();
  globalThis.__benillaIo = {
    /** Fill `out` from file `i` at `at`; the bytes read, or -1 (the page's `io-bridge.js` contract). */
    read(i, at, out) {
      const f = files[i];
      if (!f) return -1;
      try {
        let buf;
        if (f.blob) {
          buf = reader.readAsArrayBuffer(f.blob.slice(at, at + out.length));
        } else {
          const xhr = new XMLHttpRequest();
          xhr.open("GET", f.url, false);
          xhr.responseType = "arraybuffer";
          xhr.setRequestHeader("Range", `bytes=${at}-${at + out.length - 1}`);
          xhr.send();
          if (xhr.status === 416) return 0;
          if (xhr.status !== 206) return -1;
          buf = xhr.response;
        }
        const bytes = new Uint8Array(buf, 0, Math.min(buf.byteLength, out.length));
        out.set(bytes);
        return bytes.length;
      } catch {
        return -1;
      }
    },
  };
}

onmessage = async (e) => {
  const { module, memory, files } = e.data;
  installIo(files || []);
  const { default: init, thread_entry } = await import("./benilla-lambo.js");
  await init({ module_or_path: module, memory });
  thread_entry();
};
