// The I/O worker: the one place that can read a picked file synchronously. The engine asks for
// bytes on the main thread through `io-bridge.js`; this worker answers over a SharedArrayBuffer.
//
// Control block (Int32 at byte 0): [0] state (0 idle, 1 request, 2 done), [1] file index,
// [2] length wanted, [3] bytes read or -1. Float64 at byte 16: the offset. Data from byte 64.

let files;
let ctrl;
let offset;
let data;
const reader = new FileReaderSync();

onmessage = (e) => {
  const m = e.data;
  if (m.type !== "init") return;
  files = m.files;
  ctrl = new Int32Array(m.sab, 0, 4);
  offset = new Float64Array(m.sab, 16, 1);
  data = new Uint8Array(m.sab, 64);
  postMessage({ type: "ready" });
  serve();
};

function readInto(file, at, len) {
  if (file.blob) {
    const end = Math.min(at + len, file.blob.size);
    if (at >= end) return 0;
    const buf = reader.readAsArrayBuffer(file.blob.slice(at, end));
    data.set(new Uint8Array(buf));
    return buf.byteLength;
  }
  // A development server's file (`serve.mjs --data`), through the chunk cache.
  let done = 0;
  while (done < len) {
    const pos = at + done;
    const n = Math.floor(pos / CHUNK);
    const c = chunk(file, n);
    const o = pos - n * CHUNK;
    if (o >= c.length) break;
    const take = Math.min(len - done, c.length - o);
    data.set(c.subarray(o, o + take), done);
    done += take;
    if (c.length < CHUNK) break;
  }
  return done;
}

// The engine's reads are small and many; a request each is slow, and slower still with the
// DevTools Network panel recording. Fetch aligned 1 MiB chunks by HTTP range and keep the recent.
const CHUNK = 1 << 20;
const KEEP = 64;
const cache = new Map();

function chunk(file, n) {
  const key = `${file.url}#${n}`;
  const hit = cache.get(key);
  if (hit) {
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }
  const xhr = new XMLHttpRequest();
  xhr.open("GET", file.url, false);
  xhr.setRequestHeader("Range", `bytes=${n * CHUNK}-${(n + 1) * CHUNK - 1}`);
  xhr.responseType = "arraybuffer";
  xhr.send();
  if (xhr.status !== 206 && xhr.status !== 416) throw new Error(`range read failed: ${xhr.status}`);
  const bytes = xhr.status === 206 ? new Uint8Array(xhr.response) : new Uint8Array(0);
  cache.set(key, bytes);
  if (cache.size > KEEP) cache.delete(cache.keys().next().value);
  return bytes;
}

function serve() {
  for (;;) {
    let state = Atomics.load(ctrl, 0);
    while (state !== 1) {
      Atomics.wait(ctrl, 0, state);
      state = Atomics.load(ctrl, 0);
    }
    let got;
    try {
      got = readInto(files[ctrl[1]], offset[0], ctrl[2]);
    } catch (err) {
      got = -1;
    }
    ctrl[3] = got;
    Atomics.store(ctrl, 0, 2);
    Atomics.notify(ctrl, 0);
  }
}
