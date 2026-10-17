// The guest's CDP, answered as a browser's DevTools would answer it
// (docs/explorations/owner-browser.md, a spike): each guest socket's
// frames arrive named (`<socket> <message>`, link.js), a page's methods
// go to its tab through chrome.debugger, and the browser's own
// (`Target.*`, `Browser.*`) are answered here, over the tabs the agent
// may use (access.js). It detaches once no guest socket is left, and the
// person's closing Chrome's "debugging this browser" bar disconnects it.

import * as access from "./access.js";
import * as link from "./link.js";

const PROTOCOL = "1.3";
// a guest socket's end, as the computer frames it (fragment_core::owner_browser::CLOSE)
const CLOSE = "close";
// JSON-RPC's code for a server's own error: CDP answers a refused call with it
const SERVER_ERROR = -32000;
// the browser context `Target.createBrowserContext` answers: a new window
const NEW_WINDOW = "new-window";

// each guest socket's: its sessions (sessionId -> tabId), and whether it
// asked to hear of targets
const clients = new Map();
// the tabs the debugger is attached to, and the tabs announced to clients
// that asked (tabId -> its target's id)
const attached = new Set();
const shown = new Map();
let group = null;

function send(client, message) {
  link.send(`${client} ${JSON.stringify(message)}`);
}

// A frame for guest socket `client`: a CDP message, or its end.
export function said(client, message) {
  if (message === CLOSE) return forget(client);
  let m;
  try {
    m = JSON.parse(message);
  } catch {
    return;
  }
  if (typeof m.id !== "number" || typeof m.method !== "string") return;
  answer(client, m).then((reply) => send(client, reply));
}

// A guest socket ended: its sessions go, and once none is left the
// debugger lets go of every tab (Chrome's bar goes with it).
function forget(client) {
  clients.delete(client);
  if (!clients.size) detachAll();
}

// The computer's socket ended: every guest socket with it.
export function ended() {
  clients.clear();
  shown.clear();
  detachAll();
}

function detachAll() {
  for (const tabId of attached) chrome.debugger.detach({ tabId }).catch(() => {});
  attached.clear();
}

function clientOf(client) {
  if (!clients.has(client)) clients.set(client, { sessions: new Map(), discover: false });
  return clients.get(client);
}

function infoOf(t) {
  return { targetId: t.id, type: "page", title: t.title ?? "", url: t.url ?? "", attached: attached.has(t.tabId), canAccessOpener: false, browserContextId: "default" };
}

async function targetOf(targetId) {
  const t = (await access.usable()).find((t) => t.id === targetId);
  if (!t) throw new Error(`no target ${targetId}: its owner has not given the agent that tab (the extension's Access)`);
  return t;
}

// What the agent may use changed (the person's choice, or a tab came or
// went): every client that asked hears what it gained and lost, and the
// agent lets go of what it may use no more.
export async function refresh() {
  const now = await access.usable();
  const ids = new Set(now.map((t) => t.tabId));
  detach((tabId) => !ids.has(tabId));
  for (const tabId of [...attached]) {
    if (ids.has(tabId)) continue;
    attached.delete(tabId);
    chrome.debugger.detach({ tabId }).catch(() => {});
  }
  const discovering = [...clients].filter(([, c]) => c.discover);
  for (const t of now) {
    if (shown.has(t.tabId)) continue;
    shown.set(t.tabId, t.id);
    for (const [client] of discovering) send(client, { method: "Target.targetCreated", params: { targetInfo: infoOf(t) } });
  }
  for (const [tabId, targetId] of [...shown]) {
    if (ids.has(tabId)) continue;
    shown.delete(tabId);
    for (const [client] of discovering) send(client, { method: "Target.targetDestroyed", params: { targetId } });
  }
}

// Each client's sessions on a tab `gone` says is gone: dropped, and the
// client told.
function detach(gone) {
  for (const [client, c] of clients) {
    for (const [sessionId, tabId] of c.sessions) {
      if (!gone(tabId)) continue;
      c.sessions.delete(sessionId);
      send(client, { method: "Target.detachedFromTarget", params: { sessionId } });
    }
  }
}

async function answer(client, m) {
  await access.ready;
  const c = clientOf(client);
  const session = m.sessionId ? { sessionId: m.sessionId } : {};
  try {
    const result = m.sessionId ? await onPage(c, m) : await onBrowser(client, c, m);
    return { id: m.id, result, ...session };
  } catch (e) {
    return { id: m.id, error: { code: SERVER_ERROR, message: String(e?.message ?? e) }, ...session };
  }
}

// The browser's own methods, over the tabs the agent may use.
async function onBrowser(client, c, { method, params = {} }) {
  switch (method) {
    case "Browser.getVersion": {
      const v = navigator.userAgent.match(/Chrome\/([\d.]+)/)?.[1] ?? "";
      return { protocolVersion: PROTOCOL, product: `Chrome/${v}`, revision: "", userAgent: navigator.userAgent, jsVersion: "" };
    }
    case "Target.setDiscoverTargets": {
      c.discover = !!params.discover;
      if (!c.discover) return {};
      for (const t of await access.usable()) {
        shown.set(t.tabId, t.id);
        send(client, { method: "Target.targetCreated", params: { targetInfo: infoOf(t) } });
      }
      return {};
    }
    case "Target.getTargets": {
      // a browser always has a page: the agent's first, opened blank
      if (!(await access.usable()).length) await open("about:blank", false);
      return { targetInfos: (await access.usable()).map(infoOf) };
    }
    case "Target.getTargetInfo":
      return { targetInfo: infoOf(await targetOf(params.targetId)) };
    case "Target.createTarget": {
      const tabId = await open(params.url || "about:blank", !!params.newWindow || params.browserContextId === NEW_WINDOW);
      const t = (await access.usable()).find((t) => t.tabId === tabId);
      return { targetId: t.id };
    }
    case "Target.attachToTarget": {
      const t = await targetOf(params.targetId);
      if (params.flatten === false) throw new Error("only flattened sessions are offered");
      if (!attached.has(t.tabId)) {
        await chrome.debugger.attach({ tabId: t.tabId }, PROTOCOL);
        attached.add(t.tabId);
      }
      const sessionId = crypto.randomUUID().replaceAll("-", "").toUpperCase();
      c.sessions.set(sessionId, t.tabId);
      send(client, { method: "Target.attachedToTarget", params: { sessionId, targetInfo: infoOf(t), waitingForDebugger: false } });
      return { sessionId };
    }
    case "Target.detachFromTarget":
      c.sessions.delete(params.sessionId);
      return {};
    case "Target.activateTarget": {
      const tab = await chrome.tabs.update((await targetOf(params.targetId)).tabId, { active: true });
      await chrome.windows.update(tab.windowId, { focused: true });
      return {};
    }
    case "Target.closeTarget":
      await chrome.tabs.remove((await targetOf(params.targetId)).tabId);
      return { success: true };
    case "Target.setAutoAttach":
      // the agent attaches to its tabs itself; nothing else is offered
      return {};
    case "Target.createBrowserContext":
      // no profile apart from the person's: a "new context" is a new window
      // of theirs (`Target.createTarget` with it), with their sign-ins
      return { browserContextId: NEW_WINDOW };
    case "Target.disposeBrowserContext":
      return {};
    case "Browser.getWindowForTarget": {
      const t = await targetOf(params.targetId);
      const w = await chrome.windows.get((await chrome.tabs.get(t.tabId)).windowId);
      return { windowId: w.id, bounds: { left: w.left, top: w.top, width: w.width, height: w.height, windowState: w.state } };
    }
    default:
      throw new Error(`${method} is not offered through its owner's browser`);
  }
}

// A page's methods, on a tab the agent may use, through chrome.debugger.
async function onPage(c, { method, params = {}, sessionId }) {
  const tabId = c.sessions.get(sessionId);
  if (tabId === undefined) throw new Error(`no session ${sessionId}`);
  if (!access.allowed(tabId)) throw new Error("its owner no longer gives the agent that tab");
  // a page's own targets (its frames, its workers) are not offered
  if (method.startsWith("Target.")) return {};
  return chrome.debugger.sendCommand({ tabId }, method, params);
}

// A tab of the agent's own, in its "Agent" group (a window of its own
// when asked), heard of by every client that asked.
async function open(url, inWindow) {
  const tab = inWindow ? (await chrome.windows.create({ url, focused: false })).tabs[0] : await chrome.tabs.create({ url, active: false });
  await access.adopt(tab.id);
  await grouped(tab.id);
  await refresh();
  return tab.id;
}

async function grouped(tabId) {
  for (const fresh of [false, true]) {
    try {
      if (group !== null && !fresh) {
        await chrome.tabs.group({ groupId: group, tabIds: [tabId] });
      } else {
        group = await chrome.tabs.group({ tabIds: [tabId] });
        await chrome.tabGroups.update(group, { title: "Agent", color: "purple" });
      }
      return;
    } catch {
      // its group was closed, or is in another window: a new one; a browser with no tab strip: none
      group = null;
    }
  }
}

chrome.debugger.onEvent.addListener((source, method, params) => {
  // a page's own: each client attached to the tab hears it, as its session
  if (source.sessionId || !attached.has(source.tabId)) return;
  for (const [client, c] of clients) {
    for (const [sessionId, tabId] of c.sessions) if (tabId === source.tabId) send(client, { method, params, sessionId });
  }
});

chrome.debugger.onDetach.addListener((source, reason) => {
  if (!attached.delete(source.tabId)) return;
  // the person closed Chrome's "debugging this browser" bar: they said stop
  if (reason === "canceled_by_user") return link.forget();
  detach((tabId) => tabId === source.tabId);
});

chrome.tabs.onRemoved.addListener(async (tabId) => {
  attached.delete(tabId);
  await access.closed(tabId);
  if (link.current().connected) await refresh();
});

chrome.tabs.onCreated.addListener(() => link.current().connected && refresh());

chrome.tabs.onUpdated.addListener(async (tabId, change) => {
  if (!link.current().connected || !shown.has(tabId) || !(change.url || change.title)) return;
  const t = (await access.usable()).find((t) => t.tabId === tabId);
  if (!t) return;
  for (const [client, c] of clients) if (c.discover) send(client, { method: "Target.targetInfoChanged", params: { targetInfo: infoOf(t) } });
});
