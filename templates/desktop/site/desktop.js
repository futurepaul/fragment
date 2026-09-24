// The desktop: its owner's fragments side by side. Chats are chat
// fragments (the middle column shows the open one), apps open as panes in
// the viewer, and a file opens through its own fragment's __file. Each
// frame signs in on its own origin (through its __signin), so the desktop
// holds no authority over any of them: the one platform power it has is
// its owner's list (`__fragments`, which fragment.json asks for).
import * as fragment from "./__fragment.js";
import { createLayout, store } from "./layout.js";
import { createViewer } from "./viewer.js";

const $ = (id) => document.getElementById(id);
const ICON = {
  chat: '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"/>',
  file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
  grid: '<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>',
  reload: '<path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 4v7h-7"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;
const CURRENT = "desktop.chat.v1";
// the platform's templates an app can start from
const CATALOG = [
  { template: "todo", name: "Todo", about: "A list, live for everyone who has it open." },
  { template: "inbox", name: "Inbox", about: "Webhooks in, a job to read each one." },
  { template: "blank", name: "Blank", about: "One page to start from." },
];

const state = { fragments: [], chats: [], current: store.get(CURRENT, null), frames: new Map() };

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// The platform's answer for this page: the owner's fragments, or a new one.
async function platform(body) {
  const init = body ? { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) } : {};
  const r = await fetch("__fragments", { credentials: "same-origin", ...init });
  const out = await r.json().catch(() => ({}));
  if (!r.ok) throw Object.assign(new Error(out.message || r.statusText), { status: r.status });
  return out;
}

const byName = (name) => state.fragments.find((f) => f.name === name);
const label = (name) => name.split(".")[0];
// A frame goes through the fragment's own sign-in, which comes straight
// back when the owner is signed in there already.
const framed = (url, path = "/") => `${url}__signin?return=${encodeURIComponent(path)}`;
const fresh = (prefix) => `${prefix}-${crypto.getRandomValues(new Uint32Array(1))[0].toString(36).slice(0, 5)}`;

function notice(title, text, action) {
  const n = $("notice");
  n.replaceChildren(el("strong", null, title));
  if (text) n.append(text);
  if (action) n.append(" ", action);
  n.hidden = false;
}

// ---- layout: sidebar | chat | viewer ----
const layout = createLayout({
  grid: $("layout"),
  gutters: [$("gutter-left"), $("gutter-right")],
  onChange: (shown) => { $("scrim").classList.toggle("on", $("layout").classList.contains("narrow") && (shown.left || shown.right)); },
});
const viewer = createViewer({
  stack: $("stack"),
  onChange: (keys) => {
    $("viewer").classList.toggle("is-empty", !keys.length);
    $("viewer-count").hidden = !keys.length;
    $("viewer-count").textContent = keys.length;
    for (const row of $("apps").querySelectorAll(".row[data-key]")) row.classList.toggle("open", keys.includes(row.dataset.key));
    if (!keys.length) renderQuickOpen();
  },
});

$("toggle-left").onclick = () => layout.toggle("left");
$("collapse-left").onclick = () => layout.hide("left");
$("toggle-right").onclick = () => layout.toggle("right");
$("collapse-right").onclick = () => layout.hide("right");
$("scrim").onclick = () => { layout.hide("left"); layout.hide("right"); };
$("new-chat-top").onclick = () => $("new-chat").click();
addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || e.code !== "KeyB") return;
  e.preventDefault();
  layout.toggle(e.altKey ? "right" : "left");
});
// In the narrow layout the sidebar is an overlay; picking something closes it.
const leaveSidebar = () => { if (layout.narrow) layout.hide("left"); };

function show(spec) {
  viewer.open(spec);
  layout.show("right");
}

// ---- chats: each a chat fragment, shown in the middle column ----
function renderChats() {
  const chats = state.chats.filter(byName);
  $("chats").replaceChildren(...(chats.length ? chats.map((name) => {
    const row = el("button", `row${name === state.current ? " active" : ""}`);
    row.innerHTML = svg("chat");
    row.append(el("span", "label", label(name)));
    row.onclick = () => { openChat(name); leaveSidebar(); };
    return row;
  }) : [el("div", "empty-row", "No chats yet")]));
}

function openChat(name) {
  const f = byName(name);
  if (!f) return;
  state.current = name;
  store.set(CURRENT, name);
  $("chat-title").textContent = label(name);
  $("notice").hidden = true;
  if (!state.frames.has(name)) {
    const frame = el("iframe");
    frame.title = label(name);
    frame.src = framed(f.url);
    frame.dataset.fragment = name;
    $("frames").append(frame);
    state.frames.set(name, frame);
  }
  for (const [n, frame] of state.frames) frame.hidden = n !== name;
  renderChats();
}

$("new-chat").onclick = async () => {
  $("new-chat").disabled = true;
  try {
    const made = await platform({ label: fresh("chat"), template: "chat" });
    await fragment.call("add_chat", { name: made.name });
    await load();
    openChat(made.name);
    leaveSidebar();
  } catch (e) {
    notice("A new chat could not be made", e.message);
  } finally {
    $("new-chat").disabled = false;
  }
};

// ---- apps: every other fragment, each a pane in the viewer ----
const PASTELS = [["#8ab4ff", "rgba(138,180,255,.16)"], ["#39d98a", "rgba(57,217,138,.16)"], ["#f2b36b", "rgba(242,179,107,.16)"], ["#e58fd1", "rgba(229,143,209,.16)"], ["#9ad0d8", "rgba(154,208,216,.16)"], ["#c9b6ff", "rgba(201,182,255,.16)"]];
function appIcon(name) {
  const [fg, bg] = PASTELS[[...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7) % PASTELS.length];
  const i = el("span", "app-icon", name[0]?.toUpperCase() || "?");
  i.style.color = fg;
  i.style.background = bg;
  return i;
}

function paneIcon(name) {
  const i = el("span", "pane-icon");
  i.innerHTML = svg(name);
  return i;
}

const apps = () => state.fragments.filter((f) => f.name !== state.self && !state.chats.includes(f.name));

function renderApps() {
  const open = viewer.keys;
  const list = apps();
  $("apps").replaceChildren(...(list.length ? list.map((f) => {
    const row = el("button", `row${open.includes(`app:${f.name}`) ? " open" : ""}`);
    row.dataset.key = `app:${f.name}`;
    row.append(appIcon(label(f.name)), el("span", "label", label(f.name)));
    if (f.role !== "owner") row.append(el("span", "meta", f.role));
    row.onclick = () => { openApp(f.name); leaveSidebar(); };
    return row;
  }) : [el("div", "empty-row", "No apps yet")]));
}

function frameFor(url, title) {
  const frame = el("iframe");
  frame.title = title;
  frame.src = url;
  return frame;
}

function openApp(name) {
  const f = byName(name);
  if (!f) return;
  const frame = frameFor(framed(f.url), label(name));
  show({
    key: `app:${name}`, title: label(name), subtitle: f.role === "owner" ? undefined : f.role, icon: appIcon(label(name)), body: frame,
    actions: [
      { icon: ICON.folder, title: "Files", onClick: () => openTree(name) },
      { icon: ICON.reload, title: "Reload", onClick: () => { frame.src = framed(f.url); } },
    ],
  });
}

// A fragment's files: its own __files page, whose links ask this page to
// open them (the message listener below).
function openTree(name) {
  const f = byName(name);
  if (!f) return;
  show({ key: `tree:${name}`, title: label(name), subtitle: "files", icon: paneIcon("folder"), body: frameFor(framed(f.url, "/__files"), `${label(name)} files`) });
}

// A file, read by its own fragment (`<fragment>/__file?path=`).
function openFile(url, title) {
  const origin = new URL(url).origin;
  const f = state.fragments.find((x) => new URL(x.url).origin === origin);
  if (!f) return;
  const path = new URL(url).searchParams.get("path") || title;
  const cut = path.lastIndexOf("/");
  show({
    key: `file:${url}`, title: path.slice(cut + 1), subtitle: [label(f.name), path.slice(0, Math.max(cut, 0))].filter(Boolean).join(" / "), icon: paneIcon("file"),
    body: frameFor(url, path),
    actions: [{ icon: ICON.folder, title: `All files in ${label(f.name)}`, onClick: () => openTree(f.name) }],
  });
}

// Only a frame of one of the owner's own fragments may ask for a file.
addEventListener("message", (e) => {
  const ask = e.data;
  if (ask?.fragment !== "open" || typeof ask.url !== "string") return;
  if (!state.fragments.some((f) => new URL(f.url).origin === e.origin) || new URL(ask.url, e.origin).origin !== e.origin) return;
  openFile(new URL(ask.url, e.origin).href, String(ask.title || ""));
});

// ---- the catalog: a new app from one of the platform's templates ----
function showCatalog() {
  const box = el("div", "catalog");
  box.replaceChildren(...CATALOG.map((t) => {
    const card = el("form", "card");
    const input = el("input");
    input.name = "label";
    input.placeholder = t.template;
    input.pattern = "[a-z0-9]([a-z0-9\\-]*[a-z0-9])?";
    input.maxLength = 63;
    const error = el("div", "error");
    const add = el("button", null, "Make it");
    card.append(appIcon(t.name), el("strong", null, t.name), el("p", null, t.about), input, add, error);
    card.onsubmit = async (e) => {
      e.preventDefault();
      add.disabled = true;
      error.textContent = "";
      try {
        const made = await platform({ label: input.value.trim() || fresh(t.template), template: t.template });
        await load();
        viewer.close("catalog");
        openApp(made.name);
      } catch (err) {
        error.textContent = err.message;
      } finally {
        add.disabled = false;
      }
    };
    return card;
  }));
  show({ key: "catalog", title: "Add an app", icon: paneIcon("grid"), body: box, persist: false });
}
$("add-app").onclick = () => { showCatalog(); leaveSidebar(); };

// Shown in the viewer when nothing is open.
function renderQuickOpen() {
  const rows = apps().map((f) => {
    const r = el("button", "row");
    r.append(appIcon(label(f.name)), el("span", "label", label(f.name)));
    r.onclick = () => openApp(f.name);
    return r;
  });
  const add = el("button", "row");
  add.innerHTML = svg("grid");
  add.append(el("span", "label", "Add an app…"));
  add.onclick = showCatalog;
  $("quick-open").replaceChildren(...rows, add);
}

// ---- loading ----
async function load() {
  const [{ fragments }, { chats }] = await Promise.all([platform(), fragment.call("chats", {})]);
  state.fragments = fragments;
  state.chats = chats;
  // this desktop is one of them: the one this page is under
  state.self = fragments.find((f) => location.href.startsWith(f.url))?.name ?? null;
  renderChats();
  renderApps();
  renderQuickOpen();
}

// Reopen what was open last time (bottom first: new panes open on top).
function restoreViewer() {
  for (const { key } of viewer.saved().reverse()) {
    const [kind, ...rest] = key.split(":");
    const name = rest.join(":");
    if (kind === "app") openApp(name);
    else if (kind === "tree") openTree(name);
    else if (kind === "file") openFile(name, "");
  }
}

async function start() {
  try {
    await load();
  } catch (e) {
    if (e.status === 403 || e.status === 401) {
      // who this page is to the fragment, if the socket says in time
      const hello = await Promise.race([fragment.me(), new Promise((r) => setTimeout(r, 3000))]);
      const signedIn = hello && !String(hello.principal || "").startsWith("anon:");
      const signin = el("a", null, "Sign in");
      signin.href = `__signin?return=${encodeURIComponent("/")}`;
      notice("This desktop is its owner's", signedIn ? "It shows only to the person who made it." : "If it is yours, sign in to see it.", signedIn ? undefined : signin);
    } else {
      notice("Your desktop could not load", e.message);
    }
    return;
  }
  const username = state.self?.split(".")[1];
  if (username) $("brand").textContent = `${username}'s desktop`;
  const chats = state.chats.filter(byName);
  if (chats.length) openChat(chats.includes(state.current) ? state.current : chats[0]);
  else notice("No chats yet", "Start one with New chat.");
  restoreViewer();
}

start();
