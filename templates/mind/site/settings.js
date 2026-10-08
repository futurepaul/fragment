// The mind's settings (docs/optchat.md, "The page"): about you, the memory
// and its export, importing chats, and connecting another agent. A screen
// of the page (`#/settings`), never a sheet over it; and, framed by the
// shell's own Settings (`?embed=settings`), the page alone: no rail, no
// header, its height told to the shell so its Settings scrolls as one
// page (docs/api.md, "The mind in the shell").

import { mountImport } from "./import.js";
import { platformOrigin } from "./sheets.js";
import { EMBED, F, S, changed, onChange, startSettings } from "./store.js";
import { copyButton, h, icon, plural } from "./ui.js";
import { topBar } from "./thread.js";

// fragment.json's limit: settings_set's about
const ABOUT_MAX = 8000;
/// Export memory: the log a page at a time, and at most this many pages
/// (16 MiB of log is far fewer entries).
const EXPORT_PAGE = 1000;
const EXPORT_PAGES_MAX = 10_000;
/// The tallest the shell is told the page is.
const HEIGHT_MAX = 20_000;
/// How often the framed page looks again at what it shows and its height.
const SETTLE_MS = 1000;

/// The fragment's label (its host's first label's first part).
const label = () => (location.hostname.includes("--") ? location.hostname.split(".")[0].split("--")[0] : "mind");
/// The fragment's name as the CLI names it, `<label>.<username>`: its
/// host is `<label>--<username>[--<branch>].<zone>`.
const fullName = () => {
  const [l, user] = location.hostname.split(".")[0].split("--");
  return user ? `${l}.${user}` : `${label()}.${S.person?.username || "you"}`;
};

/// The whole log (`export {after?, limit}` → `{entries, next}`, oldest
/// first, paged until `next` is null) as one JSON file, downloaded.
async function exportMemory(button, note) {
  button.disabled = true;
  const entries = [];
  try {
    let after = null;
    for (let k = 0; k < EXPORT_PAGES_MAX; k++) {
      const r = await F.call("export", after === null ? { limit: EXPORT_PAGE } : { after, limit: EXPORT_PAGE });
      entries.push(...(Array.isArray(r.entries) ? r.entries : []));
      note.textContent = `Read ${plural(entries.length, "entry", "entries")}…`;
      // a `next` that does not move on ends it too
      if (r.next === null || r.next === undefined || r.next === after) break;
      after = r.next;
    }
    const file = new Blob([JSON.stringify({ mind: fullName(), exported: new Date().toISOString(), entries }, null, 2)], { type: "application/json" });
    const a = h("a", { href: URL.createObjectURL(file), download: `${fullName()}-${new Date().toISOString().slice(0, 10)}.json` });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 60_000);
    note.textContent = `Saved ${plural(entries.length, "entry", "entries")}.`;
  } catch (e) {
    note.textContent = `Could not export it: ${e.message}`;
  }
  button.disabled = false;
}

/// One way to connect: what it is, the line to give it, and what then.
const way = (name, line, then) => h("div.cmd", null, h("span.cmd-label", { text: name }), h("div.cmd-line", null, h("code", { text: line }), copyButton(line)), then ? h("small.cmd-then", { text: then }) : null);

/// The settings, as a screen: `{el, render}`. `framed`: the shell's
/// Settings frames it, which gives it its title and its scroll.
export function settingsScreen({ framed = false } = {}) {
  const about = h("textarea.field", { rows: 5, maxlength: ABOUT_MAX, placeholder: "Your name, what you do, who's who in your life, how you like answers.", "aria-label": "About you" });
  let typed = false;
  about.addEventListener("input", () => (typed = true));
  const saved = h("span.saved", { "aria-live": "polite" });
  const save = h("button.pill", { type: "button" }, "Save");
  save.addEventListener("click", async () => {
    save.disabled = true;
    saved.className = "saved";
    try {
      await F.call("settings_set", { about: about.value.trim() });
      S.about = about.value.trim();
      typed = false;
      saved.textContent = "Saved";
      setTimeout(() => (saved.textContent = ""), 1600);
    } catch (e) {
      saved.className = "saved bad";
      saved.textContent = `Not saved: ${e.message}`;
    }
    save.disabled = false;
  });

  const memory = h("p.quiet");
  const exportNote = h("span.quiet", { "aria-live": "polite" });
  const exportBtn = h("button.pill", { type: "button" }, icon("download"), "Export memory");
  exportBtn.addEventListener("click", () => exportMemory(exportBtn, exportNote));

  const importing = h("div.import-slot");
  mountImport(importing);

  // the mind's own MCP server (`__mcp`), at its origin
  const url = `${location.origin}/__mcp`;
  const name = label();
  const body = h(
    `div.column.settings${framed ? ".framed" : ""}`,
    null,
    h("section", null, h("h3", { text: "About you" }), h("p.quiet", { text: "Every persona reads this, in every chat." }), about, h("div.row", null, save, saved)),
    h("section", null, h("h3", { text: "Memory" }), memory, h("div.row", null, exportBtn, exportNote)),
    h("section", null, h("h3", { text: "Import chats" }), importing),
    h(
      "section",
      null,
      h("h3", { text: "Connect another agent" }),
      h("p.quiet", { text: "Give another agent this memory: it reads the view, opens a line, dates a message and searches every chat; and it notes things when you let it." }),
      way("claude.ai or Claude Desktop", url, "Settings, Connectors, Add custom connector: this URL. Sign in when it asks; tick “Also let it change things” to let it note."),
      way("Claude Code", `claude mcp add --transport http ${name} ${url}`, "Then /mcp in Claude Code, choose it, and Authenticate."),
      way("Any agent with a shell", `claude mcp add ${name} -- fragment mcp ${fullName()}`, "The fragment CLI, signed in: no sign-in page. Add --write to let it note."),
    ),
    // on its own, the apps are a link away; in the shell, beside it already
    EMBED || framed ? null : h("a.apps-link", { href: `${platformOrigin()}/`, target: "_top" }, icon("grid"), h("span", null, h("b", { text: "Your apps" }), h("small", { text: "Everything else on fragment" })), icon("open")),
  );
  const top = framed ? null : topBar(h("div.top-title", null, h("div.top-text", null, h("div.top-name", { text: "Settings" }))));
  const el = h("section.screen.settings-screen", null, top, framed ? body : h("div.scroll", null, body));

  let said = "";
  return {
    el,
    body,
    render() {
      if (!typed && S.about !== null && about.value !== S.about) about.value = S.about;
      const st = S.status;
      const now = st ? `${plural(st.T ?? 0, "message")}${st.unbuilt ? `, ${st.unbuilt.toLocaleString()} still being summarized` : ", all summarized"}. ${st.hands ? "Your computer is connected." : "No computer is connected yet."}` : "…";
      if (now !== said) memory.textContent = said = now;
    },
  };
}

/// The page framed by the shell's Settings (`?embed=settings`): the
/// settings alone, the theme the shell says, and its height told to the
/// shell whenever it changes.
export function mountSettings(root, fragment) {
  root.classList.add("mind", "settings-frame");
  document.documentElement.classList.add("settings-frame");
  const screen = settingsScreen({ framed: true });
  root.replaceChildren(screen.el);
  addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const d = event.data;
    if (d && d.fragment === "theme" && (d.mode === "light" || d.mode === "dark")) document.documentElement.dataset.theme = d.mode;
  });
  let told = 0;
  const tell = () => {
    const height = Math.min(HEIGHT_MAX, Math.ceil(screen.el.getBoundingClientRect().height));
    if (height === told) return;
    told = height;
    window.parent.postMessage({ fragment: "mind", height }, platformOrigin());
  };
  new ResizeObserver(tell).observe(screen.el);
  onChange(() => screen.render());
  // a frame of another origin scrolled out of view is not drawn (no
  // animation frames, no resize observations), and the shell's Settings
  // often holds this one there: what it shows, and its height, are also
  // checked each second
  setInterval(() => {
    screen.render();
    tell();
  }, SETTLE_MS);
  startSettings(fragment);
  changed();
}
