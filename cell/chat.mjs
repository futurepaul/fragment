// The chat's page, served by the platform on every fragment as ./__chat.js
// (with ./__chat.css): it renders the records agents write, so it ships
// with the platform instead of being frozen into each chat (docs/platform.md).
// A chat's own page is a shell:
//
//   <link rel="stylesheet" href="./__chat.css">
//   <script type="module">
//     import { mount } from "./__chat.js";
//     mount(document.body);
//   </script>
//
// A chat is two channels (docs/api.md, the chat template). `chat` holds its
// messages, `{text}`, posted by the people in it (`fragment.post`), and its
// agents' answers, `{text, turn}`; a record of another kind (`{kind: "stop",
// turn}`) is never a message. `work` holds an agent's progress while a turn
// runs: `turn.start` (who asked), one `turn.step` per tool call (the tool,
// its short arguments, whether it worked, a short excerpt of its result,
// and the model's text before it), `turn.end`. The page groups a turn's
// steps above its answer, shows who is working while one runs, and gives
// the person who started it a Stop button. Nothing streams: each record is
// a whole step.
//
// Ported from finite-mono's hosted chat (the desktop's look): Funnel Sans and
// JetBrains Mono, a 720 px column, dark unless the system asks for light.

import * as fragment from "./__fragment.js";

const ICON = {
  wrench: '<path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.4-.6-.6-2.4z"/>',
  loader: '<path d="M12 3a9 9 0 1 0 9 9"/>',
  chevron: '<path d="m9 6 6 6-6 6"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>',
  check: '<path d="M5 12l5 5L20 7"/>',
  chat: '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="1.5"/>',
  send: '<path d="M12 19V5M5 12l7-7 7 7"/>',
};
const svg = (name, cls = "") => `<svg class="${cls}" viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;

// The model's text and a tool's result are excerpts already (the agent cuts
// them); a message is at most what a record holds.
const TEXT_MAX = 16000;
// After a message of one's own, the working line shows at once, until the
// agent's turn starts (or this long passes: an agent may not be listening).
const PENDING_MS = 15000;
const ROLES = ["public", "viewer", "editor", "owner"];
const atLeast = (role, floor) => ROLES.indexOf(role) >= ROLES.indexOf(floor);

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const time = (at) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/// Mounts the chat in `root` (the page's body, say). Options:
/// `suggestions`, the empty chat's chips; `placeholder`, the composer's.
export function mount(root, options = {}) {
  const suggestions = options.suggestions ?? ["What can you do?", "Make me a todo list app", "What apps do I have?"];
  const placeholder = options.placeholder ?? "Ask your agent anything";

  root.classList.add("fragment-chat");
  root.innerHTML = `
    <div class="scroll" id="scroll"><div class="column" id="messages"></div></div>
    <div class="composer-wrap">
      <button class="latest" id="latest" type="button" hidden>Latest</button>
      <div class="banner" id="banner" hidden><span id="banner-text"></span><button type="button" id="banner-dismiss">Dismiss</button></div>
      <form class="composer" id="say">
        <textarea id="text" rows="1" aria-label="Message" maxlength="${TEXT_MAX}"></textarea>
        <div class="composer-actions">
          <span class="note" id="note"></span>
          <button class="stop" id="stop" type="button" title="Stop your agent's turn" hidden>${svg("stop")}Stop</button>
          <button class="send" id="send" type="submit" aria-label="Send" disabled>${svg("send")}</button>
        </div>
      </form>
    </div>`;
  const $ = (id) => root.querySelector(`#${id}`);
  const input = $("text");
  input.placeholder = placeholder;

  const state = {
    me: null, // { principal, role } once the socket said hello
    messages: [], // the chat's messages, in order: { seq, at, principal, text, turn }
    turns: new Map(), // turn -> { turn, agent, asker, at, steps: Map(n -> step), end, answered }
    toggled: new Map(), // turn -> open, once someone opened or closed its group
    pending: 0, // when this page sent a message the agent has not started on
    stopping: new Set(), // turns this page asked to stop
    work: false, // whether this page reads the work channel
  };
  const nodes = new Map(); // a message's seq -> its node (records never change)
  const people = new Map(); // principal -> a promise of its profile
  const known = new Map(); // principal -> its profile, once fetched

  // ---- names: a person by username, an agent as its owner's ----
  function profile(principal) {
    if (!principal.startsWith("id:")) {
      known.set(principal, { agent: false, label: "a visitor" });
      return Promise.resolve(known.get(principal));
    }
    if (!people.has(principal)) {
      people.set(
        principal,
        fetch(`./__people?id=${encodeURIComponent(principal)}`, { credentials: "same-origin" })
          .then((r) => r.json())
          .then((v) => v.profiles?.[principal] ?? {})
          .catch(() => ({}))
          .then((p) => ({ agent: p.kind === "agent", label: p.kind === "agent" ? `${p.username ?? "someone"}'s agent` : (p.username ?? `id:…${principal.slice(-6)}`) }))
          .then((p) => {
            known.set(principal, p);
            // a name that arrives later: the nodes that show it are made again
            for (const m of state.messages) if (m.principal === principal) nodes.delete(m.seq);
            render();
            return p;
          }),
      );
    }
    return people.get(principal);
  }
  function named(principal) {
    if (!known.has(principal)) profile(principal);
    return known.get(principal);
  }
  const labelOf = (principal, fallback) => named(principal)?.label ?? fallback;

  // ---- records ----
  function turnOf(id) {
    if (!state.turns.has(id)) state.turns.set(id, { turn: id, agent: null, asker: null, at: null, steps: new Map(), end: null, answered: false });
    return state.turns.get(id);
  }

  function onChat(record) {
    const body = record.body;
    const object = body !== null && typeof body === "object" && !Array.isArray(body);
    // a stop, or any kind but a message, is not shown
    if (object && body.kind !== undefined && body.kind !== "message") return;
    const text = object && typeof body.text === "string" ? body.text : JSON.stringify(body);
    const turn = object && typeof body.turn === "string" ? body.turn : null;
    state.messages.push({ seq: record.seq, at: record.at, principal: record.principal, text, turn });
    if (turn) {
      const t = turnOf(turn);
      t.answered = true;
      t.agent ??= record.principal;
      state.pending = 0;
    }
    schedule();
  }

  function onWork(record) {
    const b = record.body;
    if (!b || typeof b.turn !== "string") return;
    const t = turnOf(b.turn);
    t.agent ??= record.principal;
    if (b.kind === "turn.start") {
      t.asker = b.asker;
      t.at = record.at;
      if (b.asker === state.me?.principal) state.pending = 0;
    } else if (b.kind === "turn.step" && Number.isInteger(b.step)) {
      t.at ??= record.at;
      t.steps.set(b.step, { ...b, at: record.at });
    } else if (b.kind === "turn.end") {
      t.at ??= record.at;
      t.end = { outcome: b.outcome, error: b.error, at: record.at };
    }
    schedule();
  }

  const running = (t) => t.at !== null && !t.end && !t.answered;
  const runningTurns = () => [...state.turns.values()].filter(running);

  // ---- rendering: messages in order, each turn's steps above its answer ----
  let scheduled = false;
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      render();
    });
  }

  function items() {
    // a turn not answered (running, stopped, failed) sits where it started
    const loose = [...state.turns.values()].filter((t) => !t.answered && t.at !== null).sort((a, b) => a.at - b.at);
    const out = [];
    for (const m of state.messages) {
      while (loose.length && loose[0].at <= m.at) out.push({ type: "turn", t: loose.shift() });
      if (m.turn && state.turns.has(m.turn)) out.push({ type: "turn", t: state.turns.get(m.turn) });
      out.push({ type: "message", m });
    }
    for (const t of loose) out.push({ type: "turn", t });
    return out;
  }

  function render() {
    const scroll = $("scroll");
    const col = $("messages");
    const list = items();
    const live = runningTurns();
    if (state.pending && Date.now() - state.pending > PENDING_MS) state.pending = 0;
    const out = [];
    for (const item of list) {
      if (item.type === "message") out.push(message(item.m));
      else out.push(...turnNodes(item.t));
    }
    for (const t of live) out.push(workingLine(t));
    if (!live.length && state.pending) out.push(workingLine(null));
    if (!out.length) out.push(emptyState());
    col.replaceChildren(...out);
    // the Stop button: for whoever started a turn that runs
    const mine = live.find((t) => t.asker && t.asker === state.me?.principal);
    $("stop").hidden = !mine;
    $("stop").disabled = !!mine && state.stopping.has(mine.turn);
    $("stop").dataset.turn = mine?.turn ?? "";
    input.placeholder = mine ? "Add to what your agent is doing" : placeholder;
    $("send").disabled = !input.value.trim() || !canPost();
    if (stuck) scroll.scrollTop = scroll.scrollHeight;
    showLatest();
  }

  function message(m) {
    const cached = nodes.get(m.seq);
    if (cached) return cached;
    const who = named(m.principal);
    const node = m.turn || who?.agent ? agentMessage(m, who) : userMessage(m, who);
    nodes.set(m.seq, node);
    return node;
  }

  function userMessage(m, who) {
    const mine = m.principal === state.me?.principal;
    const wrap = el("div", `msg user ${mine ? "mine" : "other"}`);
    if (!mine) wrap.append(el("div", "who", who?.label ?? "…"));
    wrap.append(el("div", "bubble", m.text), el("div", "time", time(m.at)));
    return wrap;
  }

  function agentMessage(m, who) {
    const wrap = el("div", "msg agent");
    const body = el("div", "md");
    body.append(renderMarkdown(m.text));
    const actions = el("div", "actions");
    const copy = el("button", "icon-button copy");
    copy.type = "button";
    copy.title = "Copy";
    copy.innerHTML = svg("copy");
    copy.onclick = () => copyText(m.text, copy);
    actions.append(copy, el("span", "who", who?.label ?? "the agent"), el("span", "time", time(m.at)));
    wrap.append(body, actions);
    return wrap;
  }

  async function copyText(text, button) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const t = el("textarea");
      t.value = text;
      document.body.append(t);
      t.select();
      document.execCommand("copy");
      t.remove();
    }
    button.innerHTML = svg("check");
    button.title = "Copied";
    setTimeout(() => {
      button.innerHTML = svg("copy");
      button.title = "Copy";
    }, 1200);
  }

  // A turn's group of steps (while it runs: open, "Working"; once done:
  // "Worked through N steps", folded unless someone opened it), and a note
  // for a turn that ended with no answer.
  function turnNodes(t) {
    const out = [];
    const steps = [...t.steps.values()].sort((a, b) => a.step - b.step);
    const live = running(t);
    if (steps.length) {
      const d = el("details", `tools${live ? " live" : ""}`);
      d.dataset.turn = t.turn;
      d.open = state.toggled.has(t.turn) ? state.toggled.get(t.turn) : live;
      d.ontoggle = () => {
        if (d.open !== (state.toggled.has(t.turn) ? state.toggled.get(t.turn) : live)) state.toggled.set(t.turn, d.open);
      };
      const summary = el("summary");
      summary.innerHTML = svg(live ? "loader" : "wrench", live ? "spin" : "");
      const n = steps.length;
      summary.append(el("span", "grow", live ? `Working · ${n} step${n === 1 ? "" : "s"}` : `Worked through ${n} step${n === 1 ? "" : "s"}`));
      summary.insertAdjacentHTML("beforeend", svg("chevron", "chev"));
      d.append(summary);
      for (const s of steps) {
        if (s.text) d.append(el("div", "step-text", s.text));
        const step = el("div", `step${s.ok === false ? " error" : ""}`);
        const pre = el("pre");
        pre.append(el("span", "step-name", `${s.tool}${s.args ? ` ${s.args}` : ""}\n`), s.excerpt || (s.ok === false ? "failed" : "done"));
        step.append(pre);
        d.append(step);
      }
      out.push(d);
    }
    if (t.end && !t.answered && t.end.outcome !== "idle") {
      const said = t.end.outcome === "stopped" ? "Stopped." : t.end.outcome === "error" ? `The agent hit an error${t.end.error ? `: ${t.end.error}` : "."}` : `The agent stopped (${t.end.outcome}).`;
      out.push(el("div", `msg notice ${t.end.outcome}`, said));
    }
    return out;
  }

  function workingLine(t) {
    const line = el("div", "msg working");
    const hint = el("span", "hint");
    const dots = el("span", "dots");
    dots.append(el("i"), el("i"), el("i"));
    const agent = t?.agent ? named(t.agent)?.label : null;
    const forWhom = t?.asker && t.asker !== state.me?.principal ? ` for ${labelOf(t.asker, "someone")}` : "";
    const who = !t || t.asker === state.me?.principal ? "Your agent" : agent ? capital(agent) : "The agent";
    hint.append(dots, `${who} is working${forWhom}`);
    line.append(hint);
    return line;
  }
  const capital = (s) => s.charAt(0).toUpperCase() + s.slice(1);

  function emptyState() {
    const wrap = el("div", "empty");
    const logo = el("div", "logo");
    logo.innerHTML = svg("chat");
    wrap.append(logo, el("h2", null, "What should we work on?"));
    wrap.append(el("p", null, canPost() ? "Ask your agent, or anyone else here." : "Nothing has been said here yet."));
    if (canPost() && suggestions.length) {
      const chips = el("div", "suggestions");
      for (const s of suggestions) {
        const b = el("button", null, s);
        b.type = "button";
        b.onclick = () => send(s);
        chips.append(b);
      }
      wrap.append(chips);
    }
    return wrap;
  }

  // ---- scrolling: follow the end while the reader is there (a picture
  // that loads later too), and a way back to it ----
  let stuck = true;
  const atEnd = () => {
    const s = $("scroll");
    return s.scrollHeight - s.scrollTop - s.clientHeight < 160;
  };
  function showLatest() {
    $("latest").hidden = atEnd();
  }
  $("scroll").addEventListener("scroll", () => {
    stuck = atEnd();
    showLatest();
  });
  $("messages").addEventListener(
    "load",
    (e) => {
      if (e.target.tagName === "IMG" && stuck) $("scroll").scrollTop = $("scroll").scrollHeight;
      showLatest();
    },
    true,
  );
  $("latest").onclick = () => {
    stuck = true;
    $("scroll").scrollTop = $("scroll").scrollHeight;
  };

  // A screenshot opens in the desktop's viewer when this page is its frame
  // (the desktop listens for `{fragment: "open"}` from its own fragments).
  $("messages").addEventListener("click", (e) => {
    const shot = e.target.closest("img.shot");
    if (!shot || parent === window) return;
    e.preventDefault();
    parent.postMessage({ fragment: "open", url: new URL(shot.getAttribute("src"), location.href).href, title: shot.alt || "screenshot" }, "*");
  });

  // ---- the composer: grows with its text; Enter sends, Shift+Enter adds a line ----
  const canPost = () => !!state.me && atLeast(state.me.role, "viewer");
  function grow() {
    input.style.height = "auto";
    input.style.height = `${Math.min(Math.max(input.scrollHeight, 42), 220)}px`;
    $("send").disabled = !input.value.trim() || !canPost();
  }
  input.addEventListener("input", grow);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      $("say").requestSubmit();
    }
  });
  $("say").addEventListener("submit", (e) => {
    e.preventDefault();
    send(input.value);
  });

  function problem(text) {
    $("banner-text").textContent = text;
    $("banner").hidden = false;
  }
  $("banner-dismiss").onclick = () => {
    $("banner").hidden = true;
  };

  // A message; while one's own turn runs, it steers that turn (the agent
  // reads it between steps).
  async function send(text) {
    text = text.trim();
    if (!text || !canPost()) return;
    const draft = input.value;
    input.value = "";
    grow();
    try {
      await fragment.post("chat", { text });
      $("banner").hidden = true;
      // someone signed in, in a chat an agent works in: it starts soon
      if (!state.me.principal.startsWith("anon:") && [...state.turns.values()].some((t) => t.agent) && !runningTurns().length) state.pending = Date.now();
      schedule();
      if (state.pending) setTimeout(schedule, PENDING_MS + 50);
    } catch (err) {
      input.value = draft;
      grow();
      problem(`Your message was not sent: ${err.message}`);
    }
    input.focus();
  }

  $("stop").onclick = async () => {
    const turn = $("stop").dataset.turn;
    if (!turn) return;
    state.stopping.add(turn);
    render();
    try {
      await fragment.post("chat", { kind: "stop", turn }, { id: `stop-${turn}` });
    } catch (err) {
      state.stopping.delete(turn);
      render();
      problem(`Your agent could not be stopped: ${err.message}`);
    }
  };

  // ---- who this page is, then the channels it may read ----
  fragment.subscribe("chat", onChat, { last: 200 });
  fragment.me().then((hello) => {
    state.me = hello;
    for (const m of state.messages) nodes.delete(m.seq);
    if (atLeast(hello.role, "viewer")) {
      state.work = true;
      fragment.subscribe("work", onWork, { last: 1000 });
    }
    const anon = hello.principal.startsWith("anon:");
    const note = $("note");
    note.replaceChildren();
    if (!canPost()) {
      note.textContent = "You can read this chat.";
      input.disabled = true;
      input.placeholder = "Only people in this chat can write here";
    } else if (anon) {
      const a = el("a", null, "Sign in");
      a.href = "./__signin?return=/";
      note.append(a, " so the agent answers you.");
    }
    $("say").dataset.ready = "1";
    render();
    grow();
  });
  fragment.closed(({ code }) => {
    problem(code === 4004 ? "This chat was deleted." : "Your access to this chat changed: reload the page.");
    input.disabled = true;
  });
  render();
}

// ---- markdown → DOM, with textContent only (no innerHTML), so an agent's
// text can never add markup or script to the page. Paragraphs, headings,
// lists, quotes, fenced code, inline code, bold, italic, links, pipe
// tables, and images from this fragment's own files (`__file?path=`). ----

export function renderMarkdown(text) {
  const root = document.createDocumentFragment();
  const lines = String(text || "").replace(/\r\n/g, "\n").split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (/^```/.test(line)) {
      const code = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i])) code.push(lines[i++]);
      i++;
      const pre = el("pre");
      pre.append(el("code", null, code.join("\n")));
      root.append(pre);
      continue;
    }
    if (!line.trim()) {
      i++;
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      root.append(inline(el(`h${Math.min(heading[1].length + 2, 6)}`), heading[2]));
      i++;
      continue;
    }
    if (/^\s*>/.test(line)) {
      const quote = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) quote.push(lines[i++].replace(/^\s*>\s?/, ""));
      const bq = el("blockquote");
      bq.append(renderMarkdown(quote.join("\n")));
      root.append(bq);
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(line) && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1] || "")) {
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(lines[i++]);
      root.append(table(rows));
      continue;
    }
    const listMatch = line.match(/^(\s*)([-*+]|\d+[.)])\s+/);
    if (listMatch) {
      const ordered = /\d/.test(listMatch[2]);
      const list = el(ordered ? "ol" : "ul");
      while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        const item = lines[i++].replace(/^\s*([-*+]|\d+[.)])\s+/, "");
        const li = el("li");
        const task = item.match(/^\[([ xX])\]\s+(.*)$/);
        if (task) {
          const box = el("input");
          Object.assign(box, { type: "checkbox", checked: task[1] !== " ", disabled: true });
          li.append(box, " ");
          inline(li, task[2]);
        } else inline(li, item);
        list.append(li);
      }
      root.append(list);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !/^(```|#{1,6}\s|\s*>|\s*([-*+]|\d+[.)])\s+)/.test(lines[i])) para.push(lines[i++]);
    root.append(inline(el("p"), para.join("\n")));
  }
  return root;
}

function table(rows) {
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const wrap = el("div", "md-table");
  const t = el("table");
  const head = el("tr");
  for (const c of cells(rows[0])) head.append(inline(el("th"), c));
  t.append(head);
  for (const row of rows.slice(2)) {
    const tr = el("tr");
    for (const c of cells(row)) tr.append(inline(el("td"), c));
    t.append(tr);
  }
  wrap.append(t);
  return wrap;
}

// Inline spans: `code`, **bold**, *italic*/_italic_, ![alt](__file?path=…)
// (an image from this fragment's own files only), [text](url), bare URLs.
const INLINE = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(!\[([^\]]*)\]\((__file\?path=[^)\s]+)\))|(\[[^\]]+\]\((https?:\/\/[^)\s]+)\))|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

function inline(parent, text) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) appendText(parent, text.slice(last, m.index));
    const [whole] = m;
    if (m[1]) parent.append(el("code", null, whole.slice(1, -1)));
    else if (m[2]) parent.append(inline(el("strong"), whole.slice(2, -2)));
    else if (m[3]) parent.append(inline(el("em"), whole.slice(1, -1)));
    else if (m[4]) parent.append(image(m[6], m[5]));
    else if (m[7]) parent.append(link(m[8], whole.slice(1, whole.indexOf("]("))));
    else parent.append(link(whole, whole));
    last = m.index + whole.length;
  }
  if (last < text.length) appendText(parent, text.slice(last));
  return parent;
}

function appendText(parent, text) {
  text.split("\n").forEach((part, n) => {
    if (n) parent.append(el("br"));
    parent.append(part);
  });
}

function image(src, alt) {
  const a = link(src, "");
  a.className = "shot-link";
  const img = el("img", "shot");
  img.src = src;
  img.alt = alt;
  img.loading = "lazy";
  a.append(img);
  return a;
}

function link(href, label) {
  const a = el("a", null, label);
  a.href = href;
  a.target = "_blank";
  a.rel = "noopener noreferrer";
  return a;
}
