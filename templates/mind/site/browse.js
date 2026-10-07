// The screens for finding things again, none of them a list of subject
// lines: Search (instant, grouped by thread, each hit opening in place),
// Everything (every chat as what it came to, by time), Topics and a topic
// (the chats Clef sorted there, as cards that open in place), and Memory
// (the view itself, as the agent sees it, each line opening down to its
// messages).

import { go, hitRow, memRow, threadCard, threadTitle } from "./pieces.js";
import { EVERYTHING_PAGE, F, S, changed, problem, putThread } from "./store.js";
import { topBar } from "./thread.js";
import { ago, h, icon, kb, period, plural, reconcile, when } from "./ui.js";

/// The most hits one search answers (fragment.json: `search.limit`).
const SEARCH_LIMIT = 50;
const SEARCH_WAIT_MS = 110;
/// The view's budget (the spec's VIEW), for the Memory screen's gauge.
const VIEW_BYTES = 128_000;

const title = (text, sub) => h("div.top-title", null, h("div.top-text", null, h("div.top-name", { text }), sub ? h("div.top-sub", { text: sub }) : null));

// ---- Everything: the chats by time, as cards ----

function everything(into, row) {
  const list = h("div.timeline");
  const more = h("button.ghost.more", { type: "button", hidden: true }, "Further back");
  into.replaceChildren(h("div.screen-intro", null, h("h1", { text: "Everything" }), h("p", { text: "Every chat, as it came to be remembered. Open one to read it here." }), row), list, more);
  let oldest = null;
  let lastPeriod = null;
  async function page() {
    more.disabled = true;
    try {
      const input = { limit: EVERYTHING_PAGE };
      if (oldest !== null) input.before = oldest;
      const r = await F.call("threads", input);
      const threads = r.threads ?? [];
      for (const t of threads) {
        const known = putThread(t);
        const at = known.last ?? known.started ?? 0;
        const p = period(at);
        if (p !== lastPeriod) {
          list.append(h("h2.period", { text: p }));
          lastPeriod = p;
        }
        list.append(threadCard(known));
        oldest = at;
      }
      more.hidden = threads.length < EVERYTHING_PAGE;
      if (!list.children.length) list.append(h("p.empty", { text: "Nothing yet. Every chat you have lands here." }));
    } catch (e) {
      problem(`Could not read your chats: ${e.message}`);
    }
    more.disabled = false;
  }
  more.addEventListener("click", page);
  page();
}

// ---- Search ----

export function searchScreen(q0 = "") {
  const input = h("input.search-input", { type: "search", placeholder: "Search everything you've ever said", value: q0, autocomplete: "off", spellcheck: "false", "aria-label": "Search" });
  const clear = h("button.icon-btn.search-clear", { type: "button", "aria-label": "Clear", hidden: !q0 }, icon("x"));
  const bar = h("div.search-bar", null, icon("search"), input, clear);
  const status = h("div.search-status", { "aria-live": "polite" });
  const results = h("div.results");
  // Everything's ways in by topic
  const topicRow = h("div.chips.big.topic-row");
  const col = h("div.column.wide", null, bar, status, results);
  const el = h("section.screen.search", null, topBar(title("Search")), h("div.scroll", null, col));
  let seq = 0;
  let timer = 0;
  let shown = null;

  async function run() {
    const q = input.value.trim();
    clear.hidden = !input.value;
    history.replaceState(null, "", q ? `#/search?q=${encodeURIComponent(q)}` : "#/search");
    const n = ++seq;
    if (!q) {
      status.textContent = "";
      if (shown !== "") everything(results, topicRow);
      shown = "";
      return;
    }
    const slow = setTimeout(() => n === seq && (status.textContent = "Searching…"), 200);
    let r;
    try {
      r = await F.call("search", { q, limit: SEARCH_LIMIT });
    } catch (e) {
      clearTimeout(slow);
      if (n === seq) status.textContent = `Search failed: ${e.message}`;
      return;
    }
    clearTimeout(slow);
    if (n !== seq) return;
    shown = q;
    const hits = (r.results ?? []).filter((x) => Number.isInteger(x.i));
    const groups = new Map();
    for (const hit of hits) {
      const k = hit.thread ?? "";
      if (!groups.has(k)) groups.set(k, []);
      groups.get(k).push(hit);
    }
    status.textContent = hits.length ? `${plural(hits.length, "match", "matches")} in ${plural(groups.size, "chat")}` : "";
    if (!hits.length) {
      results.replaceChildren(h("div.empty-search", null, h("p", { text: `Nothing yet for “${q}”.` }), h("p.quiet", { text: "Search looks through every word ever said here, yours and Mind's." })));
      return;
    }
    results.replaceChildren(
      ...[...groups].map(([thread, list]) => {
        const newest = Math.max(...list.map((x) => x.at ?? 0));
        const head = thread
          ? h("a.group-head", { href: `#/t/${thread}` }, h("span.group-title", null, threadTitle(thread)), h("time", { text: ago(newest), title: when(newest) }), icon("open"))
          : h("div.group-head", null, h("span.group-title", { text: "Notes" }), h("time", { text: ago(newest) }));
        return h("section.group", null, head, h("div.group-hits", null, list.map((hit) => hitRow(hit, q))));
      }),
    );
  }

  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(run, SEARCH_WAIT_MS);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && input.value) {
      e.stopPropagation();
      input.value = "";
      run();
    }
  });
  clear.addEventListener("click", () => {
    input.value = "";
    run();
    input.focus();
  });
  run();
  return {
    el,
    render() {
      const topics = [...(S.topics ?? [])].sort((a, b) => (b.count ?? 0) - (a.count ?? 0));
      const sig = JSON.stringify(topics.map((t) => [t.id, t.name, t.count]));
      if (topicRow.__sig === sig) return;
      topicRow.__sig = sig;
      topicRow.replaceChildren(...topics.map((t) => h("a.chip", { href: `#/topic/${encodeURIComponent(t.id)}` }, icon("hash"), t.name, h("span.chip-count", { text: String(t.count ?? 0) }))));
    },
    focus: () => input.focus(),
    // the hash named another search (a link, the back button)
    update(r) {
      if ((r.q ?? "") === input.value.trim()) return;
      input.value = r.q ?? "";
      run();
    },
  };
}

// ---- Topics ----

/// A topic's hue, the same for its name everywhere.
const hue = (name) => {
  let x = 0;
  for (const c of String(name)) x = (x * 31 + c.codePointAt(0)) % 360;
  return x;
};

export function topicsScreen() {
  const addBtn = h("button.pill", { type: "button" }, icon("plus"), "New topic");
  const suggestBtn = h("button.pill.ghost", { type: "button" }, icon("sparkles"), "Suggest some");
  const name = h("input.field", { placeholder: "Name, like “Garden” or “The house”", maxlength: 48, "aria-label": "Topic name" });
  const desc = h("input.field", { placeholder: "What belongs here (optional)", maxlength: 400, "aria-label": "What belongs here" });
  const form = h("form.add-topic", { hidden: true }, name, desc, h("div.row", null, h("span.grow"), h("button.ghost", { type: "button", onclick: () => (form.hidden = true) }, "Cancel"), h("button.primary", { type: "submit" }, "Add topic")));
  const sugg = h("div.suggestions");
  const grid = h("div.topic-grid");
  const intro = h("div.screen-intro", null, h("h1", { text: "Topics" }), h("p", { text: "Chats sort themselves into topics as you go. Name what matters to you; past chats are sorted too." }), h("div.row", null, addBtn, suggestBtn));
  const el = h("section.screen.topics", null, topBar(title("Topics")), h("div.scroll", null, h("div.column.wide", null, intro, form, sugg, grid)));

  addBtn.addEventListener("click", () => {
    form.hidden = false;
    name.focus();
  });
  suggestBtn.addEventListener("click", async () => {
    S.suggesting = true;
    S.suggestions = null;
    changed();
    try {
      await F.call("topic_suggest", {});
    } catch (e) {
      S.suggesting = false;
      problem(`Could not suggest topics: ${e.message}`);
    }
  });
  async function add(n, d) {
    try {
      await F.call("topic_add", d ? { name: n, description: d } : { name: n });
      if (S.suggestions) S.suggestions = S.suggestions.filter((x) => x !== n);
      changed();
    } catch (e) {
      problem(`Could not add “${n}”: ${e.message}`);
    }
  }
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    const n = name.value.trim();
    if (!n) return name.focus();
    add(n, desc.value.trim());
    name.value = "";
    desc.value = "";
    form.hidden = true;
  });

  function render() {
    const have = new Set((S.topics ?? []).map((t) => t.name.toLowerCase()));
    const offer = (S.suggestions ?? []).filter((n) => !have.has(n.toLowerCase()));
    suggestBtn.disabled = S.suggesting;
    if (S.suggesting) sugg.replaceChildren(h("div.suggest-wait", null, icon("sparkles", "pulse"), "Reading your memory for topics…"));
    else if (offer.length) sugg.replaceChildren(h("div.suggest-label", { text: "Suggested from your memory" }), h("div.chips.big", null, offer.map((n) => h("button.chip.offer", { type: "button", onclick: () => add(n) }, icon("plus"), n))));
    else sugg.replaceChildren();
    const topics = [...(S.topics ?? [])].sort((a, b) => (b.count ?? 0) - (a.count ?? 0) || a.name.localeCompare(b.name));
    if (S.topics && !topics.length) {
      grid.replaceChildren(h("p.empty", { text: "No topics yet. Add one, or let Mind suggest a few from what you've talked about." }));
      return;
    }
    reconcile(
      grid,
      topics.map((t) => ({
        key: t.id,
        sig: `${t.name}|${t.description}|${t.count}`,
        make: () =>
          h(
            "a.topic-card",
            { href: `#/topic/${encodeURIComponent(t.id)}`, style: `--h:${hue(t.name)}` },
            h("span.topic-mark", null, icon("hash")),
            h("span.topic-main", null, h("span.topic-name", { text: t.name }), t.description ? h("span.topic-desc", { text: t.description }) : null),
            h("span.topic-count", { text: String(t.count ?? 0), title: plural(t.count ?? 0, "chat") }),
          ),
      })),
    );
  }
  return { el, render, focus() {} };
}

// ---- a topic ----

export function topicScreen(id) {
  const head = h("div.screen-intro.topic-intro");
  const list = h("div.timeline");
  const more = h("button.ghost.more", { type: "button", hidden: true }, "More chats");
  const back = h("a.back", { href: "#/topics" }, icon("left"), "Topics");
  const el = h("section.screen.topic", null, topBar(back), h("div.scroll", null, h("div.column.wide", null, head, list, more)));
  let oldest = null;
  let sig = "";
  let loaded = 0;

  async function page(fresh) {
    if (fresh) {
      oldest = null;
      loaded = 0;
    }
    try {
      const input = { topic: id, limit: EVERYTHING_PAGE };
      if (oldest !== null) input.before = oldest;
      const r = await F.call("threads", input);
      const threads = r.threads ?? [];
      if (fresh) list.replaceChildren();
      for (const t of threads) {
        const known = putThread(t);
        list.append(threadCard(known));
        oldest = known.last ?? known.started ?? oldest;
      }
      loaded += threads.length;
      more.hidden = threads.length < EVERYTHING_PAGE;
      if (!loaded) list.replaceChildren(h("p.empty", { text: "No chats here yet. As soon as one is about this, it shows up here." }));
    } catch (e) {
      problem(`Could not read this topic: ${e.message}`);
    }
  }
  more.addEventListener("click", () => page(false));
  page(true);

  function render() {
    const t = (S.topics ?? []).find((x) => x.id === id);
    const next = JSON.stringify([t?.name, t?.description, t?.count]);
    if (next === sig) return;
    const countChanged = sig && JSON.parse(sig)[2] !== t?.count;
    sig = next;
    if (!t) {
      head.replaceChildren(h("h1", { text: S.topics ? "This topic is gone" : "" }));
      return;
    }
    document.title = `${t.name} · Mind`;
    const remove = h("button.ghost.small.danger", { type: "button" }, icon("trash"), "Remove topic");
    remove.addEventListener("click", async () => {
      if (remove.dataset.sure !== "1") {
        remove.dataset.sure = "1";
        remove.lastChild.textContent = "Remove it? Chats stay.";
        return;
      }
      try {
        await F.call("topic_remove", { id });
        go("/topics");
      } catch (e) {
        problem(`Could not remove it: ${e.message}`);
      }
    });
    head.style.setProperty("--h", hue(t.name));
    head.replaceChildren(
      h("div.topic-hero", null, h("span.topic-mark.big", null, icon("hash")), h("h1", { text: t.name })),
      t.description ? h("p", { text: t.description }) : null,
      h("div.row", null, h("span.quiet", { text: plural(t.count ?? 0, "chat") }), h("span.grow"), remove),
    );
    if (countChanged) page(true);
  }
  return { el, render, focus() {} };
}

// ---- Memory ----

/// What a stretch of the view is, said plainly.
function stretch(n) {
  if (n <= 1) return "One line per message";
  return `Each line holds ${n.toLocaleString()} messages`;
}

export function memoryScreen() {
  const stats = h("div.mem-stats");
  const strata = h("div.strata", { role: "list", "aria-label": "How your history is folded, oldest first" });
  const legend = h("div.strata-legend", { "aria-hidden": "true", hidden: true }, h("span", { text: "long ago, folded" }), h("span", { text: "now, line by line" }));
  const list = h("div.mem-list");
  const newest = h("button.pill.float-newest", { type: "button" }, icon("down"), "Newest");
  const intro = h(
    "div.screen-intro",
    null,
    h("h1", { text: "Memory" }),
    h("p", { text: "This is the whole of your history as every chat begins with it. Older stretches fold into fewer lines; the newest are a line each. Open any line to unfold it, all the way down to the words." }),
    stats,
    strata,
    legend,
  );
  const scroll = h("div.scroll", null, h("div.column.wide.memory-col", null, intro, list));
  const el = h("section.screen.memory", null, topBar(title("Memory", "as Mind sees it")), scroll, newest);
  const dates = new Map(); // first id of a stretch -> its date
  let painted = false;
  let parts = [];
  let bytes = 0;
  let T = 0;
  let cut = 0;

  newest.addEventListener("click", () => scroll.scrollTo({ top: scroll.scrollHeight, behavior: "smooth" }));
  scroll.addEventListener("scroll", () => newest.classList.toggle("away", scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 400));

  function dateOf(id) {
    if (dates.has(id)) return dates.get(id);
    dates.set(id, null);
    F.call("date", { id })
      .then((r) => {
        const at = Date.parse(r.text);
        dates.set(id, Number.isFinite(at) ? at : String(r.text ?? ""));
        draw();
      })
      .catch(() => {});
    return null;
  }

  function draw() {
    legend.hidden = parts.length < 2;
    newest.hidden = parts.length < 30;
    if (!parts.length) {
      list.replaceChildren(h("p.empty", { text: "Nothing yet. Every message lands here as a line of its own, and lines fold together as they age." }));
      strata.replaceChildren();
      stats.replaceChildren();
      return;
    }
    const items = [];
    const runs = [];
    let run = null;
    for (const p of parts) {
      if (!run || run.n !== p.n) {
        run = { n: p.n, first: p.id, lines: 0, key: `sec:${p.n}:${p.id}` };
        runs.push(run);
        const d = dateOf(p.id);
        const since = typeof d === "number" ? `from ${new Date(d).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" })}` : typeof d === "string" ? d : "";
        const label = stretch(p.n);
        const key = run.key;
        items.push({ key, sig: label + since, make: () => h("div.mem-sec", { dataset: { sec: key } }, h("span", { text: label }), since ? h("time", { text: since }) : null) });
      }
      run.lines++;
      const fresh = painted;
      items.push({ key: `${p.id}+${p.n}`, sig: `${p.built}|${p.text}`, make: () => memRow(p, { fresh }) });
    }
    // the newest lines past what one answer holds (`memory`'s `cut`)
    if (cut) items.push({ key: "cut", sig: String(cut), make: () => h("p.empty.mem-cut", { text: `And ${plural(cut, "newer line")}, more than one answer holds. Search finds them.` }) });
    reconcile(list, items, { quiet: true });
    // the strata: each level's stretch, as wide as (the root of) what it covers
    const deepest = Math.max(1, ...runs.map((r) => Math.log2(r.n)));
    reconcile(
      strata,
      runs.map((r) => ({
        key: r.key,
        sig: `${r.lines}|${deepest}`,
        make: () => {
          const covered = r.lines * r.n;
          const fine = 1 - Math.log2(r.n) / deepest;
          const b = h(
            "button.stratum",
            { type: "button", role: "listitem", style: `--g:${Math.sqrt(covered).toFixed(2)};--f:${fine.toFixed(2)}`, title: `${plural(covered, "message")} in ${plural(r.lines, "line")} (${r.n <= 1 ? "one per line" : `${r.n} per line`})` },
            h("span", { text: r.n <= 1 ? "1" : r.n >= 1024 ? `${r.n / 1024}k` : String(r.n) }),
          );
          b.addEventListener("click", () => list.querySelector(`[data-sec="${r.key}"]`)?.scrollIntoView({ behavior: "smooth", block: "start" }));
          return b;
        },
      })),
      { quiet: true },
    );
    const unbuilt = parts.filter((p) => p.built === false).length;
    const pct = Math.min(100, Math.round((bytes / VIEW_BYTES) * 100));
    // lines whose summary keeps failing (`status.failing`): said, not hidden
    const failing = Array.isArray(S.status?.failing) ? S.status.failing : [];
    stats.replaceChildren(
      h("div.stat", null, h("b", { text: T.toLocaleString() }), h("span", { text: "messages" })),
      h("div.stat", null, h("b", { text: (parts.length + cut).toLocaleString() }), h("span", { text: "lines" })),
      h("div.stat.gauge", null, h("b", { text: kb(bytes) }), h("span", { text: `of ${kb(VIEW_BYTES)}` }), h("span.bar", { "aria-hidden": "true" }, h("i", { style: `width:${pct}%` }))),
      unbuilt ? h("div.stat.live", null, icon("loader", "spin"), h("span", { text: `summarizing ${plural(unbuilt, "new message")}` })) : h("div.stat.settled", null, icon("check"), h("span", { text: "all summarized" })),
      failing.length ? h("div.stat.failing", { title: failing.map((f) => `${f.id}+${f.n}: ${f.error} (${f.tries} tries)`).join("\n") }, icon("alert"), h("span", { text: `${plural(failing.length, "line")} won't summarize yet` })) : null,
    );
    painted = true;
  }

  const stopLive = F.live(
    "memory",
    {},
    (r) => {
      parts = (r.parts ?? []).filter((p) => Number.isInteger(p.id) && Number.isInteger(p.n));
      bytes = Number(r.bytes) || 0;
      T = Number(r.T) || 0;
      cut = Number(r.cut) || 0;
      draw();
    },
    (e) => problem(`Could not read memory: ${e.message}`),
  );
  list.append(h("div.loading-thread", null, icon("loader", "spin")));
  return { el, render() {}, focus() {}, destroy: stopLive };
}
