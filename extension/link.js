// The computer's socket, and this browser's link (docs/explorations/
// owner-browser.md, a spike). The first time its owner connects it from
// Settings, the socket spends a one-time ticket, and the computer gives
// this browser a link: a secret kept here, and its id. With it the browser
// comes back on its own after anything but its owner's or the computer's
// word (a dropped network, a sleep, Chrome or this worker restarting):
// it asks the computer's origin for a ticket of its own and connects
// again, at once, then less often. The frames the socket carries are the
// guest's CDP (`<socket> <message>`: cdp.js), the link, and a keepalive.

// Chrome keeps a worker alive while its socket carries a message every 30 s
const PING_MS = 20_000;
const PING = '{"type":"ping"}';
const PONG = '{"type":"pong"}';
// a ticket's socket: the computer's own origin (`<24 hex>--computer…`), `/__browser`
const COMPUTER_HEX = 24;
const TICKET_HOST = new RegExp(`^[0-9a-f]{${COMPUTER_HEX}}--computer[.-]`);
// The computers it trusts: under its manifest's host permissions
// (`http://*.<zone>/*`), on the port of the platform's page that handed it
// the ticket. A page elsewhere could otherwise send it to any socket.
const ZONES = chrome.runtime.getManifest().host_permissions.map((p) => /^https?:\/\/\*\.([^/:]+)\/\*$/.exec(p)?.[1]).filter(Boolean);
// a guest socket's name, before each frame it carries (fragment_core::owner_browser::SOCKET_HEX)
const SOCKET_HEX = 8;
// the computer's word not to come back (fragment_core::owner_browser):
// replaced by a newer browser, forgotten, its owner wiped
const REPLACED = 4001;
const FORGOTTEN = 4002;
const WIPED = 4003;
// coming back: at once, then after these, then every minute (an alarm,
// which wakes this worker however long it slept)
const RETRY_MS = [1_000, 5_000, 15_000];
const ALARM = "fragment-reconnect";
// Why it is not connected, as its owner reads it after "Not connected: "
// (Settings, the popup): the computer's word not to come back, or a link
// the computer does not know (forgotten while it was away, or a computer
// that never made it: another's, or one made again).
const ENDED = {
  [REPLACED]: "another browser connected in its place",
  [FORGOTTEN]: "this browser was forgotten in Settings",
  [WIPED]: "your account was wiped",
};
const UNKNOWN = "your computer no longer knows this browser";

let socket = null;
let pinger = null;
let tries = 0;
// `computer`: the 24 hex of the computer it holds a socket to; `link`: the
// id of this browser's link, while it has one
let status = { connected: false, computer: null, link: null, error: null };
let on = { frame() {}, ended() {}, changed() {} };

/// What the rest of the worker hears: each guest socket's frame, the
/// socket's end, and a change of status.
export function wire(handlers) {
  on = handlers;
}

export function current() {
  return status;
}

export function send(text) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(text);
}

async function saved() {
  return (await chrome.storage.local.get("link")).link ?? null;
}

// Settings' ticket, from the platform's page at `page`: this browser
// connects, and is given its link.
export async function connect(url, page) {
  const u = new URL(url);
  const trusted = ZONES.some((z) => u.hostname.endsWith(`.${z}`)) && u.port === new URL(page).port;
  if (!["ws:", "wss:"].includes(u.protocol) || !TICKET_HOST.test(u.hostname) || u.pathname !== "/__browser" || !trusted) {
    throw new Error("that is not a Fragment browser ticket");
  }
  await close(1000, "connected again");
  open(url);
}

// Its owner's Disconnect, here or in Settings: the computer forgets this
// browser, and it does not come back.
export async function forget() {
  const link = await saved();
  await chrome.storage.local.remove("link");
  if (link) {
    await ask(link, "forget").catch(() => {});
  }
  await close(1000, "its owner disconnected");
  set({ connected: false, computer: null, link: null, error: null });
}

// Comes back with its link, if it has one and is not connected.
export async function resume() {
  const link = await saved();
  if (!link || socket) return;
  // coming back, as its owner sees it, from the first try
  if (status.link !== link.id) set({ ...status, link: link.id });
  let answer;
  try {
    answer = await ask(link, "ticket");
  } catch (e) {
    return later(`the computer did not answer: ${e.message}`);
  }
  if (answer.status === 401) {
    await chrome.storage.local.remove("link");
    return set({ connected: false, computer: null, link: null, error: UNKNOWN });
  }
  if (!answer.ok) return later(`the computer answered ${answer.status}`);
  open((await answer.json()).url);
}

// The computer's origin, asked as this browser: a ticket to come back
// with, or to forget it. The link goes in the body: an `Authorization`
// there is a signer's.
function ask(link, what) {
  return fetch(`${link.origin}/__browser/${what}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ link: link.secret }) });
}

function open(url) {
  const computer = new URL(url).hostname.slice(0, COMPUTER_HEX);
  const ws = new WebSocket(url);
  socket = ws;
  ws.onopen = async () => {
    tries = 0;
    chrome.alarms.clear(ALARM);
    ws.send(JSON.stringify({ type: "hello", name: browserName() }));
    pinger = setInterval(() => ws.readyState === WebSocket.OPEN && ws.send(PING), PING_MS);
    set({ connected: true, computer, link: (await saved())?.id ?? null, error: null });
  };
  ws.onmessage = (e) => heard(String(e.data), url);
  ws.onclose = (e) => {
    if (socket !== ws) return;
    socket = null;
    clearInterval(pinger);
    on.ended();
    ended(e.code, e.reason);
  };
}

async function heard(frame, url) {
  if (frame === PONG) return;
  if (frame.startsWith("{")) {
    let m;
    try {
      m = JSON.parse(frame);
    } catch {
      return;
    }
    if (m.type !== "link") return;
    // the computer's origin, where this browser comes back
    const origin = new URL(url.replace(/^ws/, "http")).origin;
    await chrome.storage.local.set({ link: { origin, id: m.id, secret: m.link } });
    return set({ ...status, link: m.id });
  }
  const at = frame.indexOf(" ");
  if (at === SOCKET_HEX) on.frame(frame.slice(0, at), frame.slice(at + 1));
}

async function ended(code, reason) {
  // told not to come back (replaced, forgotten, wiped): its link goes, so a
  // worker Chrome starts again later never takes the slot back
  const told = code in ENDED;
  if (told) await chrome.storage.local.remove("link");
  const back = !told && (await saved());
  set({ connected: false, computer: null, link: back ? status.link : null, error: back ? null : (ENDED[code] ?? (reason || null)) });
  if (back) later(null);
}

// Tries again: soon, then each minute, until it is connected or has no link.
function later(error) {
  if (error) set({ ...status, error });
  if (tries < RETRY_MS.length) setTimeout(resume, RETRY_MS[tries++]);
  else chrome.alarms.create(ALARM, { periodInMinutes: 1 });
}

chrome.alarms.onAlarm.addListener((a) => a.name === ALARM && resume());

async function close(code, reason) {
  const ws = socket;
  socket = null;
  clearInterval(pinger);
  chrome.alarms.clear(ALARM);
  tries = 0;
  if (ws && ws.readyState <= WebSocket.OPEN) ws.close(code, reason);
  if (ws) on.ended();
}

function set(next) {
  status = next;
  on.changed(status);
}

// How its owner sees this browser in Settings: its brand and platform.
function browserName() {
  const ua = navigator.userAgentData;
  const brand = ua?.brands?.map((b) => b.brand).find((b) => !/not.?a.?brand|chromium/i.test(b)) ?? (ua?.brands?.some((b) => /chromium/i.test(b.brand)) ? "Chromium" : "A browser");
  return ua?.platform ? `${brand} on ${ua.platform}` : brand;
}
