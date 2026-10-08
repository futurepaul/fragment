// The wiki's page: a page's markdown rendered (`page`, live, so an edit
// anywhere shows at once), the list of pages, Recent changes, who else is
// here (presence), and an editor for whoever may edit. A page is
// `wiki/<name>.md`; its address is `#/<name>`.
import * as fragment from "./__fragment.js";
import { render } from "./markdown.js";

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => Object.assign(document.createElement(tag), cls ? { className: cls } : {}, text === undefined ? {} : { textContent: text });
const HOME = "home";
const nameOf = (path) => path.slice("wiki/".length, -".md".length);
const pathOf = (name) => `wiki/${name}.md`;
const slug = (s) => s.normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase().replace(/[^a-z0-9/]+/g, "-").replace(/^-+|-+$/g, "").replace(/-*\/-*/g, "/");
const titleOf = (name) => {
  const last = name.split("/").pop().replaceAll("-", " ");
  return last.charAt(0).toUpperCase() + last.slice(1);
};
const ago = (at) => {
  const s = (Date.now() - at) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" });
};
const show = (e) => { $("error").textContent = e ? e.message : ""; };

let me = null;
let editor = false;
let pages = [];
let changes = [];
let here = [];
let current = null; // the page shown: {name, result}
let stopPage = null;
let editing = null; // {name, base}: the sha the editor started from (null: a new page)
let writing = null; // a new page's name, whose editor opens once it reads as absent
const names = new Map(); // principal -> a name to show

// ---- who: names for identities, as the fragment's __people knows them ----
async function learn(ids) {
  const want = [...new Set(ids)].filter((id) => id && id.startsWith("id:") && !names.has(id));
  if (!want.length) return;
  want.forEach((id) => names.set(id, "someone"));
  const r = await fetch(`./__people?${want.map((id) => `id=${encodeURIComponent(id)}`).join("&")}`).then((r) => r.json()).catch(() => ({}));
  for (const [id, p] of Object.entries(r.profiles ?? {})) names.set(id, p.name || p.username || "someone");
  draw();
}
const who = (principal) => (principal === me?.principal ? "you" : names.get(principal) ?? "someone");

// ---- where links go ----
function find(target) {
  const t = slug(target.replace(/\.md$/i, "").replace(/^\.?\//, ""));
  const named = pages.map((p) => nameOf(p.path));
  return named.find((n) => slug(n) === t) ?? named.find((n) => slug(n.split("/").pop()) === t.split("/").pop()) ?? null;
}
const ctx = {
  link: (target) => {
    const found = find(target);
    return { href: `#/${encodeURI(found ?? slug(target.replace(/\.md$/i, "")))}`, missing: found === null };
  },
  // a relative image, from the page's own folder
  file: (src) => {
    const dir = current ? pathOf(current.name).split("/").slice(0, -1) : ["wiki"];
    for (const part of src.split("/")) {
      if (part === "..") dir.length > 1 && dir.pop();
      else if (part && part !== ".") dir.push(part);
    }
    return `./__file?path=${encodeURIComponent(dir.join("/"))}`;
  },
};

// ---- the views ----
function route() {
  const name = decodeURIComponent(location.hash.replace(/^#\/?/, "")) || HOME;
  $("side").classList.remove("open");
  if (name === "_recent") return openRecent();
  openPage(name);
}

function openPage(name) {
  if (editing && editing.name !== name) closeEditor();
  if (current?.name === name && stopPage) return draw();
  stopPage?.();
  current = { name, result: undefined };
  stopPage = fragment.live("page", { path: pathOf(name) }, (result) => {
    current.result = result;
    if (writing === name && result === null && editor) openEditor();
    writing = null;
    draw();
  }, show);
  fragment.presence.set({ page: name, editing: false });
  draw();
}

function openRecent() {
  closeEditor();
  stopPage?.();
  stopPage = null;
  current = null;
  fragment.presence.set({ page: "_recent", editing: false });
  draw();
}

function draw() {
  drawList();
  $("recent-link").className = current ? "" : "on";
  if (!current) return drawRecent();
  const { name, result } = current;
  document.title = result ? `${titleOf(name)} · Wiki` : "Wiki";
  // when it last changed, and by whom
  const last = changes.find((c) => c.path === pathOf(name));
  const meta = $("meta");
  meta.replaceChildren();
  if (last) meta.append(last.commit_sha ? `Changed ${ago(last.at)} ${last.by ? `by ${who(last.by)}` : "from the folder"}` : `Saving ${who(last.by)}'s edit…`);
  if (editor && result !== undefined && !editing) {
    const edit = el("button", "quiet", result ? "Edit" : "Write it");
    edit.id = "edit";
    edit.onclick = () => openEditor();
    meta.append(meta.childNodes.length ? " · " : "", edit);
  }
  // who else is on this page
  const others = here.filter((p) => p.id !== me?.id && p.data?.page === name);
  const pills = others.map((p) => el("span", p.data.editing ? "pill editing" : "pill", p.data.editing ? `${who(p.principal)} is editing` : who(p.principal)));
  $("who").replaceChildren(...(pills.length ? ["Also here", ...pills] : []));
  if (editing) return drawEditor(result);
  const view = $("view");
  if (result === undefined) return view.replaceChildren(el("p", "empty", "Loading…"));
  if (result === null) {
    return view.replaceChildren(el("h1", null, titleOf(name)), el("p", "empty", editor ? "There is no page here yet: write it." : "There is no page here yet."));
  }
  const body = render(result.text, ctx);
  if (!/^\s*#\s/.test(result.text.replace(/^---\n[\s\S]*?\n---\n/, ""))) body.prepend(el("h1", null, titleOf(name)));
  view.replaceChildren(body);
}

function drawList() {
  const q = $("filter").value.trim().toLowerCase();
  const shown = pages.map((p) => nameOf(p.path)).filter((n) => !q || n.toLowerCase().includes(q) || titleOf(n).toLowerCase().includes(q));
  $("list").replaceChildren(...shown.map((n) => {
    const a = el("a", current?.name === n ? "on" : "");
    a.href = `#/${encodeURI(n)}`;
    const folder = n.includes("/") ? `${n.slice(0, n.lastIndexOf("/"))}/` : "";
    if (folder) a.append(el("small", null, folder));
    a.append(titleOf(n));
    const li = el("li");
    li.append(a);
    return li;
  }));
}

function drawRecent() {
  document.title = "Recent changes · Wiki";
  $("meta").replaceChildren("Every change to a page, the newest first");
  $("who").replaceChildren();
  const list = el("ul");
  list.id = "changes";
  for (const c of changes) {
    const li = el("li");
    const what = el("div");
    const page = el(c.gone ? "s" : "a", null, titleOf(nameOf(c.path)));
    if (!c.gone) page.href = `#/${encodeURI(nameOf(c.path))}`;
    what.append(page, " ", el("small", null, `${c.by ? `by ${who(c.by)}` : "from the folder"}${c.commit_sha ? ` · ${c.commit_sha.slice(0, 7)}` : " · saving…"}${c.gone ? " · removed" : ""}`));
    li.append(el("time", null, ago(c.at)), what);
    list.append(li);
  }
  $("view").replaceChildren(el("h1", null, "Recent changes"), changes.length ? list : el("p", "empty", "No changes yet."));
}

// ---- editing ----
function openEditor() {
  const { name, result } = current;
  editing = { name, base: result?.sha ?? null };
  $("text").value = result ? result.text : `# ${titleOf(name)}\n\n`;
  $("editor").hidden = false;
  $("view").hidden = true;
  fragment.presence.set({ page: name, editing: true });
  draw();
  $("text").focus();
}

function closeEditor() {
  if (!editing) return;
  editing = null;
  $("editor").hidden = true;
  $("view").hidden = false;
  if (current) fragment.presence.set({ page: current.name, editing: false });
}

// what changed under the editor, said above it
function drawEditor(result) {
  const moved = (result?.sha ?? null) !== editing.base;
  const note = $("edit-note");
  note.className = moved ? "warn" : "";
  note.textContent = moved ? "This page changed while you were editing; saving replaces that change." : `Editing ${pathOf(editing.name)}`;
  $("save").textContent = moved ? "Save anyway" : "Save";
}

$("editor").onsubmit = async (e) => {
  e.preventDefault();
  const text = $("text").value;
  // the same text again commits nothing
  if (current.result && text === current.result.text) {
    closeEditor();
    return draw();
  }
  try {
    await fragment.call("save", { path: pathOf(editing.name), text });
    closeEditor();
    show();
    draw();
  } catch (err) {
    show(err);
  }
};
$("cancel").onclick = () => { closeEditor(); draw(); };

$("new").onsubmit = (e) => {
  e.preventDefault();
  const title = $("new-title").value.trim();
  if (!slug(title)) return;
  $("new-title").value = "";
  const name = find(title) ?? slug(title);
  if (!find(title)) writing = name;
  location.hash = `#/${encodeURI(name)}`;
};

$("filter").oninput = drawList;
$("menu").onclick = () => $("side").classList.toggle("open");
window.addEventListener("hashchange", route);

fragment.live("pages", {}, (r) => { pages = r.pages; draw(); }, show);
fragment.live("recent", {}, (r) => {
  changes = r.changes;
  learn(changes.map((c) => c.by));
  draw();
}, show);
fragment.presence.on((list) => {
  here = list;
  learn(list.map((p) => p.principal));
  draw();
});
fragment.me().then((m) => {
  me = m;
  editor = m.role === "editor" || m.role === "owner";
  $("new").hidden = !editor;
  draw();
});
route();
