// The multiplayer socket. The engine's wasm calls `globalThis.__benillaNet` (see `src/net.rs`) once a
// frame: it polls for the messages that have arrived and sends its own car's state. The relay is a
// separate server (`server/server.mjs`), and everyone connected to it shares one world; with no
// server this installs an inert stub and the game is single player.

const RETRY_MS = 3000;

/** state(): 0 off, 1 connecting, 2 open, 3 closed (retrying), 4 refused (server full). */
export function installNet({ server, name }) {
  const stub = { state: () => 0, poll: () => undefined, send() {}, error: () => "" };
  globalThis.__benillaNet = stub;
  if (!server) return;

  /** @type {WebSocket | null} */
  let ws = null;
  let state = 1;
  let error = "";
  let wasOpen = false;
  const inbox = [];

  const connect = () => {
    state = 1;
    const url = new URL(server);
    url.searchParams.set("name", name);
    ws = new WebSocket(url);
    ws.onopen = () => { state = 2; error = ""; wasOpen = true; };
    ws.onmessage = (e) => {
      if (typeof e.data === "string") inbox.push(e.data);
      // A game that stalls (a hidden tab) must not buffer without bound.
      if (inbox.length > 512) inbox.splice(0, inbox.length - 256);
    };
    ws.onclose = (e) => {
      if (e.code === 4000 || e.code === 4001 || e.code === 4002) {
        state = 4;
        error = e.reason || "refused";
        return;
      }
      state = 3;
      if (!wasOpen && !error) error = "could not reach the server";
      setTimeout(connect, RETRY_MS);
    };
    ws.onerror = () => {};
  };
  connect();

  globalThis.__benillaNet = {
    state: () => state,
    poll: () => inbox.shift(),
    send(msg) { if (ws && ws.readyState === WebSocket.OPEN) ws.send(msg); },
    error: () => error,
  };
}

/** The first connection, awaited, so the lobby can say why it failed instead of dropping into the game alone. */
export function checkServer({ server, name }, timeoutMs = 6000) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (ok, why) => { if (!done) { done = true; try { ws.close(); } catch {} resolve({ ok, why }); } };
    let ws;
    try {
      const url = new URL(server);
      if (url.protocol !== "wss:" && url.protocol !== "ws:") return resolve({ ok: false, why: "the server address must start with wss://" });
      // A page on https may not open ws:// (except to localhost).
      if (location.protocol === "https:" && url.protocol === "ws:" && !["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)) {
        return resolve({ ok: false, why: "this page is https, so the server must be wss:// (or localhost)" });
      }
      url.searchParams.set("name", name);
      url.searchParams.set("probe", "1");
      ws = new WebSocket(url);
    } catch (e) {
      return resolve({ ok: false, why: "that is not a server address" });
    }
    const timer = setTimeout(() => finish(false, "no answer from the server"), timeoutMs);
    ws.onmessage = () => { clearTimeout(timer); finish(true); };
    ws.onclose = (e) => { clearTimeout(timer); finish(false, e.reason || "could not reach the server"); };
  });
}
