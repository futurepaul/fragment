// The shell: the platform's one page (docs/cloudflare-v1.md, decisions
// 6–12). Its sidebar is the person's fragments by kind: their chats (a
// direct chat with one agent, in its colour, or a group of several, their
// colours stacked) and their apps, less the ones they archived. The open
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
import { billingSections, returning } from "./billing.js";

const $ = (id) => document.getElementById(id);
// New agents run on GLM-5.3 Flash (Paul, 2026-10-08).
const DEFAULT_AGENT_TIER = "cheap";
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
  archive: '<rect x="2" y="3" width="20" height="5" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8M10 12h4"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;
// Skyler's agent palette: a plain colored circle, no eyes
const COLORS = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];
// A new agent's name, when its maker names none (Paul, 2026-10-03): the
// beige boxes of an alternate 1990s, never a real one's.
const NAMES = ["XBT-2000", "Starfire 40K", "Turbo Quasar 486", "Novatron DX", "Hyperion 9000", "Cobalt Prism 66", "Megastation LX", "Orbitron 3D", "Datastar Pro", "Pulsar 360", "Zephyr XL", "Titan MX", "Vortex 7", "Nimbus 4K", "Galaxion SE", "Powerframe 99"];
const CURRENT = "shell.chat.v1";
// the templates an app starts from (the platform's catalog: publish.rs),
// each made with a title: the name its maker gives it, or the template's
const CATALOG = [
  { template: "todo", name: "Todo", about: "A list, live for everyone who has it open." },
  { template: "inbox", name: "Inbox", about: "Webhooks in, a job to read each one." },
  { template: "blank", name: "Blank", about: "One page to start from." },
  { template: "brain", name: "Brain", about: "A knowledge base your agents keep and search." },
];

// me: the signed-in person; fragments: their list (name, role, kind,
// title, agents, preview, sharing, archived: a chat's agents the lead
// first, and its newest message); computer: theirs, with its agents;
// agents: by identity
const state = { me: null, fragments: [], computer: null, defaultImage: null, agents: new Map(), current: store.get(CURRENT, null), frames: new Map(), page: null };

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
// a name is its label and a random suffix (decision 47): `todo--k3x9`
const labelOf = (name) => name.replace(/--[a-km-np-z2-9]{4}$/, "");
const titleOf = (name) => byName(name)?.title || labelOf(name);
const own = (f) => f.role === "owner";
const chats = () => state.fragments.filter((f) => f.kind === "chat");
const apps = () => state.fragments.filter((f) => f.kind === "app" || f.kind === "brain");
// the sidebar's: what the person has not archived (search still finds the rest)
const shown = (list) => list.filter((f) => !f.archived);
// a frame of one of the person's fragments, signed in there by the platform
// A path segment: the API's ids keep their ":" (`computer:…`) as
// sent, since the router matches segments as they are, not decoded.
const seg = (s) => encodeURIComponent(s).replace(/%3A/gi, ":");
const framed = (name, path = "/") => `/auth/frame?name=${encodeURIComponent(name)}&return=${encodeURIComponent(path)}`;
// An agent's colour, chosen by its identity as the chat's page chooses it
// (templates/chat: FNV-1a over the id), so the two always agree.
const colorOf = (id) => {
  if (!id) return COLORS[0];
  let h = 0x811c9dc5;
  for (let i = 0; i < id.length; i++) h = Math.imul(h ^ id.charCodeAt(i), 0x01000193) >>> 0;
  return COLORS[h % COLORS.length];
};
// The identity of the agent a fragment is (`maple--k3x9`), or of a direct
// chat's (`maple-chat--p2m4`, labeled for it), while its row names no agents.
const agentOf = (name) => {
  const direct = labelOf(name).match(/^(.+)-chat$/)?.[1];
  return [...state.agents.values()].find((a) => a.fragment === name || labelOf(a.fragment) === direct)?.identity ?? null;
};
// A chat's agents, the lead first, as its row names them; a direct chat's
// agent while it names none.
const agentsOf = (name) => byName(name)?.agents ?? [agentOf(name)].filter(Boolean);
const isGroup = (name) => agentsOf(name).length >= 2;
// a chat's face: its title, its lead agent's colour, and its agents' colours
const identity = (name) => {
  const agents = agentsOf(name);
  return { title: titleOf(name), color: colorOf(agents[0] ?? null), colors: agents.map(colorOf), preview: byName(name)?.preview ?? "" };
};
// A chat's mark: its agent's avatar, or a group's agents' stacked (at most
// three, the lead first), coloured by identity as the chat's page does.
function mark(name, size = "") {
  const who = identity(name);
  if (!isGroup(name)) return avatar(who, size);
  const stack = el("span", `avatar-stack ${size}`);
  const colors = who.colors.slice(0, 3);
  stack.dataset.count = String(colors.length);
  stack.setAttribute("aria-hidden", "true");
  stack.append(...colors.map((color) => avatar({ color })));
  return stack;
}

// A list's rows, put in place (`data-key` names each): a row the same as
// one shown keeps that element (its focus, its card, the scroll around
// it), a new one goes where it belongs, and one no longer listed goes.
function patch(box, rows) {
  const shown = new Map([...box.children].filter((e) => e.dataset.key).map((e) => [e.dataset.key, e]));
  const next = rows.map((row) => {
    const was = shown.get(row.dataset.key);
    return was?.isEqualNode(row) ? was : row;
  });
  next.forEach((row, i) => {
    if (box.children[i] !== row) box.insertBefore(row, box.children[i] ?? null);
  });
  while (box.children.length > next.length) box.lastElementChild.remove();
}

function notice(title, text) {
  const n = $("notice");
  n.replaceChildren(el("strong", null, title));
  if (text) n.append(el("span", null, text));
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

// A frame of `src` (the mint's, for fragment `name`; a port's ticket).
function frameOf(src, title, name = null) {
  const frame = el("iframe");
  frame.title = title;
  frame.src = src;
  frame.allow = "clipboard-write; microphone";
  if (name) frame.dataset.fragment = name;
  frame.dataset.src = src;
  frame.addEventListener("load", () => theme(frame));
  return frame;
}

// A frame that could not sign in says so, and how on (the mint's note,
// auth.rs `frame_note`, or a fragment's page whose browser keeps its frame
// cookie from it): in this page's look, in the frame's place, tried again
// when the person comes back from the tab it sent them to.
const BLOCKED = {
  consent: (t) => [`Open ${t} to continue`, "It isn't yours, and no one shared it with you: it learns who you are only once you say so, in a tab of its own.", "Open it in a tab", "_blank"],
  cookies: (t) => [`${t} can't sign in here`, "This browser keeps a window's own sign-in from it. Open it in a tab instead.", "Open it in a tab", "_blank"],
  "signed-out": (t) => [`Sign in to see ${t}`, "You're signed out.", "Sign in", "_top"],
};
const html = (t) => String(t).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
addEventListener("message", (event) => {
  const d = event.data;
  if (d?.fragment !== "signin-blocked" || !Object.hasOwn(BLOCKED, d.why)) return;
  const frame = [...document.querySelectorAll("iframe[data-fragment]")].find((f) => f.contentWindow === event.source);
  if (!frame || frame.dataset.fragment !== d.name) return;
  const [title, text, go, target] = BLOCKED[d.why](titleOf(d.name));
  const href = d.why === "signed-out" ? `/auth/login?return=${encodeURIComponent(location.pathname)}` : `/auth/fragment?name=${encodeURIComponent(d.name)}&return=/`;
  frame.dataset.blocked = d.why;
  frame.srcdoc = `<!doctype html><link rel="stylesheet" href="/__shell/shell.css"><body class="frame-note"><div class="notice"><strong>${html(title)}</strong><span>${html(text)}</span><a class="allow" href="${html(href)}" target="${target}" rel="noopener">${html(go)}</a></div></body>`;
});
addEventListener("focus", () => {
  for (const frame of document.querySelectorAll("iframe[data-blocked]")) {
    delete frame.dataset.blocked;
    frame.removeAttribute("srcdoc");
    frame.src = frame.dataset.src;
  }
});
// The frames follow this page's light or dark (they may not see its media).
const dark = matchMedia("(prefers-color-scheme: dark)");
function theme(frame) {
  try {
    frame.contentWindow?.postMessage({ fragment: "theme", mode: dark.matches ? "dark" : "light" }, "*");
  } catch {}
}
dark.addEventListener("change", () => { for (const f of document.querySelectorAll("iframe")) theme(f); });

// ---- the person's agents, for a page of theirs that asks (a chat's @) ----
// A frame of a fragment the person owns may ask for their agents
// (`{fragment: "agents?"}`); the shell answers it, at that fragment's own
// origin only (its status's canonical URL), `{fragment: "agents", agents:
// [{identity, fragment, name, title}]}`, and again whenever they change.
// It may then ask for one of them in its fragment (`{fragment: "add-agent",
// identity, nonce}`): the shell asks its person, in its own dialog (never
// in the frame: a page is code its author or an agent wrote, so it asks
// and never grants), "Add Fred to <title>?", and only on their Add adds that
// agent as an editor, as making a chat does (decision 36: an owner shares
// their own fragment with their own agent). It answers `{fragment:
// "agent-added", nonce, identity, ok, error?}`: `error` "declined" on
// Cancel, "not answered" after ADD_CONFIRM_MS, "busy" while another ask is
// open. Nothing is remembered: every add is asked. A frame of a fragment
// the person does not own (shared with them) learns nothing and adds no
// one, and no page adds anyone but the person's own agents. It names no
// template: any page of theirs may use it.
const ADD_CONFIRM_MS = 90_000;
// Add arms this long after the dialog shows, so the click or key that sent
// the page's message cannot confirm it (the share sheet's 800 ms).
const ADD_ARM_MS = 800;
const rosterTo = new Map(); // a frame's window -> its fragment's origin
const origins = new Map(); // fragment name -> its origin (a promise)
function originOf(name) {
  if (!origins.has(name)) {
    const asked = api("GET", `/api/f/${seg(name)}/status`).then((s) => new URL(s.urls.canonical).origin);
    asked.catch(() => origins.delete(name));
    origins.set(name, asked);
  }
  return origins.get(name);
}
const roster = () => [...state.agents.values()].map((a) => ({ identity: a.identity, fragment: a.fragment, name: a.name || labelOf(a.fragment), title: titleOf(a.fragment) }));
let rosterSent = "";
function rosterChanged() {
  const agents = roster();
  const now = JSON.stringify(agents);
  if (now === rosterSent) return;
  rosterSent = now;
  const live = new Set([...document.querySelectorAll("iframe[data-fragment]")].map((f) => f.contentWindow));
  for (const [win, origin] of rosterTo) {
    if (!live.has(win)) rosterTo.delete(win);
    else win.postMessage({ fragment: "agents", agents }, origin);
  }
}
addEventListener("message", async (event) => {
  const d = event.data;
  if (d?.fragment !== "agents?" && d?.fragment !== "add-agent") return;
  // the frame that sent it shows one of the person's own fragments, and
  // the page in it is that fragment's
  const frame = [...document.querySelectorAll("iframe[data-fragment]")].find((f) => f.contentWindow === event.source);
  const name = frame?.dataset.fragment;
  if (!name || byName(name)?.role !== "owner") return;
  let origin;
  try {
    origin = await originOf(name);
  } catch {
    return;
  }
  if (event.origin !== origin || frame.contentWindow !== event.source) return;
  if (d.fragment === "agents?") {
    rosterTo.set(event.source, origin);
    event.source.postMessage({ fragment: "agents", agents: roster() }, origin);
    return;
  }
  const asker = event.source;
  const answer = (ok, error) => asker.postMessage({ fragment: "agent-added", nonce: d.nonce, identity: d.identity, ok, ...(error ? { error } : {}) }, origin);
  if (typeof d.nonce !== "string" || d.nonce.length > 64) return;
  const agent = state.agents.get(d.identity);
  if (!agent) return answer(false, "that is not one of your agents");
  const said = await confirmAdd(agent, name);
  if (said !== "added") return answer(false, said);
  // the frame may have gone, or shown another page, while its person read
  if (frame.contentWindow !== asker || !frame.isConnected) return;
  try {
    await api("PUT", `/api/f/${seg(name)}/members/${seg(agent.identity)}`, { role: "editor" });
    answer(true);
  } catch (e) {
    answer(false, e.message);
  }
});
// The person's answer to one page's add, in the shell's own dialog:
// "added" on Add; "declined" on Cancel or Escape; "not answered" when left
// ADD_CONFIRM_MS. One at a time: another ask while it is open is "busy".
const addDialog = $("add-agent-dialog");
let adding = null; // { resolve, timer, arm, outcome }
function confirmAdd(agent, name) {
  if (adding) return Promise.resolve("busy");
  return new Promise((resolve) => {
    const who = titleOf(agent.fragment);
    $("add-agent-title").textContent = `Add ${who}?`;
    $("add-agent-text").textContent = `Add ${who} to ${titleOf(name)}? ${who} will be able to read and edit it.`;
    addDialog.dataset.agent = agent.identity;
    addDialog.dataset.fragment = name;
    $("add-agent-go").disabled = true;
    adding = {
      resolve,
      outcome: "declined",
      arm: setTimeout(() => { $("add-agent-go").disabled = false; }, ADD_ARM_MS),
      timer: setTimeout(() => {
        if (adding) adding.outcome = "not answered";
        addDialog.close();
      }, ADD_CONFIRM_MS),
    };
    addDialog.showModal();
    $("add-agent-cancel").focus();
  });
}
// whatever closes it (Add, Cancel, Escape, the timeout) settles the ask once
addDialog.addEventListener("close", () => {
  if (!adding) return;
  const { resolve, timer, arm, outcome } = adding;
  adding = null;
  clearTimeout(timer);
  clearTimeout(arm);
  resolve(outcome);
});
$("add-agent-form").onsubmit = (e) => {
  e.preventDefault();
  if ($("add-agent-go").disabled || !adding) return;
  adding.outcome = "added";
  addDialog.close();
};
$("add-agent-cancel").onclick = () => addDialog.close();

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
  $("agent-mark").replaceChildren(mark(state.current, "small"));
  const frame = state.frames.get(state.current);
  if (frame) frame.title = who.title;
}
function renderChats() {
  const list = shown(chats());
  patch($("chats"), list.length ? list.map((f) => {
    const who = identity(f.name);
    const row = el("button", `row agent-row${f.name === state.current && !state.page ? " active" : ""}`);
    row.type = "button";
    row.dataset.key = `chat:${f.name}`;
    if (isGroup(f.name)) row.dataset.group = String(agentsOf(f.name).length);
    row.setAttribute("aria-current", f.name === state.current && !state.page ? "page" : "false");
    const text = el("span", "agent-copy");
    text.append(el("span", "label", who.title), el("span", "agent-preview", who.preview || (own(f) ? "Say hello" : f.role)));
    row.append(mark(f.name), text, ...badges(f));
    row.onclick = () => { openChat(f.name); leaveSidebar(); };
    return row;
  }) : [el("div", "empty-row", chats().length ? "Every chat is archived" : "Your first agent starts here")]);
}
$("agent-heading").onclick = () => {
  const name = state.current;
  const group = isGroup(name);
  const agent = agentOfChat(name);
  // a group's agents each have a profile; a direct chat's agent has one
  const profiles = group
    ? agentsOf(name).map((id) => state.agents.get(id)?.fragment).filter(Boolean).map((a) => ({ icon: "agent", text: `${titleOf(a)}'s profile`, onClick: () => openApp(a) }))
    : agent ? [{ icon: "agent", text: "Its profile", onClick: () => openApp(agent) }] : [];
  // each agent's own desktop: a group's agents each have one to pick from
  const screens = !state.computer ? [] : group
    ? agentsOf(name).map((id) => state.agents.get(id)?.fragment).filter(Boolean).map((a) => ({ icon: "screen", text: `${titleOf(a)}'s screen`, onClick: () => openScreen(a) }))
    : agent ? [{ icon: "screen", text: "Its screen", onClick: () => openScreen(agent) }] : [];
  openMenu($("agent-heading"), [
    { icon: "rename", text: "Rename…", onClick: () => rename(name) },
    { icon: "invite", text: "Invite…", onClick: () => share(name) },
    ...profiles,
    ...screens,
    archiveItem(name),
  ]);
};
// the agent fragment a direct chat is with: the one whose title the chat
// carries (a group's agents keep names of their own)
function agentOfChat(name) {
  if (isGroup(name)) return null;
  const title = titleOf(name);
  return [...state.agents.values()].find((a) => titleOf(a.fragment) === title)?.fragment ?? null;
}

function openChat(name, push = true) {
  if (!byName(name)) return;
  if (push) at("/");
  state.page = null;
  $("settings-page").hidden = true;
  $("frames").hidden = false;
  state.current = name;
  store.set(CURRENT, name);
  $("notice").hidden = true;
  if (!state.frames.has(name)) {
    const frame = frameOf(framed(name), titleOf(name), name);
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
// An app's card: its page as the platform shot it after its last deploy
// (`GET /api/f/<name>/card`, read with this page's session and shown as a
// blob URL), or its icon until it has one.
const cards = new Map();
function cardOf(name) {
  const box = el("span", "app-card");
  const card = cards.get(name);
  if (card) {
    const img = el("img");
    img.src = card.url;
    img.alt = "";
    box.classList.add("shot");
    box.append(img);
  } else box.append(iconOf(name));
  return box;
}
// "new", "same", or "none" (no card yet)
async function loadCard(name) {
  const known = cards.get(name);
  const headers = { "x-fragment-shell": "1" };
  if (known) headers["if-none-match"] = known.etag;
  const r = await fetch(`/api/f/${seg(name)}/card`, { headers, credentials: "same-origin" });
  if (r.status === 304) return "same";
  if (r.status === 404 && !known) return "none";
  if (!r.ok) return "same";
  const url = URL.createObjectURL(await r.blob());
  if (known) URL.revokeObjectURL(known.url);
  cards.set(name, { etag: r.headers.get("etag"), url });
  return "new";
}
// every listed app's, again after a deploy may have made one: an app with
// none yet is asked again, waiting longer each time (2 s to a minute), and
// the rest when the page comes back after a minute away. `only` (a set of
// names, after a change to the list): those, and the apps still without one.
const cardsLoad = { wait: 0, timer: null, at: 0 };
async function loadCards(only = null) {
  clearTimeout(cardsLoad.timer);
  cardsLoad.at = Date.now();
  const listed = shown(apps()).map((f) => f.name);
  for (const [name, card] of cards) if (!listed.includes(name)) { URL.revokeObjectURL(card.url); cards.delete(name); }
  const asked = listed.filter((name) => !only || only.has(name) || !cards.has(name));
  const got = await Promise.all(asked.map((name) => loadCard(name).catch(() => "same")));
  if (got.includes("new")) renderApps();
  if (got.includes("none") && cardsLoad.wait < 60_000) {
    cardsLoad.wait = Math.min(60_000, cardsLoad.wait ? cardsLoad.wait * 2 : 2_000);
    cardsLoad.timer = setTimeout(() => loadCards().catch(() => {}), cardsLoad.wait);
  }
}
addEventListener("focus", () => { if (Date.now() - cardsLoad.at > 60_000) loadCards().catch(() => {}); });
// a pointer over an app's row shows its card larger, beside the sidebar
const peek = el("div", "card-peek");
peek.hidden = true;
peek.setAttribute("aria-hidden", "true");
peek.append(el("img"));
document.body.append(peek);
function showPeek(row, name) {
  const card = cards.get(name);
  if (!card) return;
  peek.firstChild.src = card.url;
  peek.hidden = false;
  const r = row.getBoundingClientRect();
  const edge = Math.max(r.right, $("sidebar").getBoundingClientRect().right);
  peek.style.left = `${Math.max(8, Math.min(edge + 12, innerWidth - peek.offsetWidth - 8))}px`;
  peek.style.top = `${Math.max(8, Math.min(r.top + r.height / 2 - peek.offsetHeight / 2, innerHeight - peek.offsetHeight - 8))}px`;
}
const hidePeek = () => { peek.hidden = true; };
function renderApps() {
  hidePeek();
  const list = shown(apps());
  patch($("apps"), list.length ? list.map((f) => {
    const row = el("button", `row app-row${viewer.keys.includes(`app:${f.name}`) ? " open" : ""}`);
    row.type = "button";
    row.dataset.key = `app:${f.name}`;
    row.append(cardOf(f.name), el("span", "label", titleOf(f.name)), ...badges(f));
    if (!own(f)) row.append(el("span", "meta", f.role));
    row.onclick = () => { hidePeek(); openApp(f.name); leaveSidebar(); };
    row.onpointerenter = (e) => { if (e.pointerType !== "touch") showPeek(row, f.name); };
    row.onpointerleave = hidePeek;
    return row;
  }) : [el("div", "empty-row", apps().length ? "Every app is archived" : "No apps yet")]);
}
function openApp(name) {
  const f = byName(name);
  if (!f) return;
  const frame = frameOf(framed(name), titleOf(name), name);
  const status = el("span", "pane-sharing");
  status.append(...badges(f));
  show({
    key: `app:${name}`, title: titleOf(name), subtitle: own(f) ? undefined : f.role, icon: iconOf(name), body: frame, status,
    actions: [
      ...(own(f) ? [{ icon: ICON.share, title: "Share…", onClick: () => share(name) }] : []),
      { icon: ICON.folder, title: "Files", onClick: () => show({ key: `tree:${name}`, title: titleOf(name), subtitle: "files", icon: paneIcon("folder"), body: frameOf(framed(name, "/__files"), `${titleOf(name)} files`, name) }) },
      { icon: ICON.reload, title: "Reload", onClick: () => { frame.src = framed(name); } },
      { icon: ICON.more, title: "More", onClick: (e) => openMenu(e.currentTarget, [archiveItem(name)]) },
    ],
  });
}

// ---- archiving: the person's own view (their list's row), not the fragment's ----
function archiveItem(name) {
  const archived = !!byName(name)?.archived;
  return { icon: "archive", text: archived ? "Unarchive" : "Archive", onClick: () => setArchived(name, !archived) };
}
async function setArchived(name, archived) {
  try {
    await api("PUT", `/api/fragments/${seg(name)}/archived`, { archived });
    await load();
    // the open chat, archived, gives the middle column to another
    const next = shown(chats()).find((f) => f.name !== name);
    if (archived && state.current === name && !state.page && next) openChat(next.name);
  } catch (e) {
    notice(archived ? "It was not archived" : "It was not unarchived", e.message);
  }
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

// ---- an agent's screen: its own desktop, a port of its computer's own
// origin, through a ticket that lands on the agent's page (docs/computers.md, Ports) ----
async function openScreen(agent) {
  if (!state.computer || !agent) return;
  try {
    const t = await api("POST", `/api/computers/${seg(state.computer.computer)}/ports/6080/ticket`, { path: `/?agent=${encodeURIComponent(agent)}` });
    const frame = frameOf(t.url, `${titleOf(agent)}'s screen`);
    show({ key: `screen:${agent}`, title: titleOf(agent), subtitle: "screen", icon: paneIcon("screen"), body: frame, persist: false });
  } catch (e) {
    notice("Its screen did not open", e.message);
  }
}

// ---- making an agent: its fragment, its computer, its chat (decision 10, 16) ----
// A label from a name, short enough that with freeLabel's `-<n>` and a
// chat's `-chat` its address fits any deployment's hosts (docs/api.md,
// Names: every deployment leaves 39 bytes for labels); never ending in a
// dash.
const LABEL_ROOM = 39;
const slug = (s) => s.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^-+/, "").slice(0, LABEL_ROOM - "-99-chat".length).replace(/-+$/, "") || "agent";
function freeLabel(base) {
  const taken = new Set(state.fragments.map((f) => labelOf(f.name)));
  for (let i = 1; ; i++) {
    const l = i === 1 ? base : `${base}-${i}`;
    if (!taken.has(l) && !taken.has(`${l}-chat`)) return l;
  }
}
function pickName() {
  const taken = new Set(state.fragments.map((f) => titleOf(f.name)));
  const free = NAMES.filter((n) => !taken.has(n));
  return free.length ? free[Math.floor(Math.random() * free.length)] : `Unit ${state.fragments.length + 1}`;
}
// Through public APIs only: the agent fragment (the agent template, its job
// in SOUL.md), assigned to the person's computer; a chat with it (the chat
// template), the agent added (the platform wakes its computer); and the job
// as the person's first message, which the agent answers.
async function makeAgent(job, chosen) {
  const name = chosen?.trim() || pickName();
  const label = freeLabel(slug(name));
  const agent = await api("POST", "/api/fragments", { label, template: "agent", title: name });
  const computer = await warmComputer();
  const assigned = await api("PUT", `/api/computers/${seg(computer.computer)}/agents/${seg(agent.name)}`, {});
  const id = assigned.agents.find((a) => a.fragment === agent.name)?.identity;
  // its colour is its identity's, as every page that shows it chooses it
  await api("POST", `/api/f/${agent.name}/files`, {
    key: "agent-job",
    message: "its job",
    files: [{ path: "SOUL.md", text: `${job.trim()}\n` }, { path: "agent.json", text: JSON.stringify({ tier: DEFAULT_AGENT_TIER, color: colorOf(id) }, null, 2) + "\n" }],
  });
  await api("POST", `/api/f/${agent.name}/deploy`, {});
  const chat = await api("POST", "/api/fragments", { label: `${label}-chat`, template: "chat", title: name });
  if (id) await api("PUT", `/api/f/${chat.name}/members/${seg(id)}`, { role: "editor" });
  await api("POST", `/api/f/${chat.name}/channels/chat`, { id: "job", body: { text: job.trim() } });
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

// ---- a group chat: several of the person's agents in one chat (decision 8) ----
// Through public APIs only, as makeAgent: a chat fragment on the chat
// template, titled by its name or its agents' names, and the agents added
// as editors one at a time in the order picked, so the first is the lead
// (the first added, by `addedAt`: the chat's page and its agents' bridges
// read it from the member list).
async function makeGroup(picked, chosen) {
  if (picked.length < 2) throw new Error("A group chat has two agents or more.");
  const name = chosen?.trim() ?? "";
  const title = name || picked.map((a) => titleOf(a.fragment)).join(", ");
  const chat = await api("POST", "/api/fragments", { label: freeLabel(slug(name || "group")), template: "chat", title });
  for (const a of picked) await api("PUT", `/api/f/${chat.name}/members/${seg(a.identity)}`, { role: "editor" });
  if (state.computer) api("POST", `/api/computers/${seg(state.computer.computer)}/wake`, {}).catch(() => {});
  return chat.name;
}
const groupDialog = $("new-group-dialog");
let picking = [];
function renderPicks() {
  const agents = [...state.agents.values()];
  $("new-group-agents").replaceChildren(...(agents.length ? agents.map((a) => {
    const at = picking.findIndex((p) => p.identity === a.identity);
    const row = el("button", "row pick");
    row.type = "button";
    row.setAttribute("aria-pressed", String(at >= 0));
    row.dataset.identity = a.identity;
    row.append(avatar({ color: colorOf(a.identity) }, "small"), el("span", "label", titleOf(a.fragment)));
    if (at >= 0) row.append(el("span", "meta", at === 0 ? "Lead" : String(at + 1)));
    row.onclick = () => {
      picking = at >= 0 ? picking.filter((p) => p.identity !== a.identity) : [...picking, a];
      renderPicks();
    };
    return row;
  }) : [el("p", "muted", "Your agents show here once you have some.")]));
  $("new-group-go").disabled = picking.length < 2;
}
function newGroup() {
  picking = [];
  $("new-group-form").reset();
  $("new-group-error").hidden = true;
  renderPicks();
  groupDialog.showModal();
  prewake();
}
$("new-group").onclick = newGroup;
$("new-group-cancel").onclick = () => groupDialog.close();
$("new-group-form").onsubmit = async (e) => {
  e.preventDefault();
  const go = $("new-group-go");
  go.disabled = true;
  go.textContent = "Making it…";
  try {
    const chat = await makeGroup(picking, $("new-group-name").value);
    groupDialog.close();
    await load();
    openChat(chat);
  } catch (err) {
    $("new-group-error").textContent = err.message;
    $("new-group-error").hidden = false;
  } finally {
    go.textContent = "Make it";
    go.disabled = picking.length < 2;
  }
};

// ---- renaming: the chat's title (and its agent's, so they stay one) ----
let renaming = null;
function rename(name) {
  renaming = name;
  const group = isGroup(name);
  $("rename-title").textContent = group ? "Name this chat" : "Name your agent";
  $("rename-label").textContent = group ? "Chat name" : "Agent name";
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
        const label = input.value.trim();
        const made = await api("POST", "/api/fragments", { label: label || freeLabel(t.template), template: t.template, title: label || t.name });
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

// ---- search: the person's chats, apps and messages (decision 9) ----
// Empty, it lists what the sidebar does; with words, it asks the person's
// list (`GET /api/search`: titles and names, then messages, newest first,
// archived ones included). A message opens its chat; it does not scroll to
// the message (docs/api.md, Search).
const isChat = (name) => byName(name)?.kind === "chat";
function openFound(name) {
  $("search-dialog").close();
  if (isChat(name)) openChat(name); else openApp(name);
  leaveSidebar();
}
function foundRow(name, preview, extra) {
  const f = byName(name);
  const row = el("button", "row search-result");
  row.type = "button";
  row.dataset.fragment = name;
  const text = el("span", "agent-copy");
  text.append(el("span", "label", titleOf(name)));
  if (preview) text.append(el("span", "agent-preview", preview));
  row.append(isChat(name) ? mark(name, "small") : iconOf(name), text);
  if (f?.archived) row.append(el("span", "meta", "Archived"));
  if (extra) row.append(extra);
  row.onclick = () => openFound(name);
  return row;
}
const when = (at) => {
  const d = new Date(at);
  return d.toDateString() === new Date().toDateString() ? d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }) : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
};
function renderFound(fragments, messages) {
  const results = $("search-results");
  results.replaceChildren();
  const groups = [
    ["Chats", fragments.filter((f) => f.kind === "chat").map((f) => foundRow(f.name, f.preview ?? ""))],
    ["Agents", fragments.filter((f) => f.kind === "agent").map((f) => foundRow(f.name, ""))],
    ["Apps", fragments.filter((f) => f.kind === "app" || f.kind === "brain").map((f) => foundRow(f.name, ""))],
    // a message in a fragment the list has not shown yet is left out until it has
    ["Messages", messages.filter((m) => byName(m.fragment)).map((m) => {
      const row = foundRow(m.fragment, m.snippet, el("span", "meta", when(m.at)));
      row.classList.add("search-message");
      row.dataset.seq = String(m.seq);
      return row;
    })],
  ];
  let count = 0;
  for (const [heading, rows] of groups) {
    if (!rows.length) continue;
    count += rows.length;
    results.append(el("h3", null, heading), ...rows);
  }
  $("search-status").textContent = count ? `${count} result${count === 1 ? "" : "s"}` : "Nothing found.";
}
let asked = 0;
let typing = null;
async function search() {
  const q = $("workspace-search").value.trim();
  const mine = ++asked;
  if (!q) return renderFound([...shown(chats()), ...shown(apps())], []);
  try {
    const found = await api("GET", `/api/search?q=${encodeURIComponent(q)}`);
    // a slower answer to an older query never replaces a newer one's
    if (mine === asked) renderFound(found.fragments ?? [], found.messages ?? []);
  } catch (e) {
    if (mine === asked) $("search-status").textContent = e.message;
  }
}
$("search-agents").onclick = () => {
  $("workspace-search").value = "";
  search();
  $("search-dialog").showModal();
  $("search-agents").setAttribute("aria-expanded", "true");
  $("workspace-search").focus();
};
$("search-close").onclick = () => $("search-dialog").close();
$("search-dialog").addEventListener("close", () => $("search-agents").setAttribute("aria-expanded", "false"));
$("workspace-search").addEventListener("input", () => {
  clearTimeout(typing);
  typing = setTimeout(search, 150);
});
$("workspace-search").addEventListener("keydown", (e) => {
  const first = $("search-results").querySelector("button");
  if (e.key === "ArrowDown" && first) { e.preventDefault(); first.focus(); }
  if (e.key === "Enter" && first) { e.preventDefault(); first.click(); }
});

// ---- the computer: awake while the person is here, and its updates ----
let woke = 0;
// asking for a wake as the person arrives, focuses a chat, or starts a new
// agent hides the computer's start (decision 39's pre-wake, from the shell)
// The person's computer, made once (making it again is the same one:
// docs/api.md), and a wake of it. A new agent's maker wakes it at once, so
// the start hides behind the making (decision 39's pre-wake).
let making = null;
function computerOf() {
  making ??= api("POST", "/api/computers", {}).then((c) => { state.computer = c; return c; });
  making.catch(() => { making = null; });
  return making;
}
async function warmComputer() {
  const c = await computerOf();
  api("POST", `/api/computers/${seg(c.computer)}/wake`, {}).then(computerIs).catch(() => {});
  return c;
}
// A computer that won't start is not woken by the person's presence: its
// starts kept failing, so the next is theirs to ask for (Restart, in its
// notice), never one every visit pays for unasked.
function prewake() {
  const c = state.computer;
  if (!c || document.hidden || Date.now() - woke < 60_000 || ["awake", "starting", "wont_wake"].includes(c.phase)) return;
  woke = Date.now();
  api("POST", `/api/computers/${seg(c.computer)}/wake`, {}).then(computerIs).catch(() => {});
}
document.addEventListener("visibilitychange", prewake);
// The computer as the platform last answered: shown at once.
function computerIs(v) {
  if (!v?.computer) return;
  state.computer = v;
  renderComputer();
}

// ---- what the person should know of their computer (docs/computers.md,
// "What its owner is told"): the view's notices, the most pressing first,
// each in plain words with the one way back to working. Its computer tells
// this page's list socket when they change, so they show while there is
// still time. Times are the person's own clock. ----
const clock = (ms) => {
  const d = new Date(ms);
  const today = d.toDateString() === new Date().toDateString();
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return today ? time : `${d.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
};
const span = (ms) => {
  const m = Math.max(1, Math.round(ms / 60_000));
  if (m < 60) return `${m} minute${m === 1 ? "" : "s"}`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h} hour${h === 1 ? "" : "s"}` : `${Math.round(h / 24)} days`;
};
// why a computer won't start, in plain words (the platform's own words
// stay beside them, for whoever helps)
function wontStart(why) {
  if (/not ready within/.test(why)) return "It didn't finish starting within a few minutes.";
  if (/kept stopping/.test(why)) return "It kept stopping as soon as it started.";
  if (/check/.test(why)) return "Its saved files didn't pass their check.";
  if (/restore|archive|backup/i.test(why)) return "Its save couldn't be restored.";
  return "It couldn't start.";
}
const LOST = {
  crash: "Your computer stopped unexpectedly",
  unsaved: "Your computer was stopped because it couldn't save",
  restart: "Your restart couldn't save first",
  unusable: "Your computer's newest save couldn't be restored",
};
// a restart the person asked for in this page, and of which start: a
// computer that still won't start after one says where to get help
let restarted = 0;
function noticeRow(n) {
  const row = el("div", "computer-notice");
  row.dataset.kind = n.kind;
  const copy = el("div", "notice-copy");
  const actions = el("div", "notice-actions");
  const say = (title, text, detail) => {
    copy.append(el("strong", null, title), el("p", null, text));
    if (detail) copy.append(el("p", "notice-detail", detail));
  };
  const restart = (label) => {
    const b = el("button", "primary", label);
    b.type = "button";
    b.dataset.action = "restart";
    b.onclick = () => restartComputer(b);
    actions.append(b);
  };
  if (n.kind === "wont_wake") {
    row.classList.add("warn");
    const again = restarted && state.computer?.generation > restarted;
    say(
      again ? "Your computer still won't start" : "Your computer won't start",
      `${wontStart(n.why)} Your agents can't answer until it does.${again ? " If restarting again doesn't help, get help and give them the details below." : ""}`,
      `${state.computer?.computer ?? ""}: ${n.why}`,
    );
    restart(again ? "Try again" : "Restart");
    if (again && state.support) {
      const help = el("a", null, "Get help");
      help.href = state.support;
      help.target = "_blank";
      help.rel = "noopener";
      actions.append(help);
    }
  } else if (n.kind === "unsaved") {
    row.classList.add("warn");
    const then = n.save ? "starts again from that save" : "starts again with nothing kept";
    const stop = n.stopsAt ? ` If it still can't by ${clock(n.stopsAt)}, it stops and ${then}.` : "";
    say("Your computer can't save right now", `It hasn't saved since ${clock(n.since)}, so what its agents did after that isn't kept yet. It keeps trying.${stop} Restarting tries to save first.`, n.why);
    restart("Restart now");
  } else if (n.kind === "went_back") {
    const back = n.save && n.savedAt
      ? `${n.pending ? "It goes back" : "It went back"} to its save of ${clock(n.savedAt)}${n.endedAt ? `, ${span(n.endedAt - n.savedAt)} before it stopped` : ""}.`
      : `${n.pending ? "It starts again" : "It started again"} with nothing kept from before.`;
    say(LOST[n.cause] ?? "Your computer went back to an older save", `${back} Work on the computer after that is gone; your chats keep everything that was said.`);
    const ok = el("button", null, "OK");
    ok.type = "button";
    ok.dataset.action = "seen";
    ok.onclick = () => sawNotice(n.life, ok);
    actions.append(ok);
  } else return null;
  row.append(copy, actions);
  return row;
}
function renderComputer() {
  const box = $("computer-notices");
  const rows = (state.computer?.notices ?? []).map(noticeRow).filter(Boolean);
  box.replaceChildren(...rows);
  box.hidden = !rows.length;
}
// Restart: the computer saves if it can, then starts fresh from its newest
// good save. It names the start the person saw, so pressed twice (or in two
// tabs) it restarts once. What went wrong is said beside the button.
async function restartComputer(button) {
  const c = state.computer;
  if (!c) return;
  const buttons = [...document.querySelectorAll("[data-action=restart]")];
  for (const b of buttons) b.disabled = true;
  const label = button.textContent;
  button.textContent = "Restarting…";
  try {
    restarted = c.generation || 0;
    computerIs(await api("POST", `/api/computers/${seg(c.computer)}/restart`, { generation: c.generation || 0 }));
    if (state.page === "Settings") await openSettings(false);
  } catch (e) {
    button.textContent = label;
    const said = button.parentElement.querySelector(".form-error") ?? el("p", "form-error");
    said.textContent = `It did not restart: ${e.message}`;
    button.parentElement.append(said);
  } finally {
    for (const b of buttons) b.disabled = false;
  }
}
// The person read what a start went back to: told no more, anywhere.
async function sawNotice(life, button) {
  const c = state.computer;
  if (!c) return;
  button.disabled = true;
  try {
    computerIs(await api("POST", `/api/computers/${seg(c.computer)}/notices/seen`, { life }));
  } catch {
    computerIs(await api("GET", `/api/computers/${seg(c.computer)}`).catch(() => null));
  }
}
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
    await api("PUT", `/api/computers/${seg(c.computer)}/image`, { image: state.defaultImage });
    await api("POST", `/api/computers/${seg(c.computer)}/sleep`, {});
    computerIs(await api("POST", `/api/computers/${seg(c.computer)}/wake`, {}));
    $("update-confirm").hidden = true;
    $("update-pill").hidden = false;
    renderUpdate();
  } catch (e) {
    $("update-text").textContent = `The update did not finish: ${e.message}`;
  } finally {
    $("update-go").disabled = false;
  }
};

// ---- where the page is: `/` its chats, `/settings` its settings; each
// view says so in the address, so a reload or a link stays put ----
const SETTINGS = "/settings";
function at(path) {
  if (location.pathname !== path) history.pushState(null, "", path);
}
addEventListener("popstate", () => {
  const shown = !$("layout").hidden;
  if (shown && location.pathname === SETTINGS) openSettings(false).catch((e) => notice("Settings did not open", e.message));
  else if (shown && location.pathname !== SETTINGS && byName(state.current)) openChat(state.current, false);
  else start().catch((e) => notice("This page did not load", e.message));
});

// ---- settings: the person's account, credit, computer and agents ----
const usd = (micros) => (micros / 1_000_000).toLocaleString(undefined, { style: "currency", currency: "USD" });
// what a standing short of `ok` stops, and why (crates/core/src/ledger.rs `Refused`)
const STOPPED = { agents_stopped: "Your agents are stopped", read_only: "Your fragments are read-only, and your agents are stopped" };
// a computer's phase, as a person reads it (proto's `ComputerPhase`)
const PHASE = { asleep: "Asleep", starting: "Starting", awake: "Awake", sleeping: "Going to sleep", wont_wake: "Won't start" };
const WHY = {
  guest: "a guest has no agents",
  seat_canceled: "the seat was canceled",
  no_credit: "the credit is used up; adding credit starts them again",
  overdrawn: "the balance reached the overdraft; credit that brings it above zero ends it",
};
// The CLI's one-line install, for macOS and Linux (cli/SKILL.md's: `cargo
// xtask check` holds the two to each other and to the release's assets),
// and a coding agent's skill, in Claude Code's folder.
const INSTALL = "mkdir -p ~/.local/bin && curl -fsSL https://github.com/futurepaul/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin";
const SKILL = "mkdir -p ~/.claude/skills/fragment && fragment skill > ~/.claude/skills/fragment/SKILL.md";
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
// a sentence whose `quoted` parts are code
function say(text) {
  const p = el("p");
  text.split("`").forEach((part, i) => p.append(i % 2 ? el("code", null, part) : part));
  return p;
}
async function openSettings(push = true) {
  if (push) at(SETTINGS);
  state.page = "Settings";
  $("frames").hidden = true;
  const page = $("settings-page");
  page.hidden = false;
  page.replaceChildren(el("p", "muted", "Loading…"));
  renderHeading();
  renderChats();
  leaveSidebar();
  // Checkout's return, or a trial mailed, before the ledger is read
  // with no chats yet, a seat's Billing offers the first run (home makes
  // the first agent: `start`)
  const billingHelpers = { api, el, section, line, usd, reopen: () => openSettings(false), firstAgent: chats().length ? null : firstAgentOnceSeated };
  const back = await returning(billingHelpers);
  const [ledger, linked, uses] = await Promise.all([
    api("GET", "/api/ledger").catch(() => null),
    api("GET", "/api/connections").catch(() => null),
    state.computer ? api("GET", `/api/computers/${seg(state.computer.computer)}/uses`).catch(() => null) : null,
  ]);
  const emails = (state.me.subjects ?? []).map((x) => x.email).filter(Boolean);
  const id = line("Identity", state.me.id);
  id.lastChild.classList.add("mono");
  const account = section(
    "Account",
    line(emails.length > 1 ? "Sign-ins" : "Signed in as", emails.join(", ") || "—"),
    id,
  );
  // a picture: PNG, JPEG, WebP or GIF, at most 256 KiB (PUT /api/identities/me/picture)
  const picture = el("label", "quiet picture");
  const shown = el("img");
  shown.alt = "";
  shown.hidden = !state.me.picture;
  if (state.me.picture) shown.src = state.me.picture;
  const file = el("input");
  file.type = "file";
  file.accept = "image/png,image/jpeg,image/webp,image/gif";
  file.hidden = true;
  picture.append(shown, el("span", null, state.me.picture ? "Change your picture" : "Add a picture"), file);
  file.onchange = async () => {
    const f = file.files?.[0];
    if (!f) return;
    const r = await fetch("/api/identities/me/picture", { method: "PUT", headers: { "x-fragment-shell": "1", "content-type": f.type }, body: f, credentials: "same-origin" });
    if (r.ok) {
      state.me = await api("GET", "/api/identities/me");
      openSettings(false);
    } else picture.querySelector("span").textContent = (await r.json().catch(() => ({}))).message || "That picture was not taken";
  };
  account.append(picture);
  // another sign-in (an email, a provider) that is this person too, and signing out
  const actions = el("div", "settings-actions");
  const link = el("a", "quiet", "Add another sign-in");
  link.href = `/auth/link?return=${encodeURIComponent(SETTINGS)}`;
  const signout = el("form");
  signout.method = "post";
  signout.action = "/auth/logout";
  const out = el("button", "quiet", "Sign out");
  out.type = "submit";
  signout.append(out);
  actions.append(link, signout);
  account.append(actions);
  const standing = ledger?.standing?.standing;
  // the person's seat, credit and org (billing.js), and why agents stop
  const billing = await billingSections(billingHelpers, ledger, back);
  if (standing && standing !== "ok") {
    billing[0].append(el("p", "settings-warning", [STOPPED[standing] ?? "Your agents are stopped", WHY[ledger.standing.why]].filter(Boolean).join(": ") + "."));
  }
  const c = state.computer;
  const computer = section(
    "Computer",
    ...(c
      ? [line("State", PHASE[c.phase] ?? c.phase), line("Version", c.image), ...(c.why ? [el("p", "settings-warning", c.why)] : [])]
      : [el("p", "muted", "Your computer starts with your first agent.")]),
  );
  // each agent's own desktop
  for (const a of c ? state.agents.values() : []) {
    const screen = el("button", "quiet", `Open ${titleOf(a.fragment)}'s screen`);
    screen.type = "button";
    screen.onclick = () => openScreen(a.fragment);
    computer.append(screen);
  }
  // the way back to working when something is wrong with it: asked once
  // more before it cuts what its agents are doing
  if (c) {
    computer.append(el("p", "muted", "Restarting saves your computer if it can, then starts it fresh from that save. Anything its agents are doing stops."));
    const row = el("div", "settings-actions");
    const go = el("button", "quiet", "Restart computer");
    go.type = "button";
    go.dataset.action = "restart-ask";
    go.onclick = () => {
      const sure = el("button", "quiet", "Restart now");
      sure.type = "button";
      sure.dataset.action = "restart";
      sure.onclick = () => restartComputer(sure);
      const cancel = el("button", "quiet", "Cancel");
      cancel.type = "button";
      cancel.onclick = () => row.replaceChildren(go);
      row.replaceChildren(sure, cancel);
    };
    row.append(go);
    computer.append(row);
  }
  const agents = section("Agents");
  const mine = [...state.agents.values()];
  agents.append(...(mine.length ? mine.map((a) => {
    const row = el("button", "row");
    row.type = "button";
    row.append(avatar({ color: colorOf(a.identity) }, "small"), el("span", "label", titleOf(a.fragment)));
    row.onclick = () => openApp(a.fragment);
    return row;
  }) : [el("p", "muted", "None yet.")]));
  const connections = connectionsSection(linked, uses);
  const skills = await skillsSection();
  // pairing the CLI: `fragment login` opens this platform to approve its key
  const cli = section(
    "The command line",
    say("Your agents and you can publish apps from a terminal. Install `fragment` (macOS or Linux):"),
    el("pre", "command", INSTALL),
    say("If `fragment` is not found after, put `~/.local/bin` on your PATH. Then run `fragment login`: it opens this platform to approve its key. To have your coding agent (Claude Code, Codex) do the work, give it the skill:"),
    el("pre", "command", SKILL),
  );
  page.replaceChildren(account, ...billing, computer, agents, skills, connections, cli, ...credited(WALLPAPER));
}

// ---- connections (decisions 22, 37 and 44): every provider the platform
// offers, one row each: what it is, its state, which of the person's agents
// may use it (all by default; a list narrows one agent), and this month's
// calls by each agent, with an operator key's cost. Their agents use them
// through the computer's swap, each with a placeholder of its own.
const PROVIDER_KIND = { connection: "Your account", operator: "The platform's key", own: "Your key" };
const PROVIDER_STATE = {
  connected: "Connected",
  needs_reauthorization: "Needs reauthorization",
  not_connected: "Not connected",
  offered: "Offered",
  set: "Key set",
  not_set: "No key yet",
};
// a provider's name for people: google-places is Google Places, xai xAI
const PROVIDER_NAMES = { xai: "xAI", elevenlabs: "ElevenLabs" };
const providerName = (p) => PROVIDER_NAMES[p] ?? p.replace(/(^|-)([a-z])/g, (_, d, l) => `${d ? " " : ""}${l.toUpperCase()}`);
// micro-dollars, to a hundredth of a cent under a dollar (a call is often less than a cent)
const money = (micros) => (micros / 1_000_000).toLocaleString(undefined, { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: micros < 1_000_000 ? 4 : 2 });
function connectionsSection(linked, uses) {
  const s = section("Connections");
  s.id = "settings-connections";
  const offered = linked?.providers ?? [];
  if (!offered.length) {
    s.append(el("p", "muted", linked ? "This platform offers none yet." : "Your connections could not be read."));
    return s;
  }
  s.append(el("p", "muted", "Your agents use these through their computer, which adds each credential on the way out. None is ever on the computer."));
  const agents = state.computer?.agents ?? [];
  for (const p of offered) {
    const row = el("div", "provider");
    row.dataset.provider = p.provider;
    row.dataset.kind = p.kind;
    row.dataset.state = p.state;
    const head = el("div", "provider-head");
    head.append(el("span", "provider-name", providerName(p.provider)), el("span", "provider-kind", PROVIDER_KIND[p.kind] ?? p.kind));
    const st = el("span", `provider-state ${p.state}`, PROVIDER_STATE[p.state] ?? p.state);
    head.append(st);
    row.append(head);
    const actions = el("div", "settings-actions");
    if (p.kind === "connection" && p.state !== "connected") {
      const go = el("button", "quiet", p.state === "needs_reauthorization" ? "Connect again" : "Connect");
      go.type = "button";
      go.onclick = async () => {
        go.disabled = true;
        try {
          const { url } = await api("POST", `/api/connections/${encodeURIComponent(p.provider)}/authorize`, {});
          window.open(url, "_blank", "popup,width=520,height=720");
          // back from the provider: the section says so
          addEventListener("focus", () => openSettings(false).catch(() => {}), { once: true });
        } catch (e) {
          go.textContent = e.message;
        } finally {
          go.disabled = false;
        }
      };
      actions.append(go);
    }
    if (p.kind === "own") {
      const form = el("form", "provider-key");
      const input = el("input");
      input.type = "password";
      input.autocomplete = "off";
      input.placeholder = p.state === "set" ? "Replace your key" : `Your ${providerName(p.provider)} key`;
      const save = el("button", "quiet", "Save");
      save.type = "submit";
      form.append(input, save);
      form.onsubmit = async (e) => {
        e.preventDefault();
        save.disabled = true;
        try {
          await api("PUT", `/api/connections/${encodeURIComponent(p.provider)}/key`, { key: input.value });
          await openSettings(false);
        } catch (err) {
          save.textContent = err.message;
          save.disabled = false;
        }
      };
      actions.append(form);
      if (p.state === "set") {
        const remove = el("button", "quiet", "Remove");
        remove.type = "button";
        remove.onclick = async () => {
          remove.disabled = true;
          await api("DELETE", `/api/connections/${encodeURIComponent(p.provider)}/key`).catch(() => {});
          await openSettings(false);
        };
        actions.append(remove);
      }
    }
    if (p.kind === "operator" && p.price) {
      actions.append(el("span", "muted", `${money(p.price.micros / p.price.per)} a call at list, metered to your credit`));
    }
    if (actions.childElementCount) row.append(actions);
    // which agents may use it: all by default; one narrowed to a list uses those
    if (agents.length) {
      const who = el("div", "provider-agents");
      who.append(el("span", "settings-key", "Agents"));
      for (const a of agents) {
        const allowed = a.connections == null || a.connections.includes(p.provider);
        const chip = el("button", "chip", titleOf(a.fragment));
        chip.type = "button";
        chip.dataset.agent = a.fragment;
        chip.setAttribute("aria-pressed", String(allowed));
        chip.title = allowed ? `${titleOf(a.fragment)} may use it: press to take it away` : `${titleOf(a.fragment)} may not use it: press to let it`;
        chip.onclick = async () => {
          chip.disabled = true;
          const all = offered.map((x) => x.provider);
          const now = a.connections ?? all;
          const next = allowed ? now.filter((x) => x !== p.provider) : [...new Set([...now, p.provider])];
          const list = all.every((x) => next.includes(x)) ? null : next;
          try {
            computerIs(await api("PUT", `/api/computers/${seg(state.computer.computer)}/agents/${seg(a.fragment)}/connections`, { connections: list }));
            state.agents = new Map((state.computer?.agents ?? []).map((x) => [x.identity, x]));
          } catch (e) {
            notice("That was not changed", e.message);
          }
          await openSettings(false);
        };
        who.append(chip);
      }
      row.append(who);
    }
    // this month's calls through the swap, by agent, and an operator key's cost
    const mine = (uses?.uses ?? []).filter((u) => u.provider === p.provider);
    const used = el("div", "provider-uses");
    used.append(el("span", "settings-key", uses?.month ? `In ${uses.month}` : "This month"));
    if (!mine.length) used.append(el("span", "muted", "No calls yet"));
    for (const u of mine) {
      const calls = `${u.calls} ${u.calls === 1 ? "call" : "calls"}`;
      const item = el("span", "use", `${titleOf(u.agent)}: ${p.kind === "operator" ? `${calls}, ${money(u.micros)}` : calls}`);
      item.dataset.use = u.agent;
      item.dataset.calls = String(u.calls);
      item.dataset.micros = String(u.micros);
      used.append(item);
    }
    row.append(used);
    s.append(row);
  }
  return s;
}

// ---- skills (decision 17): the managed set, the person's skills fragment's
// (the platform's release, which their computers install for every agent),
// by category, and each agent's own (its fragment's skills/, which win on a
// name). Both read as the agents' computers read them: the fragments' files.
// A skill is skills/<category>/<name>/SKILL.md, or skills/<name>/SKILL.md.
function skillsIn(paths) {
  const out = [];
  for (const path of paths) {
    const parts = path.split("/");
    if (parts[0] !== "skills" || parts[parts.length - 1] !== "SKILL.md") continue;
    if (parts.length === 4) out.push({ category: parts[1], name: parts[2] });
    else if (parts.length === 3) out.push({ category: "", name: parts[1] });
  }
  return out.sort((a, b) => a.category.localeCompare(b.category) || a.name.localeCompare(b.name));
}
const skillsFragmentOf = () => state.fragments.find((f) => f.kind === "skills" && own(f));
// The person's skills fragment, made from the blessed template when they
// have none: at setup, as their default agent is, or from settings' Skills
// for a person without one (deleted, or set up before 2026-10-03).
async function skillsFragment() {
  const have = skillsFragmentOf();
  if (have) return have.name;
  const made = await api("POST", "/api/fragments", { label: freeLabel("skills"), template: "skills" });
  await load();
  return made.name;
}
function skillNames(list) {
  const value = el("span", "settings-value");
  list.forEach((s, i) => {
    if (i) value.append(", ");
    const name = el("span", "skill", s.name);
    name.dataset.skill = s.name;
    value.append(name);
  });
  return value;
}
async function skillsSection() {
  const s = section("Skills");
  s.id = "settings-skills";
  const filesOf = (name) => api("GET", `/api/f/${name}/files`).then((v) => (v.files ?? []).map((f) => f.path)).catch(() => null);
  const mine = skillsFragmentOf();
  if (!mine) {
    const add = el("button", "quiet", "Add the managed skills");
    add.type = "button";
    add.onclick = () => skillsFragment().then(() => openSettings(false)).catch((e) => { add.textContent = e.message; });
    s.append(el("p", "muted", "Your agents have no managed skills yet."), add);
  } else {
    const paths = await filesOf(mine.name);
    if (!paths) s.append(el("p", "muted", "Your skills could not be read."));
    else {
      const groups = new Map();
      for (const k of skillsIn(paths)) groups.set(k.category, [...(groups.get(k.category) ?? []), k]);
      s.dataset.fragment = mine.name;
      s.append(...[...groups].map(([category, list]) => {
        const row = el("p", "settings-line");
        row.dataset.category = category || "general";
        row.append(el("span", "settings-key", (category || "general").replace(/-/g, " ")), skillNames(list));
        return row;
      }));
      const open = el("button", "quiet", "Open your skills");
      open.type = "button";
      open.onclick = () => openApp(mine.name);
      s.append(open);
    }
  }
  // each agent's own skills, from its fragment
  const agents = state.fragments.filter((f) => f.kind === "agent" && own(f));
  const owns = await Promise.all(agents.map((a) => filesOf(a.name)));
  agents.forEach((a, i) => {
    const row = el("p", "settings-line");
    row.dataset.agent = a.name;
    const list = skillsIn(owns[i] ?? []);
    row.append(el("span", "settings-key", `${titleOf(a.name)}'s own`), list.length ? skillNames(list) : el("span", "settings-value muted", owns[i] ? "None yet" : "Not read"));
    s.append(row);
  });
  return s;
}
// The wallpaper's photographer, credited. A photo of the person's own
// (later) is credited to no one: `credited(null)` is no section.
const WALLPAPER = { by: "Teo Badini", at: "https://www.pexels.com/@teobadini/", on: "Pexels" };
function credited(photo) {
  if (!photo) return [];
  const who = el("a", null, photo.by);
  who.href = photo.at;
  who.target = "_blank";
  who.rel = "noopener";
  const p = el("p", "muted");
  p.append("Photo by ", who, ` on ${photo.on}`);
  return [section("Wallpaper", p)];
}
$("settings").onclick = () => openSettings().catch((e) => notice("Settings did not open", e.message));

// ---- first run: sign in, then the default agent is made (decision 10) ----
function firstRun(...children) {
  $("layout").hidden = true;
  $("first-run").hidden = false;
  $("first-run-card").replaceChildren(el("div", "brand", $("brand").textContent), ...children);
}
function signIn() {
  const go = el("a", "primary", "Sign in");
  go.href = `/auth/login?return=${encodeURIComponent(location.pathname)}`;
  firstRun(el("h1", null, "Agents that work for you, and the apps they make."), el("p", "muted", "Sign in to start."), go);
}
// A seat bought (or given) before any chat: home's first run, once the
// seat's plan has reached the ledger (the registry pushes it from its
// alarm, a moment after), so the first run's creates are a seat's. Bounded:
// ten seconds, then home anyway, which shows Billing again if it has not.
async function firstAgentOnceSeated() {
  for (let i = 0; i < 20; i++) {
    const ledger = await api("GET", "/api/ledger").catch(() => null);
    if (ledger && ledger.plan !== "guest") break;
    await new Promise((r) => setTimeout(r, 500));
  }
  location.assign("/");
}
// The first agent (Paul, 2026-10-03): no question asked. It is the
// person's default agent, in charge, made with its computer and its chat
// while this screen waits, so the chat opens with it ready and the first
// message is answered at once, not after a computer's first start.
const SETUP_WAIT_MS = 4 * 60_000;
const firstSoul = (name, owner) => `You are ${name}, ${owner}'s default agent: the first one they talk to, and in charge of the rest. Help with whatever they ask. When a job would be better as an app, or as an agent of its own, say so and offer to set it up. The first time you talk, say hello briefly and ask what they'd like to start with.\n`;
// Each step reuses what an earlier try made: a retry picks up where it stopped.
async function defaultAgent(step) {
  await load();
  const owner = state.me.email || "your person";
  step("agent");
  // the managed skills its computer installs at its first start (decision 17)
  await skillsFragment();
  const computer = await computerOf();
  let agent = state.fragments.find((f) => f.kind === "agent" && f.role === "owner");
  if (!agent) {
    const title = pickName();
    const made = await api("POST", "/api/fragments", { label: freeLabel(slug(title)), template: "agent", title });
    agent = { name: made.name, title };
  }
  const title = agent.title || labelOf(agent.name);
  // assigned before its computer's first start, which then runs it
  const assigned = await api("PUT", `/api/computers/${seg(computer.computer)}/agents/${seg(agent.name)}`, {});
  const id = assigned.agents.find((a) => a.fragment === agent.name)?.identity;
  if (!id) throw new Error("its computer did not take it");
  await api("POST", `/api/f/${agent.name}/files`, {
    key: "agent-default",
    message: "the default agent",
    files: [{ path: "SOUL.md", text: firstSoul(title, owner) }, { path: "agent.json", text: JSON.stringify({ tier: DEFAULT_AGENT_TIER, color: colorOf(id) }, null, 2) + "\n" }],
  });
  await api("POST", `/api/f/${agent.name}/deploy`, {});
  // its chat, by its label (a retry finds the one an earlier try made)
  const chatLabel = `${labelOf(agent.name)}-chat`;
  const chatName =
    state.fragments.find((f) => f.kind === "chat" && f.role === "owner" && labelOf(f.name) === chatLabel)?.name ??
    (await api("POST", "/api/fragments", { label: chatLabel, template: "chat", title })).name;
  // adding it to its chat is what wakes the computer (the platform's `joined`)
  await api("PUT", `/api/f/${chatName}/members/${seg(id)}`, { role: "editor" });
  api("POST", `/api/computers/${seg(computer.computer)}/wake`, {}).catch(() => {});
  return { chat: chatName, id, computer: computer.computer };
}
// Ready: its computer awake, and the agent following its chat (its wake
// subscription, which its guest makes as it follows: the chat's owner sees it).
async function agentReady({ chat, id, computer }) {
  const [c, subs] = await Promise.all([
    api("GET", "/api/computers").then((v) => v.computers?.find((x) => x.computer === computer)),
    api("GET", `/api/f/${chat}/subscriptions`),
  ]);
  // a wake refused (its owner's credit) is said, not waited out
  if (c?.why && c.phase !== "awake") throw Object.assign(new Error(c.why), { refused: true });
  return c?.phase === "awake" && (subs.subscriptions ?? []).some((x) => x.wake && x.principal === id && x.channel === "chat");
}
function creatingAgent() {
  const steps = [["agent", "Making your agent"], ["computer", "Starting its computer"]];
  const list = el("ol", "creating-steps");
  const items = new Map(steps.map(([key, text]) => { const li = el("li", null, text); list.append(li); return [key, li]; }));
  let current = null;
  const step = (key) => {
    if (current) items.get(current).className = "done";
    current = key;
    items.get(key).className = "doing";
  };
  const note = el("p", "muted", "It starts once, now, so it answers you at once after.");
  const error = el("p", "form-error");
  error.hidden = true;
  const retry = el("button", "primary", "Try again");
  retry.type = "button";
  retry.hidden = true;
  retry.onclick = () => creatingAgent();
  const anyway = el("button", "quiet", "Open the chat now");
  anyway.type = "button";
  anyway.hidden = true;
  firstRun(el("h1", null, "Creating your agent…"), note, list, error, retry, anyway);
  (async () => {
    const made = await defaultAgent(step);
    step("computer");
    anyway.onclick = () => start(made.chat);
    const t0 = Date.now();
    // one look every 1.5 s; past SETUP_WAIT_MS the person may go on without it
    for (;;) {
      let ready = false;
      try {
        ready = await agentReady(made);
      } catch (e) {
        if (e.refused) throw e;
      }
      if (ready) break;
      if (Date.now() - t0 > SETUP_WAIT_MS && anyway.hidden) {
        note.textContent = "It's taking longer than usual. It answers once its computer is up.";
        anyway.hidden = false;
      }
      await new Promise((r) => setTimeout(r, 1500));
    }
    items.get("computer").className = "done";
    await start(made.chat);
  })().catch((err) => {
    if (current) items.get(current).className = "failed";
    error.textContent = err.message;
    error.hidden = false;
    retry.hidden = false;
  });
}

// ---- the person's things, read again whenever they may have changed ----
// One read of their list is the sidebar: each chat's row names its agents
// and its newest message. After a change their list told this page of
// (`watchList`: `changed`), only the apps whose rows it touched (new, or
// not as they were) have their card read again. An older read's answer
// that comes after a newer one's is not shown.
const reads = { asked: 0, shown: 0 };
async function load(changed = false) {
  const mine = ++reads.asked;
  const [list, computers] = await Promise.all([api("GET", "/api/fragments"), api("GET", "/api/computers").catch(() => ({ computers: [] }))]);
  if (mine < reads.shown) return;
  reads.shown = mine;
  const before = new Map(state.fragments.map((f) => [f.name, JSON.stringify(f)]));
  state.fragments = list.fragments ?? [];
  state.computer = computers.computers?.[0] ?? null;
  state.defaultImage = computers.defaultImage ?? null;
  state.support = computers.support ?? null;
  state.agents = new Map((state.computer?.agents ?? []).map((a) => [a.identity, a]));
  renderChats();
  renderApps();
  renderHeading();
  renderUpdate();
  renderComputer();
  rosterChanged();
  const touched = changed ? new Set(state.fragments.filter((f) => before.get(f.name) !== JSON.stringify(f)).map((f) => f.name)) : null;
  cardsLoad.wait = 0;
  loadCards(touched).catch(() => {});
}

// ---- live: the person's list tells this page when it changes ----
// An app their agent makes, one someone shares with them, a delete or an
// archive elsewhere (Paul on p5, 2026-10-05: his agent's app showed only
// after a reload), a chat's new message: their Principal tells this page's socket
// (`GET /api/fragments/watch`, which names nothing), and a moment after,
// so a burst is one read, the page reads the list again and patches the
// sidebar in place. A socket that closes is opened again after a jittered
// wait (1 s, doubling to 30 s); each opening reads the list again, for
// what changed while it was shut.
const LIST_SETTLE_MS = 200;
const watching = { on: false, socket: null, wait: 1000, timer: null, settle: null };
function listChanged() {
  clearTimeout(watching.settle);
  watching.settle = setTimeout(() => load(true).catch(() => {}), LIST_SETTLE_MS);
}
function watchList() {
  watching.on = true;
  clearTimeout(watching.timer);
  if (watching.socket) return;
  const ws = new WebSocket(`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/fragments/watch`);
  watching.socket = ws;
  ws.onopen = () => {
    watching.wait = 1000;
    listChanged();
  };
  ws.onmessage = (e) => {
    try {
      if (JSON.parse(e.data).type === "changed") listChanged();
    } catch {}
  };
  ws.onclose = () => {
    watching.socket = null;
    watching.timer = setTimeout(watchList, watching.wait * (0.5 + Math.random()));
    watching.wait = Math.min(watching.wait * 2, 30_000);
  };
}
// back on the page, a socket waiting to open again opens now
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && watching.on && !watching.socket) watchList();
});

async function start(open) {
  let me;
  try {
    me = await api("GET", "/api/identities/me");
  } catch (e) {
    if (e.status === 401) return signIn();
    throw e;
  }
  state.me = me;
  await load();
  watchList();
  // the first agent is asked for at home; settings open as asked, chats or
  // not; a guest's first run is a seat, since creating needs one (decision 49)
  const settings = !open && location.pathname === SETTINGS;
  const first = !chats().length && !open && !settings;
  const guest = first && (await api("GET", "/api/ledger").catch(() => null))?.plan === "guest";
  if (first && !guest) return creatingAgent();
  $("first-run").hidden = true;
  $("layout").hidden = false;
  const pick = open ?? (byName(state.current) ? state.current : (shown(chats())[0] ?? chats()[0])?.name);
  if (settings || guest) await openSettings(guest);
  else if (pick) openChat(pick);
  else notice("No chats yet", "Make an agent to start.");
  prewake();
}

start().catch((e) => {
  $("layout").hidden = false;
  notice("This page did not load", e.message);
});
