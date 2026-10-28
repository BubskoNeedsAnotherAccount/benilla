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
  // A development server's file, by HTTP range (`serve.mjs --data`).
  const xhr = new XMLHttpRequest();
  xhr.open("GET", file.url, false);
  xhr.setRequestHeader("Range", `bytes=${at}-${at + len - 1}`);
  xhr.responseType = "arraybuffer";
  xhr.send();
  if (xhr.status !== 206) return xhr.status === 416 ? 0 : -1;
  data.set(new Uint8Array(xhr.response));
  return xhr.response.byteLength;
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
