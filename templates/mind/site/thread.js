// A thread (a screen of the one chat), the composer, and New chat.
//
// A thread's messages come from `thread` and then from `log` as they are
// written. A turn's tool calls and their results fold into one quiet
// "Looked through memory ×3" row, which opens to each step, and each
// result's lines open further (a search hit to its context, a memory line
// to its parts). A `computer` call is a hand-off card: goose's steps as
// they come, its words as they stream, then its report. The mind's reply
// streams in as `log`'s draft `turn:<thread>` until its `talk` arrives.

import { avatar, contextView, go, memRow, miniMessage, topicChip } from "./pieces.js";
import {
  PENDING_MS,
  S,
  busy,
  changed,
  currentPersona,
  draftOf,
  handDraftOf,
  loadEarlier,
  loadThread,
  persona,
  putThread,
  resend,
  running,
  say,
  stop,
} from "./store.js";
import { clock, dayLabel, firstLine, greeting, h, icon, iconButton, md, parseTool, plural, reconcile, reportOf, threadId, viewLines, when, copyButton } from "./ui.js";

/// A message's longest text (the log caps at 30 000 characters).
const TEXT_MAX = 30_000;
const MEMORY_TOOLS = new Set(["zoom", "search", "date"]);
/// Steps a hand-off card shows before "N earlier".
const CARD_STEPS = 4;

// unsent words, by thread ("" for New chat), kept across screens
const unsent = new Map();
// a persona picked for a thread's next message
const picked = new Map();
// what is open in threads: steps rows, hand-off cards, reports
const opened = new Set();

export const topBar = (...kids) =>
  h("header.top", null, iconButton("menu", "Open the menu", () => document.dispatchEvent(new CustomEvent("mind:drawer")), "only-narrow"), ...kids);

// ---- the composer ----

function personaMenu(anchor, current, onPick) {
  const menu = h("div.menu", { role: "menu" });
  for (const p of S.personas) {
    menu.append(
      h(
        "button.menu-item",
        { type: "button", role: "menuitemradio", "aria-checked": String(p.id === current), onclick: () => (onPick(p.id), close()) },
        avatar(p, "sm"),
        h("span.menu-name", { text: p.name }),
        p.id === S.defaultPersona ? h("span.badge", { text: "default" }) : null,
        p.hands ? h("span.badge.hands", { title: "Can use your computer" }, icon("monitor")) : null,
        p.id === current ? icon("check", "menu-check") : null,
      ),
    );
  }
  menu.append(h("div.menu-sep"), h("button.menu-item.quiet", { type: "button", onclick: () => (document.dispatchEvent(new CustomEvent("mind:persona", { detail: null })), close()) }, icon("plus"), h("span.menu-name", { text: "New persona" })));
  const away = (e) => {
    if (!menu.contains(e.target) && !anchor.contains(e.target)) close();
  };
  const esc = (e) => e.key === "Escape" && close();
  function close() {
    menu.remove();
    anchor.setAttribute("aria-expanded", "false");
    document.removeEventListener("pointerdown", away, true);
    document.removeEventListener("keydown", esc, true);
  }
  document.addEventListener("pointerdown", away, true);
  document.addEventListener("keydown", esc, true);
  anchor.setAttribute("aria-expanded", "true");
  anchor.parentElement.append(menu);
  menu.querySelector("[aria-checked=true]")?.focus();
}

/// The composer: a thread's (`thread` an id), or New chat's (null), which
/// makes the thread with its first message.
export function composer(thread, { onSent } = {}) {
  const key = thread ?? "";
  const ta = h("textarea.input", { rows: 1, "aria-label": "Message", maxlength: TEXT_MAX, value: unsent.get(key) ?? "" });
  const chipFace = h("span.chip-face");
  const chip = h("button.persona-chip", { type: "button", "aria-haspopup": "menu", "aria-expanded": "false", title: "Who answers" }, chipFace, icon("down", "chev"));
  const stopBtn = h("button.round.stop", { type: "button", "aria-label": "Stop", title: "Stop" }, icon("stop"));
  const sendBtn = h("button.round.send", { type: "submit", "aria-label": "Send", title: "Send (Enter)" }, icon("send"));
  const hint = h("span.composer-hint");
  const form = h("form.composer", null, ta, h("div.composer-row", null, h("div.chip-wrap", null, chip), hint, h("span.grow"), stopBtn, sendBtn));

  const personaId = () => picked.get(key) ?? (thread ? S.threads.get(thread)?.persona : null) ?? currentPersona().id;
  const coarse = matchMedia("(pointer: coarse)").matches;

  function grow() {
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 280)}px`;
    sendBtn.disabled = !ta.value.trim();
  }
  ta.addEventListener("input", () => {
    unsent.set(key, ta.value);
    grow();
  });
  ta.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && !coarse) {
      e.preventDefault();
      form.requestSubmit();
    }
  });
  chip.addEventListener("click", () => {
    if (form.querySelector(".menu")) return;
    personaMenu(chip, personaId(), (id) => {
      if (thread) picked.set(key, id);
      else S.chosen = id;
      changed();
      ta.focus();
    });
  });
  stopBtn.addEventListener("click", () => thread && stop(thread));
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    const text = ta.value.trim();
    if (!text) return;
    const pid = personaId();
    let id = thread;
    if (!id) {
      id = threadId();
      putThread({ id, title: firstLine(text, 80), persona: pid, started: Date.now(), last: Date.now(), count: 0, topics: [] });
      S.msgs.set(id, { byI: new Map(), more: false, loaded: true, loading: false });
      S.recent = [id, ...S.recent];
    }
    ta.value = "";
    unsent.delete(key);
    grow();
    say(id, text, pid);
    onSent?.(id);
  });

  function render() {
    const p = persona(personaId()) ?? currentPersona();
    chipFace.replaceChildren(avatar(p, "xs"), h("span", { text: p.name }));
    ta.placeholder = thread ? `Reply to ${p.name}…` : `Message ${p.name}…`;
    const on = !!thread && busy(thread);
    stopBtn.hidden = !on;
    stopBtn.disabled = S.stopping.has(thread);
    stopBtn.classList.toggle("stopping", S.stopping.has(thread));
    hint.textContent = p.hands && S.status && !S.status.hands ? "No computer yet" : "";
    grow();
  }
  render();
  return { el: form, render, focus: () => ta.focus(), set: (text) => ((ta.value = text), unsent.set(key, text), grow(), ta.focus()) };
}

// ---- the thread ----

const startOfDay = (t) => new Date(t).setHours(0, 0, 0, 0);

function stepTitle(call) {
  const a = call.args ?? {};
  switch (call.name) {
    case "zoom":
      return Number(a.n) <= 1 ? `Read message #${a.id}` : `Opened ${plural(Number(a.n) || 0, "message")} from #${a.id}`;
    case "search":
      return `Searched for “${a.q ?? a.query ?? ""}”`;
    case "date":
      return `Checked the date of #${a.id}`;
    default:
      return `${call.name}${a && Object.keys(a).length ? ` ${firstLine(JSON.stringify(a), 80)}` : ""}`;
  }
}

const STEP_ICON = { zoom: "zoom", search: "search", date: "calendar" };

/// A tool's result, readable: search hits and memory lines open in place.
function echoView(call, text) {
  if (call?.name === "search") {
    const lines = viewLines(text);
    if (lines.length) {
      return h(
        "div.echo-lines",
        null,
        lines.map((l) => {
          const m = l.text.match(/^(user|talk|tool|echo|note):\s*(.*)$/);
          const row = h("div.hit.inline");
          const head = h("button.hit-head", { type: "button" }, h("span.hit-who", { text: m ? { user: "You", talk: "Mind", note: "Note" }[m[1]] ?? m[1] : "" }), h("span.hit-text", { text: m ? m[2] : l.text }), h("span.hit-id", { text: `#${l.id}` }), icon("expand", "hit-toggle"));
          let ctx = null;
          head.addEventListener("click", () => {
            if (ctx) return ctx.remove(), (ctx = null), row.classList.remove("open");
            ctx = contextView(l.id);
            row.append(ctx);
            row.classList.add("open");
          });
          row.append(head);
          return row;
        }),
      );
    }
  }
  const lines = viewLines(text);
  if (lines.length) {
    // a zoom: lines of memory, or (n = 0, the spec's "+0|") the message whole
    if (lines.length === 1 && lines[0].n === 0) return md(lines[0].text.replace(/^(user|talk|tool|echo|note):\s*/, ""), "echo-md");
    return h("div.echo-lines", null, lines.map((l) => memRow(l)));
  }
  // a short answer ("No results.", a date) is a line, not a block
  if (text.length <= 120 && !text.includes("\n")) return h("div.echo-note", { text });
  return h("pre.echo-pre", { text: text.length > 2000 ? `${text.slice(0, 2000)}…` : text });
}

function stepsNode(key, items, live) {
  const calls = [];
  const results = [];
  for (const m of items) (m.kind === "tool" ? calls : results).push(m);
  const parsed = calls.map((m) => parseTool(m.text));
  const memory = parsed.every((c) => MEMORY_TOOLS.has(c.name));
  const label = memory ? (live ? "Looking through memory" : "Looked through memory") : live ? "Working" : "Used tools";
  const isOpen = opened.has(key);
  const node = h(`div.steps${isOpen ? ".open" : ""}${live ? ".live" : ""}`);
  const head = h(
    "button.steps-head",
    { type: "button", "aria-expanded": String(isOpen) },
    live ? icon("loader", "spin") : icon("layers"),
    h("span", { text: label }),
    parsed.length > 1 ? h("span.times", { text: `×${parsed.length}` }) : null,
    icon("chevron", "chev"),
  );
  const body = h("div.steps-body");
  if (isOpen) fill();
  function fill() {
    body.replaceChildren(
      ...parsed.map((call, k) => {
        const res = results[k];
        return h("div.step", null, h("div.step-title", null, icon(STEP_ICON[call.name] ?? "terminal"), h("span", { text: stepTitle(call) })), res ? h("div.step-result", null, echoView(call, res.text)) : live ? h("div.step-wait", null, icon("loader", "spin")) : null);
      }),
    );
  }
  head.addEventListener("click", () => {
    const now = !opened.has(key);
    if (now) {
      opened.add(key);
      fill();
    } else opened.delete(key);
    node.classList.toggle("open", now);
    head.setAttribute("aria-expanded", String(now));
  });
  node.append(head, h("div.steps-fold", null, body));
  return node;
}

const TASK_STATE = {
  running: ["Working", "loader"],
  done: ["Done", "check"],
  error: ["Didn't finish", "alert"],
  stopped: ["Stopped", "stopped"],
};

/// A hand-off's length: "just now", "4 min", "1 h 12 min".
function duration(ms) {
  const m = Math.round(ms / 60000);
  if (m < 1) return "just now";
  if (m < 60) return `${m} min`;
  return `${Math.floor(m / 60)} h${m % 60 ? ` ${m % 60} min` : ""}`;
}

function taskPhase(task) {
  if (!task) return "running";
  if (running(task)) return "running";
  if (/^(error|failed|lost)$/.test(task.state ?? "")) return "error";
  if (task.state === "stopped") return "stopped";
  return "done";
}

function taskSig(task, fallback) {
  const draft = task ? handDraftOf(task) : null;
  return JSON.stringify([task?.state, task?.steps?.length, task?.steps?.at(-1)?.ok, task?.report?.length, draft?.length, task?.text ?? fallback, opened.has(`k:${task?.id}`), opened.has(`r:${task?.id}`)]);
}

function taskNode(id, fallback) {
  const task = S.tasks.get(id) ?? null;
  const phase = taskPhase(task);
  const [label, ic] = TASK_STATE[phase];
  const steps = task?.steps ?? [];
  const showAll = opened.has(`k:${id}`);
  const shown = showAll ? steps : steps.slice(-CARD_STEPS);
  const draft = task ? handDraftOf(task) : null;
  const text = task?.text || fallback || "";
  const reportOpen = opened.has(`r:${id}`);
  const elapsed = task?.started ? duration((task.ended ?? Date.now()) - task.started) : "";
  const toggle = (k) => () => {
    if (opened.has(k)) opened.delete(k);
    else opened.add(k);
    changed();
  };
  return h(
    `div.task.task-${phase}`,
    { id: `task-${id}` },
    h("div.task-head", null, h("span.task-icon", null, icon("monitor")), h("span.task-where", { text: "On your computer" }), h("span.task-state", null, icon(ic, phase === "running" ? "spin" : ""), label), elapsed ? h("span.task-time", { text: elapsed }) : null),
    h("div.task-text", { text: text.replace(/\n\n\(task [^)]*\)\s*$/, "") }),
    steps.length
      ? h(
          "div.task-steps",
          null,
          steps.length > shown.length || showAll ? h("button.linkish", { type: "button", onclick: toggle(`k:${id}`) }, showAll ? "Fewer steps" : `${plural(steps.length - shown.length, "earlier step")}`) : null,
          shown.map((s, k) => {
            const last = showAll ? k === shown.length - 1 : k === shown.length - 1;
            const failed = s.ok === false;
            const wait = phase === "running" && last && s.ok === undefined;
            return h(
              `div.task-step${failed ? ".failed" : ""}`,
              { title: typeof s.excerpt === "string" ? s.excerpt : "" },
              wait ? icon("loader", "spin") : failed ? icon("x") : icon("check"),
              h("span.task-tool", { text: s.tool ?? s.name ?? "step" }),
              h("span.task-args", { text: String(s.args ?? s.title ?? s.text ?? "").replace(/^`|`$/g, "") }),
            );
          }),
        )
      : phase === "running"
        ? h("div.task-steps", null, h("div.task-step.quiet", null, icon("loader", "spin"), h("span.task-args", { text: "Waking your computer…" })))
        : null,
    draft && phase === "running" ? h("div.task-draft", null, md(draft)) : null,
    task?.report
      ? h(
          `div.task-report${reportOpen ? ".open" : ""}`,
          null,
          h("div.task-report-label", null, icon("file"), "Report"),
          md(task.report),
          h("button.linkish", { type: "button", onclick: toggle(`r:${id}`) }, reportOpen ? "Show less" : "Read the whole report"),
        )
      : null,
  );
}

function byline(p, at) {
  return h("div.byline", null, avatar(p, "sm"), h("span.by-name", { text: p?.name ?? "Mind" }), at ? h("time", { text: clock(at), title: when(at) }) : null);
}

function indicator(label) {
  return h("div.working", { role: "status" }, h("span.dots", null, h("i"), h("i"), h("i")), label ? h("span", { text: label }) : null);
}

// the page looks again once a message's "heard" moment has passed
let heardTimer = 0;

/// The thread's items, in order, for `reconcile`.
function items(id) {
  const box = S.msgs.get(id);
  const list = box ? [...box.byI.values()].sort((a, b) => a.i - b.i) : [];
  const out = [];
  const thread = S.threads.get(id);
  let prevAt = null;
  let steps = null;
  let side = null; // "you" | "agent": who spoke last, for bylines
  let you = "start"; // the key of what the person last said: an answer's byline is keyed by it
  let lastAt = 0;
  const placed = new Set();

  // a byline keeps its node from "Thinking" to the draft to the reply
  const agentSide = (p, at) => {
    if (side !== "agent") out.push({ key: `by:${you}`, sig: `${p?.id}|${p?.emoji}|${p?.name}|${at ?? ""}`, make: () => byline(p, at) });
    side = "agent";
  };
  const flush = (live = false) => {
    if (!steps) return;
    const s = steps;
    const key = `s:${s.first}`;
    out.push({ key, sig: `${s.items.length}|${live}|${opened.has(key)}`, make: () => stepsNode(key, s.items, live) });
    steps = null;
  };

  if (box?.more) out.push({ key: "earlier", sig: String(box.loading), make: () => h("button.earlier", { type: "button", onclick: () => earlier(id) }, box.loading ? icon("loader", "spin") : icon("up"), "Earlier in this chat") });

  for (let k = 0; k < list.length; k++) {
    const m = list[k];
    if (prevAt === null || startOfDay(m.at) !== startOfDay(prevAt) || m.at - prevAt > 3 * 3600_000) {
      flush();
      const label = prevAt !== null && startOfDay(m.at) === startOfDay(prevAt) ? clock(m.at) : `${dayLabel(m.at)} · ${clock(m.at)}`;
      out.push({ key: `day:${m.i}`, sig: label, make: () => h("div.divider", null, h("span", { text: label })) });
    }
    prevAt = m.at;
    lastAt = m.at;
    const p = persona(m.persona) ?? persona(thread?.persona) ?? currentPersona();
    if (m.kind === "tool" || m.kind === "echo") {
      const call = m.kind === "tool" ? parseTool(m.text) : null;
      if (call?.name === "computer") {
        flush();
        agentSide(p, m.at);
        const next = list[k + 1];
        const started = next?.kind === "echo" ? reportOf(next.text) : null;
        const tid = m.task ?? started?.task ?? `call-${m.i}`;
        if (started) k++;
        placed.add(tid);
        const text = typeof call.args?.task === "string" ? call.args.task : "";
        out.push({ key: `k:${tid}`, sig: taskSig(S.tasks.get(tid), text), make: () => taskNode(tid, text) });
        continue;
      }
      if (!steps) agentSide(p, m.at);
      steps ??= { first: m.i, items: [] };
      steps.items.push(m);
      continue;
    }
    flush();
    if (m.kind === "user") {
      const rep = reportOf(m.text);
      if (rep && (S.tasks.has(rep.task) || /[\d_]/.test(rep.task))) {
        side = "you";
        you = `r:${m.i}`;
        out.push({ key: you, sig: "r", make: () => h("div.divider.report-mark", null, h("span", null, icon("monitor"), "Your computer reported back")) });
        continue;
      }
      side = "you";
      // a message that was shown as sent keeps that node
      you = S.landed.get(m.i) || `u:${m.i}`;
      out.push({ key: you, sig: m.text, make: () => h("div.msg.you", { title: when(m.at) }, md(m.text)) });
    } else if (m.kind === "talk") {
      agentSide(p, m.at);
      out.push({ key: `a:${m.i}`, sig: m.text, quiet: S.landed.has(m.i), make: () => h("div.msg.agent", null, md(m.text), h("div.msg-tools", null, copyButton(m.text, "Copy reply"))) });
    } else if (m.kind === "note") {
      side = null;
      out.push({ key: `n:${m.i}`, sig: m.text, make: () => h("div.note", null, h("div.note-label", null, icon("note"), "Noted"), md(m.text)) });
    }
  }

  // hand-offs of this thread whose call is not among what is loaded
  const first = list[0]?.at ?? 0;
  for (const t of S.tasks.values()) {
    if (t.thread !== id || placed.has(t.id) || (t.started ?? Date.now()) < first) continue;
    flush();
    out.push({ key: `k:${t.id}`, sig: taskSig(t, t.text), make: () => taskNode(t.id, t.text) });
  }

  const on = busy(id);
  const turn = S.turns.get(id);
  const draft = draftOf(id);
  flush(on && !draft);

  // said here, not yet in the log
  for (const pnd of S.pending.get(id) ?? []) {
    side = "you";
    you = `p:${pnd.key}`;
    lastAt = Math.max(lastAt, pnd.at);
    out.push({
      key: you,
      sig: String(pnd.failed ?? ""),
      make: () =>
        h(
          `div.msg.you.pending${pnd.failed ? ".failed" : ""}`,
          null,
          md(pnd.text),
          pnd.failed ? h("div.failed-note", null, icon("alert"), `Not sent (${pnd.failed}). `, h("button.linkish", { type: "button", onclick: () => resend(id, pnd) }, "Try again")) : null,
        ),
    });
  }

  const p = persona(picked.get(id) ?? thread?.persona) ?? currentPersona();
  if (draft) {
    agentSide(p);
    out.push({
      key: "draft",
      sig: draft,
      make: () => h("div.msg.agent.streaming", null, md(draft)),
      update: (node) => node.replaceChildren(md(draft)),
    });
  } else if (on) {
    agentSide(p);
    const label = S.stopping.has(id) ? "Stopping" : turn?.state === "settling" ? "Gathering what I remember" : list.at(-1)?.kind === "tool" || list.at(-1)?.kind === "echo" ? "" : "Thinking";
    if (label) out.push({ key: "working", sig: label, make: () => indicator(label) });
  } else {
    const last = list.at(-1);
    const unanswered = (S.pending.get(id)?.some((x) => !x.failed) || last?.kind === "user") && !(turn && turn.at >= lastAt);
    const fresh = Date.now() - lastAt < PENDING_MS;
    if (unanswered && fresh) {
      const elsewhere = S.status?.turn?.running && S.status.turn.thread && S.status.turn.thread !== id;
      out.push({ key: "heard", sig: String(elsewhere), make: () => indicator(elsewhere ? "Finishing another chat first" : "") });
      heardTimer ||= setTimeout(() => {
        heardTimer = 0;
        changed();
      }, PENDING_MS);
    } else if (turn && turn.at >= lastAt - 1000 && (turn.state === "error" || turn.state === "stopped")) {
      out.push({
        key: `end:${turn.at}`,
        sig: turn.state + turn.error,
        make: () => h(`div.turn-end.${turn.state}`, null, icon(turn.state === "error" ? "alert" : "stopped"), turn.state === "error" ? `Couldn't answer${turn.error ? `: ${turn.error}` : "."}` : "Stopped."),
      });
    }
  }
  return out;
}

let earlierOf = null;
function earlier(id) {
  earlierOf = id;
  loadEarlier(id);
}

/// The thread screen.
export function threadScreen(id) {
  const title = h("div.top-title");
  const panelBtn = iconButton("panel", "What it did here", () => document.dispatchEvent(new CustomEvent("mind:panel")), "panel-toggle");
  const top = topBar(title, h("span.grow"), panelBtn);
  const col = h("div.column.messages");
  const scroll = h("div.scroll", null, col);
  const comp = composer(id);
  const dock = h("div.dock", null, h("div.dock-inner", null, comp.el));
  const el = h("section.screen.thread", null, top, scroll, dock);
  let painted = false;
  loadThread(id);

  function render() {
    const t = S.threads.get(id);
    const p = persona(picked.get(id) ?? t?.persona) ?? currentPersona();
    const chips = (t?.topics ?? []).map((x) => topicChip(x.id, x.p)).filter(Boolean);
    title.replaceChildren(avatar(p, "sm"), h("div.top-text", null, h("div.top-name", { text: t?.title || "New chat" }), chips.length ? h("div.chips.top-chips", null, chips) : h("div.top-sub", { text: p.name })));
    document.title = t?.title ? `${t.title} · Mind` : "Mind";

    const nearBottom = scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 160;
    const before = scroll.scrollHeight;
    const box = S.msgs.get(id);
    if (!box?.loaded) {
      if (!col.querySelector(".loading-thread")) col.replaceChildren(h("div.loading-thread", null, icon("loader", "spin")));
      comp.render();
      return;
    }
    col.querySelector(".loading-thread")?.remove();
    reconcile(col, items(id), { quiet: !painted });
    comp.render();
    if (earlierOf === id && !box.loading) {
      earlierOf = null;
      scroll.scrollTop += scroll.scrollHeight - before;
    } else if (!painted || nearBottom) scroll.scrollTop = scroll.scrollHeight;
    painted = true;
  }

  return {
    el,
    render,
    focus: () => comp.focus(),
    reveal(key) {
      const node = [...col.children].find((n) => n.__key === key);
      if (!node) return;
      node.scrollIntoView({ behavior: "smooth", block: "center" });
      node.classList.remove("flash");
      void node.offsetWidth;
      node.classList.add("flash");
    },
    toBottom: () => (scroll.scrollTop = scroll.scrollHeight),
  };
}

// ---- New chat ----

const OPENERS = {
  default: ["What did we decide last time?", "Help me think something through", "Catch me up on this week"],
  hands: ["Make me a little web page", "Tidy up a folder on my computer", "Check a website for me"],
  coach: ["Help me plan tomorrow", "I'm stuck on a decision", "Ask me about my week"],
};

function prompts(p) {
  const name = (p?.name ?? "").toLowerCase();
  const base = p?.hands ? OPENERS.hands : name.includes("coach") ? OPENERS.coach : OPENERS.default;
  const topics = [...(S.topics ?? [])].sort((a, b) => (b.count ?? 0) - (a.count ?? 0));
  const out = [...base.slice(0, 2)];
  if (topics[0]) out.push(`What's new with ${topics[0].name.toLowerCase()}?`);
  const last = S.threads.get(S.recent[0]);
  if (last?.title) out.push(`More on “${firstLine(last.title, 34)}”`);
  else out.push(base[2]);
  return out.slice(0, 4);
}

function hello(p) {
  const name = (p?.name ?? "").toLowerCase();
  if (p?.hands) return "What should we make?";
  if (name.includes("coach")) return "What are you working through?";
  if (!p || name === "mind") return "What's on your mind?";
  return `Talk with ${p.name}`;
}

/// New chat: an empty screen with the current persona; nothing is made
/// until its first message.
export function newScreen() {
  const comp = composer(null, { onSent: (id) => go(`/t/${id}`) });
  const face = h("div.hello-face");
  const line = h("p.hello-greet");
  const head = h("h1.hello-title");
  const sub = h("p.hello-sub");
  const ideas = h("div.ideas");
  const stage = h("div.new-stage", null, h("div.hello", null, face, line, head, sub), comp.el, ideas);
  const el = h("section.screen.new", null, topBar(h("span.grow")), h("div.new-scroll", null, stage));
  let sig = "";

  function render() {
    const p = currentPersona();
    const name = S.person?.name?.split(" ")[0];
    const next = JSON.stringify([p.id, p.emoji, p.name, p.hands, name, S.topics?.length, S.recent[0], S.status?.T]);
    comp.render();
    if (next === sig) return;
    sig = next;
    face.replaceChildren(avatar(p, "xl"));
    line.textContent = `${greeting()}${name ? `, ${name}` : ""}.`;
    head.textContent = hello(p);
    const T = S.status?.T;
    sub.textContent = T ? `A fresh page. I still remember all ${T.toLocaleString()} messages before it.` : "A fresh page. I still remember everything before it.";
    ideas.replaceChildren(...prompts(p).map((t) => h("button.idea", { type: "button", onclick: () => comp.set(t) }, h("span", { text: t }))));
    document.title = "Mind";
  }
  return { el, render, focus: () => comp.focus() };
}

export { miniMessage };
