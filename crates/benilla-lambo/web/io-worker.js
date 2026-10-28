// The I/O worker: the engine reads files synchronously on the main thread through `io-bridge.js`;
// this worker answers over a SharedArrayBuffer, from a chunk cache it fills ahead of the reads.
//
// Control block (Int32 at byte 0): [0] state (0 idle, 1 request, 2 done), [1] file index,
// [2] length wanted, [3] bytes read or -1. Float64 at byte 16: the offset. Data from byte 64.

let files;
let ctrl;
let offset;
let data;

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

// TEMPORARY: read counters, logged every 5 s to this worker's console.
const stat = { reads: 0, cold: 0, ms: 0, worst: 0, bytes: 0, at: performance.now() };

async function readInto(idx, at, len) {
  const t0 = performance.now();
  const done = files[idx].blob ? readBlob(idx, at, len) : await readRange(idx, at, len);
  const dt = performance.now() - t0;
  stat.reads++;
  stat.ms += dt;
  stat.bytes += done;
  stat.worst = Math.max(stat.worst, dt);
  if (t0 - stat.at > 5000) {
    console.log(`io: ${stat.reads} reads, ${stat.cold} cold chunks, ${stat.ms.toFixed(0)} ms serving, worst ${stat.worst.toFixed(0)} ms, ${(stat.bytes / 1048576).toFixed(1)} MiB`);
    Object.assign(stat, { reads: 0, cold: 0, ms: 0, worst: 0, bytes: 0, at: t0 });
  }
  return done;
}

// A picked file: from the cache if every chunk the read touches is there, else one exact
// synchronous read, the cheapest a miss can be. Only a read that continues the previous one on
// this file starts reading ahead, so scattered reads do not pull chunks nobody uses.
const reader = new FileReaderSync();
const sequel = [];

function readBlob(idx, at, len) {
  const blob = files[idx].blob;
  const end = Math.min(at + len, blob.size);
  if (at >= end) return 0;
  const first = Math.floor(at / CHUNK);
  const last = Math.floor((end - 1) / CHUNK);
  let warm = true;
  for (let n = first; warm && n <= last; n++) warm = cache.has(`${idx}#${n}`);
  let done = 0;
  if (warm) {
    for (let n = first; n <= last; n++) {
      const c = cache.get(`${idx}#${n}`);
      cache.delete(`${idx}#${n}`);
      cache.set(`${idx}#${n}`, c);
      const o = Math.max(at, n * CHUNK) - n * CHUNK;
      const piece = c.subarray(o, Math.min(end - n * CHUNK, c.length));
      data.set(piece, done);
      done += piece.length;
    }
  } else {
    stat.cold++;
    const buf = reader.readAsArrayBuffer(blob.slice(at, end));
    data.set(new Uint8Array(buf));
    done = buf.byteLength;
  }
  if (at === sequel[idx]) for (let n = last + 1; n <= last + AHEAD; n++) chunk(idx, n).catch(() => {});
  sequel[idx] = end;
  return done;
}

// A development server's file (`serve.mjs --data`): an HTTP range per chunk, from the cache when
// there, reading ahead.
async function readRange(idx, at, len) {
  let done = 0;
  while (done < len) {
    const pos = at + done;
    const n = Math.floor(pos / CHUNK);
    if (!cache.has(`${idx}#${n}`)) stat.cold++;
    const c = await chunk(idx, n);
    const o = pos - n * CHUNK;
    if (o >= c.length) break;
    const take = Math.min(len - done, c.length - o);
    data.set(c.subarray(o, o + take), done);
    done += take;
    if (c.length < CHUNK) break;
  }
  const last = Math.floor((at + Math.max(len, 1) - 1) / CHUNK);
  for (let n = last + 1; n <= last + AHEAD; n++) chunk(idx, n).catch(() => {});
  return done;
}

const CHUNK = 1 << 20;
const KEEP = 128;
const AHEAD = 3;
const cache = new Map();
const pending = new Map();

function load(file, n) {
  const from = n * CHUNK;
  if (file.blob) return file.blob.slice(from, from + CHUNK).arrayBuffer().then((b) => new Uint8Array(b));
  return fetch(file.url, { headers: { Range: `bytes=${from}-${from + CHUNK - 1}` } }).then(async (r) => {
    if (r.status !== 206 && r.status !== 416) throw new Error(`range read failed: ${r.status}`);
    return r.status === 206 ? new Uint8Array(await r.arrayBuffer()) : new Uint8Array(0);
  });
}

function chunk(idx, n) {
  const key = `${idx}#${n}`;
  const hit = cache.get(key);
  if (hit) {
    cache.delete(key);
    cache.set(key, hit);
    return Promise.resolve(hit);
  }
  let p = pending.get(key);
  if (!p) {
    p = load(files[idx], n)
      .then((bytes) => {
        cache.set(key, bytes);
        if (cache.size > KEEP) cache.delete(cache.keys().next().value);
        return bytes;
      })
      .finally(() => pending.delete(key));
    pending.set(key, p);
  }
  return p;
}

async function serve() {
  for (;;) {
    let state = Atomics.load(ctrl, 0);
    while (state !== 1) {
      // `waitAsync` lets the fetches above run while the worker is idle; Firefox lacks it and
      // blocks instead, which only costs the read-ahead.
      if (Atomics.waitAsync) {
        const w = Atomics.waitAsync(ctrl, 0, state);
        if (w.async) await w.value;
      } else {
        Atomics.wait(ctrl, 0, state);
      }
      state = Atomics.load(ctrl, 0);
    }
    let got;
    try {
      got = await readInto(ctrl[1], offset[0], ctrl[2]);
    } catch (err) {
      got = -1;
    }
    ctrl[3] = got;
    Atomics.store(ctrl, 0, 2);
    Atomics.notify(ctrl, 0);
  }
}
