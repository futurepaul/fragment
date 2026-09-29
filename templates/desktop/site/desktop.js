// The desktop: its owner's fragments side by side. Chats are chat
// fragments (the middle column shows the open one), apps and computers
// open as panes in the viewer, a file through its own fragment's __file. Each
// frame is this page's `__frame`: the platform signs the frame in on its
// fragment's origin, for this page only, so the desktop's code holds no
// authority over any of them. Its platform powers are its owner's list
// (`__fragments`) and those frames, which fragment.json asks for and its
// owner allows (making it with the platform's form, or its share sheet;
// the list says which, as `frame`, and without it the desktop says so in
// place of its panes). Sharing is the platform's too: a row's Share item
// opens the platform's share sheet, in a dialog here (`__share` signs its
// frame in on that one sheet, as `__frame` does on a fragment), or, while
// this page may not frame, in a window of its own; this page can script
// neither. The list says who else is in each, for the badges. An app pane's
// header says the same, with a Share button, and who has the app open now
// (`__presence`, asked while this page is in view).
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
  more: '<circle cx="5" cy="12" r="1.3"/><circle cx="12" cy="12" r="1.3"/><circle cx="19" cy="12" r="1.3"/>',
  people: '<circle cx="9" cy="8" r="3.2"/><path d="M3 19a6 6 0 0 1 12 0"/><path d="M16 5.2a3.2 3.2 0 0 1 0 5.6M18 19a6 6 0 0 0-2.5-4.9"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>',
  share: '<path d="M12 15V3M7 8l5-5 5 5"/><path d="M5 12v7a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-7"/>',
  computer: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;
const CURRENT = "desktop.chat.v1";
// the platform's templates an app can start from
const CATALOG = [
  { template: "todo", name: "Todo", about: "A list, live for everyone who has it open." },
  { template: "inbox", name: "Inbox", about: "Webhooks in, a job to read each one." },
  { template: "blank", name: "Blank", about: "One page to start from." },
];

// `frame`: whether this page may show the owner's fragments inside it
// (`null`: its fragment.json does not ask), as the platform last said;
// `me`: the owner's principal, once this page's socket said hello
const state = { fragments: [], chats: [], current: store.get(CURRENT, null), frames: new Map(), frame: undefined, me: null };

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
// A frame of one of the owner's fragments, signed in there by the platform.
const framed = (name, path = "/") => `__frame?name=${encodeURIComponent(name)}&return=${encodeURIComponent(path)}`;
const fresh = (prefix) => `${prefix}-${crypto.getRandomValues(new Uint32Array(1))[0].toString(36).slice(0, 5)}`;

function notice(title, text, ...actions) {
  const n = $("notice");
  n.replaceChildren(el("strong", null, title));
  if (text) n.append(text);
  for (const a of actions) n.append(" ", a);
  n.hidden = false;
}

// ---- when this page may not show them: said in place of every frame ----
function shareButton() {
  const b = el("button", "allow", "Open its share sheet");
  b.type = "button";
  b.onclick = () => share(state.self);
  return b;
}
// A tab of its own signs in there as any visit does (its own `__signin`).
function inTab(name) {
  const a = el("a", "in-tab", `Open ${label(name)} in a tab`);
  const url = byName(name)?.url;
  a.href = url ? new URL(`__signin?return=${encodeURIComponent("/")}`, url).href : "#";
  a.target = "_blank";
  a.rel = "noopener";
  return a;
}
// What stands in for the frames, and why (`frame` as the platform said).
function why(frame) {
  return frame === null
    ? ["This desktop does not show your fragments", "Its fragment.json does not ask to show them inside it. You can still open each in a tab."]
    : ["Let this desktop show your fragments", "It shows your chats and apps inside it, signed in as you, only once you allow that in its share sheet."];
}
// The middle column's notice, in place of a chat.
function cannotFrame(name, frame = state.frame) {
  const [title, text] = why(frame);
  for (const f of state.frames.values()) f.hidden = true;
  notice(title, text, ...(frame === null ? [] : [shareButton()]), ...(name ? [inTab(name)] : []));
}
// A pane's body, in place of an app or a file.
function cannotFrameNote(name, frame = state.frame) {
  const [title, text] = why(frame);
  const note = el("div", "pane-note blocked");
  note.append(el("strong", null, title), el("p", null, text));
  const actions = el("p");
  if (frame !== null) actions.append(shareButton(), " ");
  if (name) actions.append(inTab(name));
  note.append(actions);
  return note;
}
// A frame of `name`, or, when this page may not frame it, a note saying so.
// A frame still on this page's origin once it loads is `__frame`'s refusal
// (the grant was stopped after the list said it held): it never shows, and
// the list is asked again (a changed answer reloads the page).
function frameOf(src, title, name) {
  if (state.frame !== true) return cannotFrameNote(name);
  const frame = el("iframe");
  frame.title = title;
  frame.src = src;
  frame.addEventListener("load", () => {
    let refused = false;
    try {
      refused = !!frame.contentDocument?.location.pathname.endsWith("/__frame");
    } catch {}
    if (!refused) return;
    if (frame.parentElement === $("frames")) {
      state.frames.delete(frame.dataset.fragment);
      frame.remove();
      if (state.current === frame.dataset.fragment) cannotFrame(frame.dataset.fragment, false);
    } else {
      frame.replaceWith(cannotFrameNote(name, false));
    }
    refresh();
  });
  return frame;
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
    for (const row of document.querySelectorAll("#apps .row[data-key], #computers .row[data-key]")) row.classList.toggle("open", keys.includes(row.dataset.key));
    if (!keys.length) renderQuickOpen();
    look();
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

// ---- sharing: the platform's sheet, and who else is in each ----
// Who else is in a fragment, as a badge: people besides its owner and
// their agents, and whether anyone at all may open it. (A link is every
// fragment's default, so it is not badged.) `sharing` is the owner's list's
// (none yet for a fragment that has not sent it: no badge).
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

// Who may open a fragment, in words: an app pane's header says it after
// the row's badges, and who else is in it in its tooltip.
const OPENS = { members: "Only people added", link: "Anyone with the link", public: "Public" };
function renderSharing(box, f) {
  const { guests = 0, visibility } = f?.sharing ?? {};
  box.replaceChildren(...badges(f ?? {}));
  if (OPENS[visibility]) box.append(el("span", "words", OPENS[visibility]));
  box.title = [OPENS[visibility], guests > 0 && `Shared with ${guests}`].filter(Boolean).join(" · ");
}

// The platform's share sheet. One of the owner's own, while this page may
// frame their fragments, opens in a dialog here: its frame is `__share`,
// which signs it in on that sheet alone, for this page alone (and the sheet
// never shows its frame grant). It says its height, and when it is done
// (Done, Escape), to this origin only; a message counts only from its
// frame, on the platform's origin. Closed, the list is read again, for the
// badges and the panes' headers. A browser that keeps no cookie in a frame
// of another site (the sheet is on the platform's) shows the sheet signed
// out, which never shows here: with no word from it after a while, the
// dialog offers the sheet's own window instead. Otherwise the sheet opens
// in a small window of its own. `noopener`: this page keeps no handle on it
// (and the sheet severs any opener anyway).
const sheet = $("sheet");
const SHEET_SHOWN_MS = 5000;
let sheetFrame = null, sheetOrigin = null, sheetWait = 0;
function share(name) {
  const f = byName(name);
  if (!f?.share) return notice("Sharing is not available", "This platform does not offer a share sheet.");
  if (state.frame === true && f.role === "owner") {
    sheetOrigin = new URL(f.share).origin;
    sheetFrame = el("iframe");
    sheetFrame.title = `Share ${label(name)}`;
    sheetFrame.allow = "clipboard-write";
    sheetFrame.src = `__share?name=${encodeURIComponent(name)}`;
    // until the sheet says it showed, the dialog says it is opening
    sheet.classList.add("opening");
    sheet.replaceChildren(el("p", "sheet-opening", "Opening the share sheet…"), sheetFrame);
    clearTimeout(sheetWait);
    sheetWait = setTimeout(() => sheet.open && sheetFrame && sheetInWindow(f), SHEET_SHOWN_MS);
    return sheet.showModal();
  }
  shareWindow(f);
}
function shareWindow(f) {
  const w = 480, h = 720;
  const left = Math.max(0, screenX + (outerWidth - w) / 2), top = Math.max(0, screenY + (outerHeight - h) / 3);
  window.open(f.share, "_blank", `popup,noopener,width=${w},height=${h},left=${left},top=${top}`);
}
// the dialog, when the sheet did not show in it: its own window, from a click
function sheetInWindow(f) {
  sheetFrame = null;
  sheet.classList.remove("opening");
  const note = el("div", "sheet-note");
  note.append(el("strong", null, "The share sheet did not open here"), el("p", null, "This browser keeps no sign-in inside a frame from another site. It opens in a window of its own instead."));
  const open = el("button", "go", "Open in a window");
  open.type = "button";
  open.onclick = () => { sheet.close(); shareWindow(f); };
  const cancel = el("button", null, "Cancel");
  cancel.type = "button";
  cancel.onclick = () => sheet.close();
  const actions = el("div", "update-actions");
  actions.append(open, cancel);
  note.append(actions);
  sheet.replaceChildren(note);
}
addEventListener("message", (e) => {
  if (!sheet.open || e.source !== sheetFrame?.contentWindow || e.origin !== sheetOrigin) return;
  clearTimeout(sheetWait);
  sheet.classList.remove("opening");
  if (Number.isFinite(e.data?.height)) sheetFrame.style.height = `${e.data.height}px`;
  if (e.data?.share === "done") sheet.close();
});
sheet.onclick = (e) => { if (e.target === sheet) sheet.close(); };
sheet.onclose = () => {
  clearTimeout(sheetWait);
  sheet.classList.remove("opening");
  sheet.replaceChildren();
  sheetFrame = null;
  load().catch(() => {});
};

// One `…` menu, for whichever row asked.
const menu = $("menu");
let menuFor = null;
function closeMenu() {
  if (!menuFor) return;
  menuFor.button.setAttribute("aria-expanded", "false");
  menuFor = null;
  menu.hidden = true;
}
function openMenu(button, name) {
  const again = menuFor?.button === button;
  closeMenu();
  if (again) return;
  menuFor = { button, name };
  button.setAttribute("aria-expanded", "true");
  menu.hidden = false;
  const r = button.getBoundingClientRect();
  menu.style.top = `${Math.min(r.bottom + 4, innerHeight - menu.offsetHeight - 8)}px`;
  menu.style.left = `${Math.max(8, Math.min(r.left, innerWidth - menu.offsetWidth - 8))}px`;
  $("menu-share").focus();
}
$("menu-share").innerHTML = svg("share");
$("menu-share").append("Share…");
$("menu-share").onclick = () => { const name = menuFor?.name; closeMenu(); if (name) share(name); };
addEventListener("pointerdown", (e) => { if (menuFor && !menu.contains(e.target) && e.target !== menuFor.button && !menuFor.button.contains(e.target)) closeMenu(); });
addEventListener("keydown", (e) => { if (e.key === "Escape") closeMenu(); });
addEventListener("blur", closeMenu);
addEventListener("resize", closeMenu);

// A sidebar row with its `…` button beside it.
function item(row, name) {
  const wrap = el("div", "item");
  const more = el("button", "icon-button small more");
  more.type = "button";
  more.title = "More";
  more.setAttribute("aria-label", `More for ${label(name)}`);
  more.setAttribute("aria-haspopup", "menu");
  more.setAttribute("aria-expanded", "false");
  more.dataset.fragment = name;
  more.innerHTML = svg("more");
  more.onclick = (e) => { e.stopPropagation(); openMenu(more, name); };
  wrap.append(row, more);
  return wrap;
}

// ---- chats: each a chat fragment, shown in the middle column ----
// A chat is a fragment New chat named (`chat-…`), wherever it was made; the
// ones this desktop made come first, newest first.
function chatNames() {
  const made = state.chats.filter(byName);
  const others = state.fragments.filter((f) => label(f.name).startsWith("chat-") && f.name !== state.self && !made.includes(f.name)).map((f) => f.name);
  return [...made, ...others];
}

function renderChats() {
  const chats = chatNames();
  $("chats").replaceChildren(...(chats.length ? chats.map((name) => {
    const row = el("button", `row${name === state.current ? " active" : ""}`);
    row.dataset.key = `chat:${name}`;
    row.innerHTML = svg("chat");
    row.append(el("span", "label", label(name)), ...badges(byName(name)));
    row.onclick = () => { openChat(name); leaveSidebar(); };
    return item(row, name);
  }) : [el("div", "empty-row", "No chats yet")]));
}

function openChat(name) {
  const f = byName(name);
  if (!f) return;
  state.current = name;
  store.set(CURRENT, name);
  $("chat-title").textContent = label(name);
  if (state.frame !== true) {
    renderChats();
    return cannotFrame(name);
  }
  $("notice").hidden = true;
  if (!state.frames.has(name)) {
    const frame = frameOf(framed(name), label(name), name);
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

// A computer is a fragment New computer named (`computer-…`, from the pet
// template), wherever it was made: a pane like an app, awake while its page
// is open (the platform's rule, on its owner's budget) and asleep a few
// minutes after its pane closes.
const isComputer = (name) => label(name).startsWith("computer-") && name !== state.self;
const iconOf = (name) => (isComputer(name) ? paneIcon("computer") : appIcon(label(name)));

const apps = () => {
  const chats = chatNames();
  return state.fragments.filter((f) => f.name !== state.self && !chats.includes(f.name) && !isComputer(f.name));
};

function appRow(f) {
  const row = el("button", `row${viewer.keys.includes(`app:${f.name}`) ? " open" : ""}`);
  row.dataset.key = `app:${f.name}`;
  row.append(iconOf(f.name), el("span", "label", label(f.name)), ...badges(f));
  if (f.role !== "owner") row.append(el("span", "meta", f.role));
  row.onclick = () => { openApp(f.name); leaveSidebar(); };
  return item(row, f.name);
}

function renderApps() {
  const list = apps();
  $("apps").replaceChildren(...(list.length ? list.map(appRow) : [el("div", "empty-row", "No apps yet")]));
  const computers = state.fragments.filter((f) => isComputer(f.name));
  $("computers").replaceChildren(...(computers.length ? computers.map(appRow) : [el("div", "empty-row", "No computers yet")]));
}

// A new computer: a pet fragment of the owner's, opened as a pane (which wakes it).
$("new-computer").onclick = async () => {
  $("new-computer").disabled = true;
  try {
    const made = await platform({ label: fresh("computer"), template: "pet" });
    await load();
    openApp(made.name);
    leaveSidebar();
  } catch (e) {
    notice("A new computer could not be made", e.message);
  } finally {
    $("new-computer").disabled = false;
  }
};

function openApp(name) {
  const f = byName(name);
  if (!f) return;
  const frame = frameOf(framed(name), label(name), name);
  const status = el("span", "pane-sharing");
  status.dataset.fragment = name;
  renderSharing(status, f);
  // who has it open: the platform says so for the owner's own alone
  const here = f.role === "owner" ? el("span", "presence") : undefined;
  if (here) {
    here.hidden = true;
    here.dataset.fragment = name;
  }
  show({
    key: `app:${name}`, title: label(name), subtitle: f.role === "owner" ? undefined : f.role, icon: iconOf(name), body: frame, status, presence: here,
    actions: [
      { icon: ICON.share, title: "Share…", onClick: () => share(name) },
      { icon: ICON.folder, title: "Files", onClick: () => openTree(name) },
      { icon: ICON.reload, title: "Reload", onClick: () => { if (frame.isConnected && frame.tagName === "IFRAME") frame.src = framed(name); } },
    ],
  });
}

// A fragment's files: its own __files viewer, whose bar asks this page to
// open a file as a pane (the message listener below).
function openTree(name) {
  const f = byName(name);
  if (!f) return;
  show({ key: `tree:${name}`, title: label(name), subtitle: "files", icon: paneIcon("folder"), body: frameOf(framed(name, "/__files"), `${label(name)} files`, name) });
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
    body: frameOf(framed(f.name, `/__file?path=${encodeURIComponent(path)}`), path, f.name),
    actions: [{ icon: ICON.folder, title: `All files in ${label(f.name)}`, onClick: () => openTree(f.name) }],
  });
}

// A frame whose browser keeps it signed out shows a link to open it in a
// tab; said once here too.
let blocked = false;
// Only a frame of one of the owner's own fragments may ask for a file.
addEventListener("message", (e) => {
  const ask = e.data;
  if (ask?.fragment === "signin-blocked" && !blocked && state.fragments.some((f) => new URL(f.url).origin === e.origin)) {
    blocked = true;
    return notice("Your browser keeps your fragments signed out inside this page", "Open each in a tab of its own from its pane.");
  }
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

// ---- presence: who has each open app open now, one `__presence` read for
// every app pane (the platform's most: 8), while this page is in view ----
const PRESENCE_MS = 12000;
const PRESENCE_MAX = 8;
const FACES = 3;
const profiles = new Map(); // id -> { kind, username, picture } ({}: the platform knows none)

// Usernames and pictures for the ids not seen yet, asked once each (64 at
// most a read, the platform's most).
async function lookUp(ids) {
  const fresh = [...new Set(ids)].filter((id) => !profiles.has(id)).slice(0, 64);
  if (!fresh.length) return;
  const r = await fetch(`__people?${fresh.map((id) => `id=${encodeURIComponent(id)}`).join("&")}`, { credentials: "same-origin" });
  if (!r.ok) return;
  const { profiles: found = {} } = await r.json();
  for (const id of fresh) profiles.set(id, found[id] ?? {});
}

const bot = (p) => p.kind === "agent" || p.kind === "computer";
const nameOf = (p) => (bot(p) ? `${p.username ?? "someone"}'s ${p.kind}` : (p.username ?? "someone"));
function face(p) {
  if (!p.picture) return el("span", "av", bot(p) ? "✦" : (p.username?.[0]?.toUpperCase() ?? "?"));
  const img = el("img", "av");
  img.src = p.picture;
  img.alt = "";
  return img;
}

// Who else has an app open (its owner has it open here): up to three
// faces, then +N; a narrow pane shows only how many (desktop.css).
function renderPresence(box, here) {
  const others = (here?.people ?? []).filter((id) => id !== state.me).map((id) => profiles.get(id) ?? {});
  const guests = here?.anonymous ?? 0;
  const n = others.length + guests;
  const shown = others.slice(0, FACES);
  const count = el("span", "count");
  count.innerHTML = svg("people");
  count.append(String(n));
  box.replaceChildren(...shown.map(face), ...(n > shown.length ? [el("span", "av more", `+${n - shown.length}`)] : []), count);
  box.title = `Here now: ${[others.map(nameOf).join(", "), guests && `${guests} ${guests === 1 ? "guest" : "guests"}`].filter(Boolean).join(" and ")}`;
  box.hidden = n === 0;
}

async function lookAround() {
  const boxes = [...document.querySelectorAll(".presence[data-fragment]")].slice(0, PRESENCE_MAX);
  if (document.hidden || !state.me || !boxes.length) return;
  const r = await fetch(`__presence?${boxes.map((b) => `name=${encodeURIComponent(b.dataset.fragment)}`).join("&")}`, { credentials: "same-origin" });
  if (!r.ok) return;
  const { presence = {} } = await r.json();
  // a face whose name did not come shows as someone's
  await lookUp(Object.values(presence).flatMap((p) => p.people)).catch(() => {});
  for (const b of boxes) renderPresence(b, presence[b.dataset.fragment]);
}
// panes that open or close together are one read
let looking = 0;
function look() {
  clearTimeout(looking);
  looking = setTimeout(() => lookAround().catch(() => {}), 200);
}
setInterval(look, PRESENCE_MS);
document.addEventListener("visibilitychange", look);

// ---- template updates: when the desktop template has newer files than
// this desktop's own (`__template`, its owner's alone), a pill at the foot
// of the sidebar, asked on load and every 10 minutes while in view. Its
// Update commits the template's files over this desktop's (the platform
// deploys them; its chats and apps are other fragments), then reloads ----
const TEMPLATE_MS = 10 * 60 * 1000;
async function templateStatus(init) {
  const r = await fetch("__template", { credentials: "same-origin", ...init });
  const out = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(out.message || r.statusText);
  return out;
}
async function checkTemplate() {
  const { template, upToDate } = await templateStatus().catch(() => ({}));
  $("update").hidden = !template || upToDate !== false;
}
function confirmUpdate(open) {
  $("update-confirm").hidden = !open;
  $("update-pill").setAttribute("aria-expanded", String(open));
  if (open) $("update-go").focus();
}
$("update-pill").onclick = () => confirmUpdate($("update-confirm").hidden);
$("update-cancel").onclick = () => confirmUpdate(false);
$("update-go").onclick = async () => {
  const [go, cancel] = [$("update-go"), $("update-cancel")];
  go.disabled = cancel.disabled = true;
  go.textContent = "Updating…";
  try {
    await templateStatus({ method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
    location.reload();
  } catch (e) {
    $("update-text").textContent = `The update did not finish: ${e.message}`;
    go.textContent = "Try again";
    go.disabled = cancel.disabled = false;
  }
};
checkTemplate();
setInterval(() => { if (!document.hidden) checkTemplate(); }, TEMPLATE_MS);

// ---- loading ----
let seen = "";
async function load() {
  const [{ fragments, frame = null }, { chats }] = await Promise.all([platform(), fragment.call("chats", {})]);
  // allowed or stopped since this page showed: it shows again, as it now may
  if (state.frame !== undefined && frame !== state.frame) return location.reload();
  state.frame = frame;
  const now = JSON.stringify([fragments, chats]);
  if (now === seen) return;
  seen = now;
  // the rows are made again: a menu open on one closes
  closeMenu();
  state.fragments = fragments;
  state.chats = chats;
  // this desktop is one of them: the one this page is under
  state.self = fragments.find((f) => location.href.startsWith(f.url))?.name ?? null;
  renderChats();
  renderApps();
  renderQuickOpen();
  for (const box of document.querySelectorAll(".pane-sharing[data-fragment]")) renderSharing(box, byName(box.dataset.fragment));
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
  fragment.me().then((hello) => { state.me = hello.principal; look(); });
  const chats = chatNames();
  if (chats.length) openChat(chats.includes(state.current) ? state.current : chats[0]);
  else if (state.frame !== true) cannotFrame();
  else notice("No chats yet", "Start one with New chat.");
  restoreViewer();
}

// New fragments (an app your agent made) show up: asked again when the
// page comes back into view, and every few seconds while it is in view.
const REFRESH_MS = 5000;
const refresh = () => { if (!document.hidden && state.self) load().catch(() => {}); };
addEventListener("focus", refresh);
document.addEventListener("visibilitychange", refresh);
setInterval(refresh, REFRESH_MS);

start();
