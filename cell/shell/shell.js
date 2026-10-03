// The shell: the platform's one page (docs/cloudflare-v1.md, decisions
// 6–12). Its sidebar is the person's fragments by kind: their agents (each
// a chat, with its lead agent's name and colour) and their apps. The open
// chat fills the middle column and apps open as windows in the viewer;
// each is a frame of the fragment's own origin, signed in there by the
// platform's frame mint (`/auth/frame`), for this page only. This page
// holds no key: it calls the API with the person's platform session
// (`x-fragment-shell`, same-origin only). It knows nothing of Hermes:
// everything in a frame is a fragment. Design: Skyler's handoff
// (2026-10-02); what it did not draw is the older prototype's, in its look.
import "./tooltips.js";
import { avatar } from "./agent-identity.js";
import { appIcon } from "./app-icons.js";
import { createLayout, store } from "./layout.js";
import { createViewer } from "./viewer.js";
import { LUCIDE_ICON } from "./lucide-icons.js";

const $ = (id) => document.getElementById(id);
const ICON = {
  rename: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>',
  invite: '<circle cx="9" cy="8" r="3.2"/><path d="M3 19a6 6 0 0 1 12 0M19 8v6M16 11h6"/>',
  agent: '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
  grid: '<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>',
  reload: LUCIDE_ICON.reload,
  folder: LUCIDE_ICON.folder,
  share: LUCIDE_ICON.share,
  more: '<circle cx="5" cy="12" r="1.3"/><circle cx="12" cy="12" r="1.3"/><circle cx="19" cy="12" r="1.3"/>',
  people: '<circle cx="9" cy="8" r="3.2"/><path d="M3 19a6 6 0 0 1 12 0"/><path d="M16 5.2a3.2 3.2 0 0 1 0 5.6M18 19a6 6 0 0 0-2.5-4.9"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>',
  screen: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;
// Skyler's agent palette: a plain colored circle, no eyes
const COLORS = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];
const NAMES = ["Juniper", "Maple", "Willow", "Rowan", "Hazel", "Sage", "Aspen", "Linden", "Alder", "Cedar", "Briar", "Fern"];
const CURRENT = "shell.chat.v1";
// the templates an app starts from (the platform's catalog: publish.rs)
const CATALOG = [
  { template: "todo", name: "Todo", about: "A list, live for everyone who has it open." },
  { template: "inbox", name: "Inbox", about: "Webhooks in, a job to read each one." },
  { template: "blank", name: "Blank", about: "One page to start from." },
];

// me: the signed-in person; fragments: their list (name, role, kind,
// title, sharing); computer: theirs, with its agents; agents: by identity;
// previews: each chat's newest message
const state = { me: null, fragments: [], computer: null, defaultImage: null, agents: new Map(), previews: new Map(), current: store.get(CURRENT, null), frames: new Map(), page: null };

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// ---- the API, as this page calls it: the person's platform session ----
class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
async function api(method, path, body) {
  const headers = { "x-fragment-shell": "1" };
  if (body !== undefined) headers["content-type"] = "application/json";
  const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin" });
  const out = await r.json().catch(() => ({}));
  if (!r.ok) throw new ApiError(r.status, out.error, out.message || r.statusText);
  return out;
}

const byName = (name) => state.fragments.find((f) => f.name === name);
const labelOf = (name) => name.split(".")[0];
const titleOf = (name) => byName(name)?.title || labelOf(name);
const own = (f) => f.role === "owner";
const chats = () => state.fragments.filter((f) => f.kind === "chat");
const apps = () => state.fragments.filter((f) => f.kind === "app" || f.kind === "brain");
// a frame of one of the person's fragments, signed in there by the platform
const framed = (name, path = "/") => `/auth/frame?name=${encodeURIComponent(name)}&return=${encodeURIComponent(path)}`;
const colorOf = (name) => {
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return COLORS[h % COLORS.length];
};
// a chat's face: its title and its lead agent's colour (from its name, as
// the agent's own page chooses it)
const identity = (name) => ({ title: titleOf(name), color: colorOf(titleOf(name)), preview: state.previews.get(name) ?? "" });

function notice(title, text, ...actions) {
  const n = $("notice");
  n.replaceChildren(el("strong", null, title));
  if (text) n.append(el("span", null, text));
  for (const a of actions) n.append(" ", a);
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
    for (const row of document.querySelectorAll("#apps .row[data-key]")) row.classList.toggle("open", keys.includes(row.dataset.key));
  },
});
$("toggle-left").onclick = () => layout.toggle("left");
$("toggle-right").onclick = () => layout.toggle("right");
$("scrim").onclick = () => { layout.hide("left"); layout.hide("right"); };
addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || e.code !== "KeyB") return;
  e.preventDefault();
  layout.toggle(e.altKey ? "right" : "left");
});
const leaveSidebar = () => { if (layout.narrow) layout.hide("left"); };
function show(spec) {
  viewer.open(spec);
  layout.show("right");
}

// A frame of `name`. One still on this origin once it loads is the mint's
// refusal (a fragment this person may not open here): it says so instead.
function frameOf(src, title) {
  const frame = el("iframe");
  frame.title = title;
  frame.src = src;
  frame.allow = "clipboard-write; microphone";
  frame.addEventListener("load", () => {
    let path = null;
    try {
      path = frame.contentDocument?.location.pathname ?? null;
    } catch {}
    // a cross-origin frame (the fragment's own) is out of reach: it showed
    if (path?.startsWith("/auth/frame")) frame.classList.add("refused");
    theme(frame);
  });
  return frame;
}
// The frames follow this page's light or dark (they may not see its media).
const dark = matchMedia("(prefers-color-scheme: dark)");
function theme(frame) {
  try {
    frame.contentWindow?.postMessage({ fragment: "theme", mode: dark.matches ? "dark" : "light" }, "*");
  } catch {}
}
dark.addEventListener("change", () => { for (const f of document.querySelectorAll("iframe")) theme(f); });

// ---- sharing: the platform's own sheet, framed (it is this origin's) ----
function badges(f) {
  const out = [];
  const { guests = 0, visibility } = f.sharing ?? {};
  if (guests > 0) {
    const b = el("span", "shared");
    b.innerHTML = svg("people");
    b.append(String(guests));
    b.title = `Shared with ${guests} ${guests === 1 ? "person" : "people"}`;
    out.push(b);
  }
  if (visibility === "public") {
    const b = el("span", "shared");
    b.innerHTML = svg("globe");
    b.title = "Anyone can open it";
    out.push(b);
  }
  return out;
}
const sheet = $("sheet");
function share(name) {
  const frame = el("iframe");
  frame.title = `Share ${titleOf(name)}`;
  frame.allow = "clipboard-write";
  frame.src = `/share/${encodeURIComponent(name)}`;
  sheet.replaceChildren(frame);
  sheet.showModal();
}
addEventListener("message", (e) => {
  const frame = sheet.querySelector("iframe");
  if (!sheet.open || e.source !== frame?.contentWindow || e.origin !== location.origin) return;
  if (Number.isFinite(e.data?.height)) frame.style.height = `${e.data.height}px`;
  if (e.data?.share === "done") sheet.close();
});
sheet.onclick = (e) => { if (e.target === sheet) sheet.close(); };
sheet.onclose = () => { sheet.replaceChildren(); load().catch(() => {}); };

// ---- one menu, for whichever button asked ----
const menu = $("menu");
let menuFor = null;
function closeMenu() {
  if (!menuFor) return;
  menuFor.button.setAttribute("aria-expanded", "false");
  menuFor = null;
  menu.hidden = true;
}
function openMenu(button, items) {
  const again = menuFor?.button === button;
  closeMenu();
  if (again) return;
  menuFor = { button };
  menu.replaceChildren(...items.map((it) => {
    if (it.heading) return el("div", "menu-heading", it.heading);
    const b = el("button");
    b.type = "button";
    b.setAttribute("role", "menuitem");
    b.innerHTML = svg(it.icon);
    b.append(it.text);
    b.onclick = () => { closeMenu(); it.onClick(); };
    return b;
  }));
  button.setAttribute("aria-expanded", "true");
  menu.hidden = false;
  const r = button.getBoundingClientRect();
  menu.style.top = `${Math.min(r.bottom + 4, innerHeight - menu.offsetHeight - 8)}px`;
  menu.style.left = `${Math.max(8, Math.min(r.left, innerWidth - menu.offsetWidth - 8))}px`;
  menu.querySelector("button")?.focus();
}
addEventListener("pointerdown", (e) => { if (menuFor && !menu.contains(e.target) && !menuFor.button.contains(e.target)) closeMenu(); });
addEventListener("keydown", (e) => {
  if (!menuFor) return;
  if (e.key === "Escape") {
    const trigger = menuFor.button;
    closeMenu();
    trigger.focus();
    e.preventDefault();
  } else if (["ArrowDown", "ArrowUp"].includes(e.key)) {
    const buttons = [...menu.querySelectorAll("button")];
    const i = buttons.indexOf(document.activeElement);
    buttons[(i + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus();
    e.preventDefault();
  } else if (e.key === "Tab") closeMenu();
});
addEventListener("blur", closeMenu);
addEventListener("resize", closeMenu);

// ---- agents: each a chat, the open one in the middle column ----
function renderHeading() {
  const open = state.current && byName(state.current) && !state.page;
  $("agent-heading").hidden = !open;
  $("page-title").hidden = !state.page;
  $("page-title").textContent = state.page ?? "";
  if (!open) return;
  const who = identity(state.current);
  $("chat-title").textContent = who.title;
  $("agent-mark").replaceChildren(avatar(who, "small"));
  const frame = state.frames.get(state.current);
  if (frame) frame.title = who.title;
}
function renderChats() {
  const list = chats();
  $("chats").replaceChildren(...(list.length ? list.map((f) => {
    const who = identity(f.name);
    const row = el("button", `row agent-row${f.name === state.current && !state.page ? " active" : ""}`);
    row.type = "button";
    row.dataset.key = `chat:${f.name}`;
    row.setAttribute("aria-current", f.name === state.current && !state.page ? "page" : "false");
    const text = el("span", "agent-copy");
    text.append(el("span", "label", who.title), el("span", "agent-preview", who.preview || (own(f) ? "Say hello" : f.role)));
    row.append(avatar(who), text, ...badges(f));
    row.onclick = () => { openChat(f.name); leaveSidebar(); };
    return row;
  }) : [el("div", "empty-row", "Your first agent starts here")]));
}
$("agent-heading").onclick = () => {
  const name = state.current;
  const agent = agentOfChat(name);
  openMenu($("agent-heading"), [
    { icon: "rename", text: "Rename…", onClick: () => rename(name) },
    { icon: "invite", text: "Invite…", onClick: () => share(name) },
    ...(agent ? [{ icon: "agent", text: "Its profile", onClick: () => openApp(agent) }] : []),
    ...(state.computer ? [{ icon: "screen", text: "Its computer's screen", onClick: () => openScreen() }] : []),
  ]);
};
// the agent fragment a chat is with: the one whose title the chat carries
function agentOfChat(name) {
  const title = titleOf(name);
  return [...state.agents.values()].find((a) => titleOf(a.fragment) === title)?.fragment ?? null;
}

function openChat(name) {
  if (!byName(name)) return;
  state.page = null;
  $("settings-page").hidden = true;
  $("frames").hidden = false;
  state.current = name;
  store.set(CURRENT, name);
  $("notice").hidden = true;
  if (!state.frames.has(name)) {
    const frame = frameOf(framed(name), titleOf(name));
    frame.dataset.fragment = name;
    $("frames").append(frame);
    state.frames.set(name, frame);
  }
  for (const [n, frame] of state.frames) frame.hidden = n !== name;
  renderHeading();
  renderChats();
  prewake();
}

// ---- apps: every other fragment, each a window in the viewer ----
function paneIcon(name) {
  const i = el("span", "pane-icon");
  i.innerHTML = svg(name);
  return i;
}
const iconOf = (name) => appIcon(labelOf(name));
function renderApps() {
  const list = apps();
  $("apps").replaceChildren(...(list.length ? list.map((f) => {
    const row = el("button", `row${viewer.keys.includes(`app:${f.name}`) ? " open" : ""}`);
    row.type = "button";
    row.dataset.key = `app:${f.name}`;
    row.append(iconOf(f.name), el("span", "label", titleOf(f.name)), ...badges(f));
    if (!own(f)) row.append(el("span", "meta", f.role));
    row.onclick = () => { openApp(f.name); leaveSidebar(); };
    return row;
  }) : [el("div", "empty-row", "No apps yet")]));
}
function openApp(name) {
  const f = byName(name);
  if (!f) return;
  const frame = frameOf(framed(name), titleOf(name));
  const status = el("span", "pane-sharing");
  status.append(...badges(f));
  show({
    key: `app:${name}`, title: titleOf(name), subtitle: own(f) ? undefined : f.role, icon: iconOf(name), body: frame, status,
    actions: [
      ...(own(f) ? [{ icon: ICON.share, title: "Share…", onClick: () => share(name) }] : []),
      { icon: ICON.folder, title: "Files", onClick: () => show({ key: `tree:${name}`, title: titleOf(name), subtitle: "files", icon: paneIcon("folder"), body: frameOf(framed(name, "/__files"), `${titleOf(name)} files`) }) },
      { icon: ICON.reload, title: "Reload", onClick: () => { frame.src = framed(name); } },
    ],
  });
}
// a fragment's file viewer asks for a file as a window (only a frame of one
// of the person's own fragments, for a file of that same origin)
addEventListener("message", (e) => {
  const ask = e.data;
  if (ask?.fragment !== "open" || typeof ask.url !== "string") return;
  const from = [...document.querySelectorAll("iframe")].find((f) => f.contentWindow === e.source);
  if (!from || new URL(ask.url, e.origin).origin !== e.origin) return;
  const url = new URL(ask.url, e.origin);
  const path = url.searchParams.get("path") || String(ask.title || "");
  const frame = el("iframe");
  frame.src = url.href;
  frame.title = path;
  show({ key: `file:${url.href}`, title: path.split("/").pop(), icon: paneIcon("folder"), body: frame });
});

// ---- its computer's screen: a port of its own origin, through a ticket ----
async function openScreen() {
  if (!state.computer) return;
  try {
    const t = await api("POST", `/api/computers/${encodeURIComponent(state.computer.computer)}/ports/6080/ticket`, {});
    const frame = frameOf(t.url, "Its computer's screen");
    show({ key: "screen", title: "Computer", subtitle: "screen", icon: paneIcon("screen"), body: frame, persist: false });
  } catch (e) {
    notice("Its screen did not open", e.message);
  }
}

// ---- making an agent: its fragment, its computer, its chat (decision 10, 16) ----
const slug = (s) => s.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 32) || "agent";
function freeLabel(base) {
  const taken = new Set(state.fragments.map((f) => labelOf(f.name)));
  for (let i = 1; ; i++) {
    const l = i === 1 ? base : `${base}-${i}`;
    if (!taken.has(l) && !taken.has(`${l}-chat`)) return l;
  }
}
function pickName() {
  const taken = new Set(state.fragments.map((f) => titleOf(f.name)));
  return NAMES.find((n) => !taken.has(n)) ?? `Agent ${state.fragments.length + 1}`;
}
// Through public APIs only: the agent fragment (the agent template, its job
// in SOUL.md), assigned to the person's computer; a chat with it (the chat
// template), the agent added (the platform wakes its computer); and the job
// as the person's first message, which the agent answers.
async function makeAgent(job, chosen) {
  const name = chosen?.trim() || pickName();
  const label = freeLabel(slug(name));
  const agent = await api("POST", "/api/fragments", { name: label, template: "agent", title: name });
  await api("POST", `/api/f/${agent.name}/files`, {
    key: "agent-job",
    message: "its job",
    files: [{ path: "SOUL.md", text: `${job.trim()}\n` }, { path: "agent.json", text: JSON.stringify({ tier: "medium", color: colorOf(name) }, null, 2) + "\n" }],
  });
  await api("POST", `/api/f/${agent.name}/deploy`, {});
  const computer = await api("POST", "/api/computers", {});
  const assigned = await api("PUT", `/api/computers/${encodeURIComponent(computer.computer)}/agents/${encodeURIComponent(agent.name)}`, {});
  const id = assigned.agents.find((a) => a.fragment === agent.name)?.identity;
  const chat = await api("POST", "/api/fragments", { name: `${label}-chat`, template: "chat", title: name });
  if (id) await api("PUT", `/api/f/${chat.name}/members/${encodeURIComponent(id)}`, { role: "editor" });
  await api("POST", `/api/f/${chat.name}/channels/chat`, { id: "job", body: { text: job.trim() } });
  // a wake now hides the start's latency: the agent is up as the page opens
  api("POST", `/api/computers/${encodeURIComponent(computer.computer)}/wake`, {}).catch(() => {});
  return chat.name;
}
const dialog = $("new-agent-dialog");
function newAgent() {
  $("new-agent-form").reset();
  $("new-agent-error").hidden = true;
  dialog.showModal();
  $("new-agent-job").focus();
  prewake();
}
$("new-agent").onclick = newAgent;
$("new-agent-top").onclick = newAgent;
$("new-agent-cancel").onclick = () => dialog.close();
$("new-agent-form").onsubmit = async (e) => {
  e.preventDefault();
  const go = $("new-agent-go");
  go.disabled = true;
  go.textContent = "Making it…";
  try {
    const chat = await makeAgent($("new-agent-job").value, $("new-agent-name").value);
    dialog.close();
    await load();
    openChat(chat);
  } catch (err) {
    $("new-agent-error").textContent = err.message;
    $("new-agent-error").hidden = false;
  } finally {
    go.disabled = false;
    go.textContent = "Make it";
  }
};

// ---- renaming: the chat's title (and its agent's, so they stay one) ----
let renaming = null;
function rename(name) {
  renaming = name;
  $("agent-name").value = titleOf(name);
  $("rename-error").hidden = true;
  $("rename-agent").showModal();
  $("agent-name").select();
}
$("rename-cancel").onclick = () => $("rename-agent").close();
$("rename-form").onsubmit = async (e) => {
  e.preventDefault();
  const title = $("agent-name").value.trim();
  if (!title || !renaming) return;
  try {
    for (const name of [renaming, agentOfChat(renaming)].filter(Boolean)) {
      const manifest = await api("GET", `/api/f/${name}/manifest`);
      manifest.meta = { ...(manifest.meta ?? {}), title };
      await api("POST", `/api/f/${name}/files`, { message: "rename", files: [{ path: "fragment.json", text: JSON.stringify(manifest, null, 2) + "\n" }] });
      await api("POST", `/api/f/${name}/deploy`, {});
    }
    $("rename-agent").close();
    await load();
  } catch (err) {
    $("rename-error").textContent = err.message;
    $("rename-error").hidden = false;
  }
};

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
    card.append(appIcon(t.template), el("strong", null, t.name), el("p", null, t.about), input, add, error);
    card.onsubmit = async (e) => {
      e.preventDefault();
      add.disabled = true;
      error.textContent = "";
      try {
        const made = await api("POST", "/api/fragments", { name: input.value.trim() || freeLabel(t.template), template: t.template });
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

// ---- search: the person's chats and apps ----
function renderSearchResults() {
  const query = $("workspace-search").value.trim().toLocaleLowerCase();
  const hit = (f) => `${titleOf(f.name)} ${state.previews.get(f.name) ?? ""} ${f.name}`.toLocaleLowerCase().includes(query);
  const results = $("search-results");
  results.replaceChildren();
  let count = 0;
  for (const [heading, list, chat] of [["Agents", chats().filter(hit), true], ["Apps", apps().filter(hit), false]]) {
    if (!list.length) continue;
    results.append(el("h3", null, heading));
    for (const f of list) {
      count++;
      const row = el("button", "row search-result");
      row.type = "button";
      const text = el("span", "agent-copy");
      text.append(el("span", "label", titleOf(f.name)));
      if (chat) text.append(el("span", "agent-preview", state.previews.get(f.name) ?? ""));
      row.append(chat ? avatar(identity(f.name), "small") : iconOf(f.name), text);
      row.onclick = () => {
        $("search-dialog").close();
        if (chat) openChat(f.name); else openApp(f.name);
        leaveSidebar();
      };
      results.append(row);
    }
  }
  $("search-status").textContent = count ? `${count} result${count === 1 ? "" : "s"}` : "Nothing found.";
}
$("search-agents").onclick = () => {
  $("workspace-search").value = "";
  renderSearchResults();
  $("search-dialog").showModal();
  $("search-agents").setAttribute("aria-expanded", "true");
  $("workspace-search").focus();
};
$("search-close").onclick = () => $("search-dialog").close();
$("search-dialog").addEventListener("close", () => $("search-agents").setAttribute("aria-expanded", "false"));
$("workspace-search").addEventListener("input", renderSearchResults);
$("workspace-search").addEventListener("keydown", (e) => {
  const first = $("search-results").querySelector("button");
  if (e.key === "ArrowDown" && first) { e.preventDefault(); first.focus(); }
  if (e.key === "Enter" && first) { e.preventDefault(); first.click(); }
});

// ---- the computer: awake while the person is here, and its updates ----
let woke = 0;
// asking for a wake as the person arrives, focuses a chat, or starts a new
// agent hides the computer's start (decision 39's pre-wake, from the shell)
function prewake() {
  const c = state.computer;
  if (!c || document.hidden || Date.now() - woke < 60_000 || c.phase === "awake" || c.phase === "starting") return;
  woke = Date.now();
  api("POST", `/api/computers/${encodeURIComponent(c.computer)}/wake`, {}).then((v) => { state.computer = v; }).catch(() => {});
}
document.addEventListener("visibilitychange", prewake);
function renderUpdate() {
  const c = state.computer;
  const due = !!(c && state.defaultImage && c.image !== state.defaultImage);
  $("update").hidden = !due;
}
$("update-pill").onclick = () => { $("update-confirm").hidden = false; $("update-pill").hidden = true; };
$("update-cancel").onclick = () => { $("update-confirm").hidden = true; $("update-pill").hidden = false; };
// an update is the person's to start (a new version applies at a wake):
// pinned to the default, it sleeps and wakes on the new image
$("update-go").onclick = async () => {
  const c = state.computer;
  $("update-go").disabled = true;
  try {
    await api("PUT", `/api/computers/${encodeURIComponent(c.computer)}/image`, { image: state.defaultImage });
    await api("POST", `/api/computers/${encodeURIComponent(c.computer)}/sleep`, {});
    state.computer = await api("POST", `/api/computers/${encodeURIComponent(c.computer)}/wake`, {});
    $("update-confirm").hidden = true;
    $("update-pill").hidden = false;
    renderUpdate();
  } catch (e) {
    $("update-text").textContent = `The update did not finish: ${e.message}`;
  } finally {
    $("update-go").disabled = false;
  }
};

// ---- settings: the person's account, credit, computer and agents ----
const usd = (micros) => (micros / 1_000_000).toLocaleString(undefined, { style: "currency", currency: "USD" });
function section(title, ...children) {
  const s = el("section", "settings-section");
  s.append(el("h2", null, title), ...children);
  return s;
}
function line(label, value) {
  const p = el("p", "settings-line");
  p.append(el("span", "settings-key", label), el("span", "settings-value", value));
  return p;
}
async function openSettings() {
  state.page = "Settings";
  $("frames").hidden = true;
  const page = $("settings-page");
  page.hidden = false;
  page.replaceChildren(el("p", "muted", "Loading…"));
  renderHeading();
  renderChats();
  leaveSidebar();
  const [ledger, linked] = await Promise.all([api("GET", "/api/ledger").catch(() => null), api("GET", "/api/connections").catch(() => null)]);
  const account = section("Account", line("Username", `@${state.me.username}`), line("Signed in as", state.me.email ?? "—"));
  const signout = el("form");
  signout.method = "post";
  signout.action = "/auth/logout";
  const out = el("button", "quiet", "Sign out");
  out.type = "submit";
  signout.append(out);
  account.append(signout);
  const credit = ledger
    ? section(
        "Credit",
        line("Plan", ledger.plan === "seat_always_on" ? "Always-on seat" : ledger.plan === "seat" ? "Seat" : "Guest"),
        line("This month", `${usd(ledger.availableMicros ?? ledger.balanceMicros ?? 0)} left`),
        ...(ledger.standing?.standing && ledger.standing.standing !== "ok" ? [el("p", "settings-warning", ledger.standing.why ? `Your agents are stopped: ${ledger.standing.why}` : "Your agents are stopped.")] : []),
      )
    : section("Credit", el("p", "muted", "Your credit could not be read."));
  const c = state.computer;
  const computer = section(
    "Computer",
    ...(c
      ? [line("State", c.phase.replace("_", " ")), line("Version", c.image), ...(c.why ? [el("p", "settings-warning", c.why)] : [])]
      : [el("p", "muted", "Your computer starts with your first agent.")]),
  );
  if (c) {
    const screen = el("button", "quiet", "Open its screen");
    screen.type = "button";
    screen.onclick = openScreen;
    computer.append(screen);
  }
  const agents = section("Agents");
  const mine = [...state.agents.values()];
  agents.append(...(mine.length ? mine.map((a) => {
    const row = el("button", "row");
    row.type = "button";
    row.append(avatar({ color: colorOf(titleOf(a.fragment)) }, "small"), el("span", "label", titleOf(a.fragment)));
    row.onclick = () => openApp(a.fragment);
    return row;
  }) : [el("p", "muted", "None yet.")]));
  // connections (decision 22): the person's accounts their agents use,
  // through WorkOS; connecting one opens its consent in a window
  const connections = section("Connections");
  const offered = linked?.connections ?? [];
  connections.append(...(offered.length ? offered.map((c) => {
    const row = el("p", "settings-line");
    const name = c.provider.replace(/(^|-)([a-z])/g, (_, d, l) => `${d ? " " : ""}${l.toUpperCase()}`);
    row.append(el("span", "settings-key", name));
    if (c.status === "connected") row.append(el("span", "settings-value", "Connected"));
    else {
      const go = el("button", "quiet", c.status === "expired" ? "Connect again" : "Connect");
      go.type = "button";
      go.onclick = async () => {
        go.disabled = true;
        try {
          const { url } = await api("POST", `/api/connections/${encodeURIComponent(c.provider)}/authorize`, {});
          window.open(url, "_blank", "popup,width=520,height=720");
          // back from the provider: the section says so
          addEventListener("focus", () => openSettings().catch(() => {}), { once: true });
        } catch (e) {
          go.textContent = e.message;
        } finally {
          go.disabled = false;
        }
      };
      row.append(go);
    }
    return row;
  }) : [el("p", "muted", linked ? "This platform offers none yet." : "Your connections could not be read.")]));
  const cli = section("The command line", el("p", null, "Your agents and you can publish apps from a terminal: install the `fragment` CLI and run `fragment login`; it opens this platform to approve its key."));
  const pair = el("a", "quiet", "Approve a CLI");
  pair.href = "/cli";
  cli.append(pair);
  page.replaceChildren(account, credit, computer, agents, connections, cli);
}
$("settings").onclick = () => openSettings().catch((e) => notice("Settings did not open", e.message));

// ---- first run: sign in, a username, the first agent (decision 10) ----
function firstRun(...children) {
  $("layout").hidden = true;
  $("first-run").hidden = false;
  $("first-run-card").replaceChildren(el("div", "brand", "Finite.Computer"), ...children);
}
function signIn() {
  const go = el("a", "primary", "Sign in");
  go.href = `/auth/login?return=${encodeURIComponent(location.pathname)}`;
  firstRun(el("h1", null, "Agents that work for you, and the apps they make."), el("p", "muted", "Sign in to start."), go);
}
function chooseUsername() {
  const form = el("form", "first-run-form");
  const input = el("input");
  input.name = "username";
  input.required = true;
  input.autocomplete = "username";
  input.placeholder = "yourname";
  input.pattern = "[a-z0-9]([a-z0-9]|-(?=[a-z0-9])){2,31}";
  input.maxLength = 32;
  const error = el("p", "form-error");
  error.hidden = true;
  const go = el("button", "primary", "Continue");
  go.type = "submit";
  form.append(el("label", null, "Choose a username"), input, el("p", "muted", "Your apps live at addresses with it; it can't change later."), error, go);
  form.onsubmit = async (e) => {
    e.preventDefault();
    go.disabled = true;
    try {
      await api("PUT", "/api/identities/me/username", { username: input.value.trim() });
      await start();
    } catch (err) {
      error.textContent = err.message;
      error.hidden = false;
    } finally {
      go.disabled = false;
    }
  };
  firstRun(el("h1", null, "Welcome."), form);
  input.focus();
}
function firstAgent() {
  const form = el("form", "first-run-form");
  const job = el("textarea");
  job.name = "job";
  job.rows = 4;
  job.required = true;
  job.maxLength = 4000;
  job.placeholder = "Help me plan a garden: what to plant, and when.";
  const error = el("p", "form-error");
  error.hidden = true;
  const go = el("button", "primary", "Start");
  go.type = "submit";
  form.append(job, error, go);
  form.onsubmit = async (e) => {
    e.preventDefault();
    go.disabled = true;
    go.textContent = "Starting…";
    try {
      await load();
      const chat = await makeAgent(job.value, null);
      await start(chat);
    } catch (err) {
      error.textContent = err.message;
      error.hidden = false;
      go.disabled = false;
      go.textContent = "Start";
    }
  };
  firstRun(el("h1", null, "What should your first agent do?"), el("p", "muted", "It takes the job, picks a name, and says hello while its computer starts."), form);
  job.focus();
}

// ---- the person's things, read again whenever they may have changed ----
async function load() {
  const [list, computers] = await Promise.all([api("GET", "/api/fragments"), api("GET", "/api/computers").catch(() => ({ computers: [] }))]);
  state.fragments = list.fragments ?? [];
  state.computer = computers.computers?.[0] ?? null;
  state.defaultImage = computers.defaultImage ?? null;
  state.agents = new Map((state.computer?.agents ?? []).map((a) => [a.identity, a]));
  renderChats();
  renderApps();
  renderHeading();
  renderUpdate();
  previews().catch(() => {});
}
// each chat's newest message, for its row
async function previews() {
  await Promise.all(chats().map(async (f) => {
    const listed = await api("GET", `/api/f/${f.name}/channels`);
    const seq = listed.channels?.find((c) => c.name === "chat")?.seq ?? 0;
    if (!seq) return;
    const read = await api("GET", `/api/f/${f.name}/channels/chat?after=${Math.max(0, seq - 1)}&limit=1`);
    const text = read.records?.[0]?.body?.text;
    if (typeof text === "string") state.previews.set(f.name, text.split("\n")[0].slice(0, 80));
  }));
  renderChats();
}

async function start(open) {
  let me;
  try {
    me = await api("GET", "/api/identities/me");
  } catch (e) {
    if (e.status === 401) return signIn();
    throw e;
  }
  state.me = me;
  if (!me.username) return chooseUsername();
  await load();
  if (!chats().length && !open) return firstAgent();
  $("first-run").hidden = true;
  $("layout").hidden = false;
  const pick = open ?? (byName(state.current) ? state.current : chats()[0]?.name);
  if (pick) openChat(pick);
  else notice("No chats yet", "Make an agent to start.");
  prewake();
}

start().catch((e) => {
  $("layout").hidden = false;
  notice("This page did not load", e.message);
});
