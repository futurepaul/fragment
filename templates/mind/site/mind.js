// The mind template's page (docs/optchat.md, "The page"): one memory,
// every chat. One screen at a time:
//
//   - the rail: the personas (the default marked; one tap starts a chat
//     with it), New chat, Search, Topics, Memory, and a few recent threads
//     as one-line summaries (it holds no list of chats: Search and Topics
//     are how old ones are found, and nothing needs finding to be
//     remembered); a drawer on a phone;
//   - the center: a thread, New chat, Search (with Everything), Topics, a
//     topic, or Memory;
//   - the right panel (toggled): what the mind did in this thread, its
//     topics and hand-offs, and the computer's and memory's state.
//
// Routes are the hash: #/new, #/t/<thread>, #/search?q=, #/topics,
// #/topic/<id>, #/memory. The fragment (./__fragment.js, or ./mock.js in
// the page's dev mode) is handed in by index.html.
//
// In the shell (`?embed=shell`, framed by the platform's page: the
// shell's `openChat`), the shell's sidebar is the rail and its topbar the
// header, so the page shows neither. It tells the shell what the rail
// would show, whenever that changes: `{fragment: "mind", state: {route,
// screen, title, face, personas, default, chosen, recent, topics,
// memory}}`, to the platform's origin only. It takes the shell's asks
// from the page around it only: `go` (a route), `new` (New chat with a
// persona, null the default), `sheet` ("persona", with `id` or none for a
// new one; "settings") and `panel` (true, false or "toggle"). It asks the
// shell for the computer's screen (`screen: true`) and to open a link to
// one of the person's fragments there (`app: <url>`).

import { memoryScreen, searchScreen, topicScreen, topicsScreen } from "./browse.js";
import { avatar, go } from "./pieces.js";
import { personaSheet, platformOrigin, settingsSheet } from "./sheets.js";
import { EMBED, S, busy, changed, currentPersona, handOff, handRunning, onChange, persona, start } from "./store.js";
import { newScreen, threadScreen } from "./thread.js";
import { WEB_TOOLS, ago, cleanSummary, firstLine, h, icon, iconButton, parseTool, plural, reconcile } from "./ui.js";

const WIDE = "(min-width: 1280px)";
const PANEL_KEY = "mind.panel";
/// A route the shell may ask for: a hash of the page's own, bounded.
const ROUTE = /^#\/[A-Za-z0-9_\-/?=&%.+]{0,300}$/;

function route() {
  const raw = location.hash.replace(/^#/, "") || "/new";
  const [path, query] = raw.split("?");
  const parts = path.split("/").filter(Boolean);
  const q = new URLSearchParams(query ?? "");
  switch (parts[0]) {
    case "t":
      return parts[1] ? { name: "thread", id: parts[1], key: `t:${parts[1]}` } : { name: "new", key: "new" };
    case "search":
      return { name: "search", q: q.get("q") ?? "", key: "search" };
    case "topics":
      return { name: "topics", key: "topics" };
    case "topic":
      return { name: "topic", id: decodeURIComponent(parts[1] ?? ""), key: `topic:${parts[1]}` };
    case "memory":
      return { name: "memory", key: "memory" };
    default:
      return { name: "new", key: "new" };
  }
}

export function mount(root, fragment) {
  root.classList.add("mind");
  const rail = h("aside.rail", { "aria-label": "Mind" });
  const scrim = h("div.scrim", { "aria-hidden": "true" });
  const main = h("main.main");
  const panel = h("aside.panel", { "aria-label": "What it did here" });
  const toast = h("div.toast", { role: "alert", hidden: true });
  const app = h(`div.app${EMBED ? ".embedded" : ""}`, null, rail, scrim, main, panel, toast);
  root.replaceChildren(app);

  let screen = null;
  let current = null;
  let panelOpen = localStorage.getItem(PANEL_KEY) !== "0" && matchMedia(WIDE).matches;

  // ---- the rail ----
  const railHead = h("div.rail-head", null, h("a.brand", { href: "#/new", "aria-label": "New chat" }, h("span.orb", { "aria-hidden": "true" }), h("span", { text: "mind" })), h("span.grow"), iconButton("compose", "New chat", () => newChat()), iconButton("x", "Close the menu", () => drawer(false), "only-narrow"));
  const personas = h("div.persona-list");
  const personasSec = h("section.rail-sec", null, h("div.rail-label", null, h("span", { text: "Personas" }), iconButton("plus", "New persona", () => personaSheet(null), "tiny")), personas);
  const nav = h("nav.rail-nav");
  const recent = h("div.recent-list");
  const recentSec = h("section.rail-sec.recent", null, h("div.rail-label", null, h("span", { text: "Recent" })), recent, h("a.rail-more", { href: "#/search" }, "Everything", icon("chevron")));
  const foot = h("div.rail-foot");
  rail.append(railHead, h("div.rail-scroll", null, personasSec, nav, recentSec), foot);

  function newChat(personaId) {
    if (personaId !== undefined) S.chosen = personaId;
    go("/new");
    drawer(false);
    if (current?.name === "new") screen?.focus();
    changed();
  }

  const navItem = (name, href, ic, label, extra) => {
    const r = current ?? route();
    const on = r.name === name || (name === "topics" && r.name === "topic");
    return h(`a.nav-item${on ? ".on" : ""}`, { href, "aria-current": on ? "page" : null }, icon(ic), h("span", { text: label }), extra ? h("span.nav-extra", { text: extra }) : null);
  };

  // the recent threads, as the rail and the shell's sidebar show them:
  // each as what it came to (its summary), not a subject line
  function recentThreads() {
    return S.recent
      .filter((id) => S.threads.has(id))
      .slice(0, 12)
      .map((id) => {
        const t = S.threads.get(id);
        const sum = cleanSummary(t.summary);
        return { id, t, sum, line: sum || firstLine(t.title, 90), working: busy(id), hands: [...S.tasks.values()].some((k) => k.thread === id && handRunning(k)) };
      });
  }

  // ---- in the shell: what the rail would show, told to the shell ----
  function screenTitle(r) {
    if (r.name === "thread") return S.threads.get(r.id)?.title || "New chat";
    if (r.name === "topic") return (S.topics ?? []).find((t) => t.id === r.id)?.name ?? "Topic";
    return { new: "New chat", search: "Search", topics: "Topics", memory: "Memory" }[r.name] ?? "Mind";
  }
  let told = "";
  function tellShell() {
    const r = current ?? route();
    const p = (r.name === "thread" ? persona(S.threads.get(r.id)?.persona) : null) ?? currentPersona();
    const st = S.status;
    const state = {
      route: location.hash || "#/new",
      screen: r.name,
      title: screenTitle(r),
      face: p.emoji || "🌿",
      personas: S.personas.map((x) => ({ id: x.id, name: x.name, emoji: x.emoji, hands: !!x.hands })),
      default: S.defaultPersona,
      chosen: S.chosen ?? null,
      recent: recentThreads().map((x) => ({ id: x.id, line: x.line, title: x.t.title ?? "", face: persona(x.t.persona)?.emoji ?? "", when: ago(x.t.last ?? x.t.started), busy: x.working || x.hands })),
      topics: S.topics ? S.topics.length : null,
      memory: st?.unbuilt ? "…" : st?.T ? compact(st.T) : "",
    };
    const said = JSON.stringify(state);
    if (said === told) return;
    told = said;
    window.parent.postMessage({ fragment: "mind", state }, platformOrigin());
  }
  const toShell = (ask) => window.parent.postMessage({ fragment: "mind", ...ask }, platformOrigin());

  function renderRail() {
    const chosen = currentPersona();
    reconcile(
      personas,
      S.personas.map((p) => {
        const on = p.id === chosen.id;
        const isDefault = p.id === S.defaultPersona;
        return {
          key: p.id,
          sig: JSON.stringify([p.name, p.emoji, p.hands, on, isDefault]),
          make: () =>
            h(
              `div.persona-row${on ? ".on" : ""}`,
              null,
              h("button.persona-pick", { type: "button", onclick: () => newChat(p.id), title: `New chat with ${p.name}` }, avatar(p, "sm"), h("span.persona-name", { text: p.name }), isDefault ? h("span.badge", { text: "default" }) : null, p.hands ? h("span.badge.hands", { title: "Can use your computer" }, icon("monitor")) : null),
              iconButton("pencil", `Edit ${p.name}`, () => personaSheet(p), "tiny persona-edit"),
            ),
        };
      }),
      { quiet: true },
    );
    const st = S.status;
    const memory = st?.unbuilt ? "…" : st?.T ? compact(st.T) : "";
    const topicsCount = S.topics ? String(S.topics.length) : "";
    once(nav, [current?.name, memory, topicsCount], () => [
      navItem("new", "#/new", "compose", "New chat"),
      navItem("search", "#/search", "search", "Search", navigator.platform?.startsWith("Mac") ? "⌘K" : "Ctrl K"),
      navItem("topics", "#/topics", "hash", "Topics", topicsCount),
      navItem("memory", "#/memory", "layers", "Memory", memory),
    ]);
    const list = recentThreads();
    reconcile(
      recent,
      list.map(({ id, t, sum, line, working, hands }) => {
        const on = current?.name === "thread" && current.id === id;
        return {
          key: id,
          sig: JSON.stringify([line, sum, on, working, hands, ago(t.last ?? t.started), persona(t.persona)?.emoji]),
          make: () =>
            h(
              `a.recent-row${on ? ".on" : ""}${working ? ".busy" : ""}`,
              { href: `#/t/${id}`, title: t.title || line },
              h("span.recent-face", { "aria-hidden": "true", text: persona(t.persona)?.emoji ?? "·" }),
              h("span.recent-text", { text: line || "A chat" }),
              hands ? icon("monitor", "recent-hands") : working ? h("span.pulse-dot") : h("time", { text: ago(t.last ?? t.started) }),
            ),
        };
      }),
      { quiet: true },
    );
    recentSec.hidden = !list.length;
    const name = S.person?.name || "You";
    once(foot, [name, S.person?.picture], () => [
      h(
        "button.me",
        { type: "button", onclick: () => settingsSheet(), "aria-label": "Settings" },
        S.person?.picture ? h("img.me-pic", { src: S.person.picture, alt: "" }) : h("span.me-pic.letter", { text: name.slice(0, 1).toUpperCase() }),
        h("span.me-name", { text: name }),
        icon("settings"),
      ),
      h("a.icon-btn", { href: `${platformOrigin()}/`, target: "_top", title: "Your apps", "aria-label": "Your apps" }, icon("grid")),
    ]);
  }

  // ---- the right panel: what it did here ----
  function renderPanel() {
    const r = current;
    const show = r?.name === "thread";
    app.classList.toggle("panel-open", show && panelOpen);
    app.classList.toggle("has-panel", show);
    if (!show) return;
    const t = S.threads.get(r.id);
    const p = persona(t?.persona) ?? currentPersona();
    const box = S.msgs.get(r.id);
    const msgs = box ? [...box.byI.values()] : [];
    const named = msgs.filter((m) => m.kind === "tool").map((m) => parseTool(m.text).name).filter((n) => n !== "computer");
    const webLooks = named.filter((n) => WEB_TOOLS.has(n)).length;
    const looks = named.length - webLooks;
    const tasks = [...S.tasks.values()].filter((k) => k.thread === r.id).sort((a, b) => (a.started ?? 0) - (b.started ?? 0));
    const turn = S.turns.get(r.id);
    const state = busy(r.id) ? (turn?.state === "settling" ? "Gathering memory" : "Thinking") : tasks.some(handRunning) ? "Waiting on the computer" : "Ready";
    const st = S.status;
    const topics = (t?.topics ?? []).map((x) => ({ x, t: (S.topics ?? []).find((y) => y.id === x.id) })).filter((o) => o.t);
    const sig = [r.id, p.id, p.name, p.emoji, p.hands, msgs.length, looks, webLooks, state, busy(r.id), tasks.map((k) => [k.id, handOff(k).state, k.text]), topics.map((o) => [o.x.id, o.x.p, o.t.name]), st?.hands, st?.T, st?.unbuilt];
    once(panel, sig, () => [
      h("div.panel-head", null, h("span", { text: "What it did here" }), h("span.grow"), iconButton("x", "Close", () => setPanel(false))),
      h("div.panel-persona", null, avatar(p, "xxl"), h("div.panel-name", { text: p.name }), h(`div.panel-state${busy(r.id) ? ".live" : ""}`, null, busy(r.id) ? h("span.pulse-dot") : null, state)),
      h(
        "section.panel-sec",
        null,
        h("h3", { text: "Topics" }),
        topics.length ? h("div.chips", null, topics.map((o) => h("a.chip", { href: `#/topic/${encodeURIComponent(o.x.id)}`, title: o.x.p ? `${Math.round(o.x.p * 100)}% sure` : "" }, icon("hash"), o.t.name))) : h("p.quiet", { text: msgs.length ? "Sorted after each answer." : "Sorted once you've talked." }),
      ),
      h(
        "section.panel-sec",
        null,
        h("h3", { text: "Done here" }),
        tasks.length || looks || webLooks
          ? h(
              "div.did",
              null,
              tasks.map((k) =>
                h(
                  `button.did-row${handRunning(k) ? ".live" : ""}`,
                  { type: "button", onclick: () => (screen?.reveal?.(`k:${k.id}`), narrowClose()) },
                  handRunning(k) ? icon("loader", "spin") : /^(error|failed|lost|stopped)$/.test(handOff(k).state) ? icon("alert") : icon("check"),
                  h("span", { text: firstLine(k.text, 70) || "A hand-off" }),
                ),
              ),
              looks ? h("div.did-row.quiet", null, icon("layers"), h("span", { text: `Looked through memory ${plural(looks, "time")}` })) : null,
              webLooks ? h("div.did-row.quiet", null, icon("globe"), h("span", { text: `Looked on the web ${plural(webLooks, "time")}` })) : null,
            )
          : h("p.quiet", { text: "Nothing yet." }),
      ),
      h(
        "section.panel-sec",
        null,
        h("h3", { text: "Computer" }),
        h("div.kv", null, icon("monitor"), h("span", { text: st ? (st.hands ? (tasks.some(handRunning) ? "Working on a hand-off" : "Connected, idle") : "No computer yet") : "…" })),
        // in the shell, its screen opens beside the chat
        EMBED && st?.hands ? h("button.kv.link", { type: "button", onclick: () => document.dispatchEvent(new CustomEvent("mind:screen")) }, icon("open"), h("span", { text: "See its screen" }), icon("chevron")) : null,
        p.hands ? null : h("p.quiet", { text: `${p.name} answers itself. Personas with hands can use the computer.` }),
      ),
      h(
        "section.panel-sec",
        null,
        h("h3", { text: "Memory" }),
        h("a.kv.link", { href: "#/memory" }, icon("layers"), h("span", { text: st ? `${(st.T ?? 0).toLocaleString()} messages${st.unbuilt ? `, summarizing ${st.unbuilt}` : ""}` : "…" }), icon("chevron")),
      ),
    ]);
  }

  function setPanel(on) {
    panelOpen = on;
    if (matchMedia(WIDE).matches) localStorage.setItem(PANEL_KEY, on ? "1" : "0");
    changed();
  }
  const narrowClose = () => !matchMedia(WIDE).matches && setPanel(false);

  // ---- the drawer (narrow screens) ----
  function drawer(on) {
    app.classList.toggle("drawer-open", on);
  }
  scrim.addEventListener("click", () => {
    drawer(false);
    if (!matchMedia(WIDE).matches) setPanel(false);
  });
  document.addEventListener("mind:drawer", () => drawer(!app.classList.contains("drawer-open")));
  document.addEventListener("mind:panel", () => setPanel(!panelOpen));
  document.addEventListener("mind:persona", (e) => personaSheet(e.detail ? persona(e.detail) : null));
  document.addEventListener("mind:reveal", (e) => screen?.reveal?.(e.detail));

  // ---- the toast ----
  function renderToast() {
    const text = S.closed ?? S.error;
    toast.hidden = !text;
    if (!text) return;
    toast.replaceChildren(icon("alert"), h("span", { text }));
    if (!S.closed) toast.append(iconButton("x", "Dismiss", () => ((S.error = null), changed())));
  }

  // ---- screens ----
  function show() {
    const r = route();
    const same = current?.key === r.key;
    current = r;
    if (!same) {
      screen?.destroy?.();
      screen = r.name === "thread" ? threadScreen(r.id) : r.name === "search" ? searchScreen(r.q) : r.name === "topics" ? topicsScreen() : r.name === "topic" ? topicScreen(r.id) : r.name === "memory" ? memoryScreen() : newScreen();
      main.replaceChildren(screen.el);
      drawer(false);
      app.dataset.screen = r.name;
      if (!matchMedia("(pointer: coarse)").matches || r.name === "search") requestAnimationFrame(() => screen.focus?.());
    } else screen?.update?.(r);
    render();
  }

  function render() {
    if (EMBED) tellShell();
    else renderRail();
    screen?.render();
    renderPanel();
    renderToast();
  }

  addEventListener("hashchange", show);
  addEventListener("keydown", (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "k") {
      e.preventDefault();
      if (current?.name === "search") screen.focus();
      else go("/search");
    } else if (mod && e.shiftKey && e.key.toLowerCase() === "o") {
      e.preventDefault();
      newChat();
    } else if (e.key === "Escape" && app.classList.contains("drawer-open")) drawer(false);
  });
  // the shell that frames the page may say light or dark, and (embedded)
  // where to go
  addEventListener("message", (event) => {
    if (window.parent === window || event.source !== window.parent) return;
    const d = event.data;
    if (d && d.fragment === "theme" && (d.mode === "light" || d.mode === "dark")) document.documentElement.dataset.theme = d.mode;
    if (!EMBED || d?.fragment !== "mind" || event.origin !== platformOrigin()) return;
    if (typeof d.go === "string" && ROUTE.test(d.go)) go(d.go.slice(1));
    if (d.new === null || typeof d.new === "string") newChat(d.new === null ? null : (persona(d.new)?.id ?? null));
    if (d.sheet === "settings") settingsSheet();
    if (d.sheet === "persona") personaSheet(typeof d.id === "string" ? persona(d.id) : null);
    if (typeof d.panel === "boolean") setPanel(d.panel);
    else if (d.panel === "toggle") setPanel(!panelOpen);
  });
  if (EMBED) {
    // the computer's screen is the shell's to show
    document.addEventListener("mind:screen", () => toShell({ screen: true }));
    // a link in what was said, or an app a turn used (thread.js), to one of
    // the person's fragments (its host's first label is `<label>--<user>`)
    // opens in the shell, beside the chat
    document.addEventListener("click", (e) => {
      const a = e.target.closest?.(".md a[href], a.app-link[href]");
      if (!a || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      let url;
      try {
        url = new URL(a.href);
      } catch {
        return;
      }
      if (!/^https?:$/.test(url.protocol) || !url.hostname.split(".")[0].includes("--")) return;
      e.preventDefault();
      toShell({ app: url.href });
    });
  }
  matchMedia(WIDE).addEventListener("change", (e) => {
    panelOpen = e.matches && localStorage.getItem(PANEL_KEY) !== "0";
    changed();
  });
  // times in lists ("5m") move on
  setInterval(changed, 60_000);

  onChange(render);
  start(fragment);
  show();
}

const compact = (n) => (n >= 10_000 ? `${Math.round(n / 1000)}k` : n.toLocaleString());

/// `node`'s children made again only when what they show (`sig`) changed,
/// so a live update does not take focus or hover from under the person.
function once(node, sig, build) {
  const s = JSON.stringify(sig);
  if (node.__sig === s) return;
  node.__sig = s;
  node.replaceChildren(...build().filter(Boolean));
}
