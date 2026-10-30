// The benilla-lambo relay: rooms of up to MAX_PLAYERS cars, each client sending its own car's state
// and the server fanning it out to the rest of the room. It runs no physics and holds no game data
// or account: a client is its socket, a room is a name in the URL, and a room exists while someone is in it.
//
//   node server.mjs                      # PORT (default 8787)
//   ORIGINS=https://you.github.io node server.mjs
//
// Client -> server: `wss://host/?room=<name>&name=<driver>`, then `{"t":"s", ...}` state messages.
// Server -> client: `welcome` (own id and who is here), `join`, `leave`, and the others' `s`.

import { createServer } from "node:http";
import { WebSocketServer } from "ws";

const PORT = Number(process.env.PORT) || 8787;
const MAX_PLAYERS = Number(process.env.MAX_PLAYERS) || 8;
const MAX_ROOMS = Number(process.env.MAX_ROOMS) || 200;
// Browsers send the page's origin on a WebSocket; an empty list accepts any page.
const ORIGINS = (process.env.ORIGINS || "").split(",").map((s) => s.trim().replace(/\/$/, "")).filter(Boolean);
// A state message is ~200 bytes; clients send 20 a second.
const MAX_BYTES = 1024;
const MAX_STATES_PER_SEC = 40;

/** @type {Map<string, Map<number, {ws: import("ws").WebSocket, name: string}>>} */
const rooms = new Map();
let nextId = 1;

const http = createServer((req, res) => {
  const players = [...rooms.values()].reduce((n, r) => n + r.size, 0);
  res.writeHead(200, { "content-type": "application/json", "access-control-allow-origin": "*" });
  res.end(JSON.stringify({ ok: true, rooms: rooms.size, players }));
});

const wss = new WebSocketServer({
  server: http,
  maxPayload: MAX_BYTES,
  verifyClient: ({ origin }) => !ORIGINS.length || (origin !== undefined && ORIGINS.includes(origin.replace(/\/$/, ""))),
});

const num = (v, lim) => (typeof v === "number" && Number.isFinite(v) && Math.abs(v) <= lim ? Math.round(v * 1000) / 1000 : null);
const vec3 = (v, lim) => {
  if (!Array.isArray(v) || v.length !== 3) return null;
  const out = v.map((x) => num(x, lim));
  return out.includes(null) ? null : out;
};

/** Rebuilds a state message from its known fields, or null if any is missing or out of range. */
function cleanState(m) {
  const p = vec3(m.p, 1e6), v = vec3(m.v, 1e4), u = vec3(m.u, 2);
  const y = num(m.y, 1e4);
  if (!p || !v || !u || y === null) return null;
  const out = { t: "s", p, v, y, u, n: m.n ? 1 : 0, d: m.d === 1 || m.d === 2 || m.d === 3 ? m.d : 0 };
  // The versus course the driver is on, and whether its start is counting down.
  if (typeof m.r === "string" && m.r.length > 0 && m.r.length <= 64) {
    out.r = m.r;
    out.c = m.c ? 1 : 0;
  }
  return out;
}

function send(ws, msg) {
  if (ws.readyState === ws.OPEN) ws.send(JSON.stringify(msg));
}

wss.on("connection", (ws, req) => {
  const url = new URL(req.url || "/", "http://x");
  const room = url.searchParams.get("room") || "";
  const name = (url.searchParams.get("name") || "").replace(/[^\p{L}\p{N} _.-]/gu, "").trim().slice(0, 16) || "driver";
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(room)) return ws.close(4000, "bad room name");

  // The lobby's check before the game starts: say whether the room has space, and join nothing.
  if (url.searchParams.has("probe")) {
    const n = rooms.get(room)?.size ?? 0;
    if (n >= MAX_PLAYERS) return ws.close(4001, "room is full");
    send(ws, { t: "probe", players: n });
    return ws.close(1000, "probe");
  }

  let members = rooms.get(room);
  if (!members) {
    if (rooms.size >= MAX_ROOMS) return ws.close(4002, "server is full");
    members = new Map();
    rooms.set(room, members);
  }
  if (members.size >= MAX_PLAYERS) return ws.close(4001, "room is full");

  const id = nextId++;
  send(ws, { t: "welcome", id, players: [...members].map(([pid, m]) => ({ id: pid, name: m.name })) });
  for (const m of members.values()) send(m.ws, { t: "join", id, name });
  members.set(id, { ws, name });
  console.log(`+ ${name} (#${id}) -> ${room} [${members.size}]`);

  let alive = true;
  ws.on("pong", () => (alive = true));
  ws._ping = setInterval(() => {
    if (!alive) return ws.terminate();
    alive = false;
    ws.ping();
  }, 30000);

  let windowStart = Date.now(), inWindow = 0;
  ws.on("message", (data, isBinary) => {
    if (isBinary) return;
    const now = Date.now();
    if (now - windowStart >= 1000) { windowStart = now; inWindow = 0; }
    if (++inWindow > MAX_STATES_PER_SEC) return;
    let msg;
    try { msg = JSON.parse(data.toString()); } catch { return; }
    if (!msg || msg.t !== "s") return;
    const state = cleanState(msg);
    if (!state) return;
    state.id = id;
    const out = JSON.stringify(state);
    for (const [pid, m] of members) if (pid !== id && m.ws.readyState === m.ws.OPEN) m.ws.send(out);
  });

  ws.on("close", () => {
    clearInterval(ws._ping);
    if (!members.delete(id)) return;
    for (const m of members.values()) send(m.ws, { t: "leave", id });
    if (!members.size) rooms.delete(room);
    console.log(`- ${name} (#${id}) <- ${room} [${members.size}]`);
  });
  ws.on("error", () => ws.terminate());
});

http.listen(PORT, () => console.log(`benilla-lambo relay on :${PORT}${ORIGINS.length ? ` (origins: ${ORIGINS.join(", ")})` : " (any origin)"}`));
