// A thread (a screen of the one chat), the composer, and New chat.
//
// A thread's messages come from `thread` and then from `log` as they are
// written. A turn's tool calls and their results fold into one quiet
// "Looked through memory ×3" row, which opens to each step, and each
// result's lines open further (a search hit to its context, a memory line
// to its parts); a turn that used the person's apps reads "Used todo:
// add", the app's name a link to it. A `computer` call is a hand-off
// card: goose's steps as
// they come, its words as they stream, then its report. The mind's reply
// streams in as `log`'s draft `turn:<thread>` until its `talk` arrives.

import { avatar, contextView, filesNode, go, memRow, topicChip } from "./pieces.js";
import {
  EMBED,
  F,
  FILES_MAX,
  FILE_MAX_BYTES,
  PENDING_MS,
  S,
  busy,
  changed,
  currentPersona,
  draftOf,
  handDraftOf,
  handOff,
  loadEarlier,
  loadThread,
  persona,
  problem,
  putThread,
  resend,
  running,
  say,
  stop,
} from "./store.js";
import { WEB_TOOLS, clock, dayLabel, firstLine, greeting, h, hostOf, icon, iconButton, isReportKind, joined, md, parseTool, plural, reconcile, reportOf, size, threadId, viewLines, when, copyButton } from "./ui.js";

/// A message's longest text (the log caps at 30 000 characters).
const TEXT_MAX = 30_000;
const MEMORY_TOOLS = new Set(["zoom", "search", "date"]);
const APP_TOOLS = new Set(["apps", "app_ops", "app_call"]);
/// The most of a web page or a research answer a step shows.
const WEB_SHOWN = 6000;
/// Steps a hand-off card shows before "N earlier".
const CARD_STEPS = 4;

// unsent words, by thread ("" for New chat), kept across screens
const unsent = new Map();
// files picked for a thread's next message (File objects), kept the same way
const unsentFiles = new Map();
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
/// makes the thread with its first message. Files come from its paperclip,
/// a paste, or a drop (`droppable`), and go with the message.
export function composer(thread, { onSent } = {}) {
  const key = thread ?? "";
  const ta = h("textarea.input", { rows: 1, "aria-label": "Message", maxlength: TEXT_MAX, value: unsent.get(key) ?? "" });
  const chipFace = h("span.chip-face");
  const chip = h("button.persona-chip", { type: "button", "aria-haspopup": "menu", "aria-expanded": "false", title: "Who answers" }, chipFace, icon("down", "chev"));
  const stopBtn = h("button.round.stop", { type: "button", "aria-label": "Stop", title: "Stop" }, icon("stop"));
  const sendBtn = h("button.round.send", { type: "submit", "aria-label": "Send", title: "Send (Enter)" }, icon("send"));
  const hint = h("span.composer-hint");
  const files = unsentFiles.get(key) ?? [];
  unsentFiles.set(key, files);
  const tray = h("div.files.tray", { hidden: !files.length });
  const picker = h("input", { type: "file", multiple: true, hidden: true, tabindex: "-1", "aria-hidden": "true" });
  const attach = iconButton("paperclip", "Attach files", () => picker.click(), "attach");
  const form = h("form.composer", null, tray, ta, h("div.composer-row", null, h("div.chip-wrap", null, chip), attach, hint, h("span.grow"), stopBtn, sendBtn), picker);

  const personaId = () => picked.get(key) ?? (thread ? S.threads.get(thread)?.persona : null) ?? currentPersona().id;
  const coarse = matchMedia("(pointer: coarse)").matches;
  const ready = () => !!ta.value.trim() || files.length > 0;

  function grow() {
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 280)}px`;
    sendBtn.disabled = !ready();
  }
  function renderFiles() {
    // the screen keeps its end in sight as the tray takes room
    changed();
    tray.hidden = !files.length;
    tray.replaceChildren(
      ...files.map((f) =>
        h(
          "span.file-chip",
          null,
          icon("file"),
          h("span.file-name", { text: f.name || "a file" }),
          h("span.file-size", { text: size(f.size) }),
          iconButton("x", `Remove ${f.name || "the file"}`, () => {
            files.splice(files.indexOf(f), 1);
            renderFiles();
            grow();
          }),
        ),
      ),
    );
  }
  function addFiles(list) {
    for (const file of list) {
      if (files.length >= FILES_MAX) {
        problem(`A message carries at most ${FILES_MAX} files.`);
        break;
      }
      if (file.size > FILE_MAX_BYTES) problem(`${file.name} is over ${size(FILE_MAX_BYTES)}, the most a message takes.`);
      else files.push(file);
    }
    renderFiles();
    grow();
    ta.focus();
  }
  picker.addEventListener("change", () => {
    addFiles([...picker.files]);
    picker.value = "";
  });
  ta.addEventListener("paste", (e) => {
    const list = [...(e.clipboardData?.files ?? [])];
    if (!list.length) return;
    e.preventDefault();
    addFiles(list);
  });
  renderFiles();
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
    if (!ready()) return;
    const sending = files.splice(0);
    const text = ta.value.trim();
    const pid = personaId();
    let id = thread;
    if (!id) {
      id = threadId();
      // titled as the mind titles it: its first line, else its first file's name
      putThread({ id, title: firstLine(text || sending[0]?.name || "", 80), persona: pid, started: Date.now(), last: Date.now(), count: 0, topics: [] });
      S.msgs.set(id, { byI: new Map(), more: false, loaded: true, loading: false });
      S.recent = [id, ...S.recent];
    }
    ta.value = "";
    unsent.delete(key);
    renderFiles();
    grow();
    say(id, text, pid, sending);
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
    sendBtn.disabled = !ready();
  }
  render();
  // words kept from before size the box once it is on the page
  requestAnimationFrame(grow);
  return { el: form, render, addFiles, focus: () => ta.focus(), set: (text) => ((ta.value = text), unsent.set(key, text), grow(), ta.focus()) };
}

/// Files dropped anywhere on `target` (a screen) go to its composer, which
/// shows it can take them while they are over it.
function droppable(target, comp) {
  let over = 0;
  const has = (e) => [...(e.dataTransfer?.types ?? [])].includes("Files");
  const leave = () => {
    over = 0;
    comp.el.classList.remove("dropping");
  };
  target.addEventListener("dragenter", (e) => {
    if (!has(e)) return;
    over++;
    comp.el.classList.add("dropping");
  });
  target.addEventListener("dragleave", () => --over <= 0 && leave());
  target.addEventListener("dragover", (e) => has(e) && e.preventDefault());
  target.addEventListener("drop", (e) => {
    leave();
    if (!e.dataTransfer?.files.length) return;
    e.preventDefault();
    comp.addFiles([...e.dataTransfer.files]);
  });
}

/// A message's words and its files (a message may be files alone).
const said = (text, files) => [text ? md(text) : null, files.length ? filesNode(files) : null];

/// A message's files before they are sent: an image's own preview, or a
/// chip, turning while it uploads.
function pendingFiles(pnd) {
  return h(
    "div.files",
    null,
    pnd.files.map((f) => {
      const up = pnd.uploading && !f.sent;
      if (f.preview) return h(`span.file-image${up ? ".uploading" : ""}`, null, h("img", { src: f.preview, alt: f.name }));
      return h(`span.file-chip${up ? ".uploading" : ""}`, null, icon(up ? "loader" : "file", up ? "spin" : ""), h("span.file-name", { text: f.name }), h("span.file-size", { text: size(f.size) }));
    }),
  );
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
    case "web_search":
      return `Searched the web for “${a.q ?? ""}”`;
    case "web_fetch":
      return `Read ${hostOf(a.url) || "a page"}`;
    case "research":
      return `Researched “${firstLine(a.question ?? "", 70)}”`;
    case "apps":
      return "Looked at your apps";
    case "app_ops":
      return `Read what ${appLabel(a.fragment)} can do`;
    case "app_call": {
      const input = a.input && typeof a.input === "object" && Object.keys(a.input).length ? ` ${firstLine(JSON.stringify(a.input), 60)}` : "";
      return `${appLabel(a.fragment)}: ${a.op ?? ""}${input}`;
    }
    default:
      return `${call.name}${a && Object.keys(a).length ? ` ${firstLine(JSON.stringify(a), 80)}` : ""}`;
  }
}

const STEP_ICON = { zoom: "zoom", search: "search", date: "calendar", web_search: "globe", web_fetch: "link", research: "sparkles", apps: "grid", app_ops: "grid", app_call: "grid" };

/// An app as a person reads it: its label (`todo` of `todo.paul`).
const appLabel = (name) => String(name ?? "").split(".")[0] || "an app";

/// An app call's echo names the app and where it is on its first line
/// (`<app> <op> at <url>`: app.mjs), its result below.
function appCalled(text) {
  const m = String(text ?? "").match(/^(\S+) (\S+) at (https?:\/\/\S+)\n?([\s\S]*)$/);
  return m ? { name: m[1], op: m[2], url: m[3], result: m[4] } : null;
}

/// A link to one of the person's apps: in the shell it opens beside the
/// mind (mind.js), else in a tab.
const appLink = (name, url) => (url ? h("a.app-link", { href: url, target: "_blank", rel: "noopener noreferrer", text: appLabel(name) }) : h("span", { text: appLabel(name) }));

/// A web search's results (applib/web.mjs `searchText`): `N. title`, then
/// its URL and its snippet, each on a line indented three spaces.
function webHits(text) {
  const out = [];
  for (const m of String(text ?? "").matchAll(/^\d+\. (.+)\n {3}(https?:\/\/\S+)(?:\n {3}(.+))?/gm)) out.push({ title: m[1], url: m[2], snippet: m[3] ?? "" });
  return out.slice(0, 20);
}

/// A tool's result, readable: search hits and memory lines open in place;
/// the web's results are links, its pages and answers markdown.
function echoView(call, text) {
  if (call?.name === "app_call") {
    const called = appCalled(text);
    if (called) return h("div.echo-app", null, h("div.echo-note", null, appLink(called.name, called.url), ` ${called.op}`), h("pre.echo-pre", { text: called.result.length > 2000 ? `${called.result.slice(0, 2000)}…` : called.result }));
  }
  if (call?.name === "web_search") {
    const hits = webHits(text);
    if (hits.length) return h("div.web-hits", null, hits.map((x) => h("a.web-hit", { href: x.url, target: "_blank", rel: "noopener noreferrer" }, h("span.web-title", { text: x.title }), h("span.web-host", { text: hostOf(x.url) }), x.snippet ? h("span.web-snippet", { text: x.snippet }) : null)));
  }
  if (call && WEB_TOOLS.has(call.name)) return md(text.length > WEB_SHOWN ? `${text.slice(0, WEB_SHOWN)}…` : text, "echo-md");
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

/// What a row of steps that used the person's apps (and looked through
/// memory or the web) says: "Used" each app called, a link to it, and what
/// was done there; or that it looked at them. Null for any other row.
function appsLabel(parsed, results, live) {
  const known = parsed.every((c) => APP_TOOLS.has(c.name) || MEMORY_TOOLS.has(c.name) || WEB_TOOLS.has(c.name));
  if (!known || !parsed.some((c) => APP_TOOLS.has(c.name))) return null;
  const apps = new Map();
  parsed.forEach((c, k) => {
    if (c.name !== "app_call" || typeof c.args?.fragment !== "string") return;
    const app = apps.get(c.args.fragment) ?? { url: null, ops: [] };
    app.url ??= appCalled(results[k]?.text)?.url ?? null;
    if (typeof c.args.op === "string" && !app.ops.includes(c.args.op)) app.ops.push(c.args.op);
    apps.set(c.args.fragment, app);
  });
  if (!apps.size) return [live ? "Looking at your apps" : "Looked at your apps"];
  const out = [live ? "Using " : "Used "];
  [...apps].forEach(([name, app], k) => out.push(k ? "; " : "", appLink(name, app.url), app.ops.length ? `: ${app.ops.join(", ")}` : ""));
  return out;
}

function stepsNode(key, items, live) {
  const calls = [];
  const results = [];
  for (const m of items) (m.kind === "tool" ? calls : results).push(m);
  const parsed = calls.map((m) => parseTool(m.text));
  const used = appsLabel(parsed, results, live);
  const memory = parsed.every((c) => MEMORY_TOOLS.has(c.name));
  const web = parsed.every((c) => WEB_TOOLS.has(c.name));
  const looked = parsed.every((c) => MEMORY_TOOLS.has(c.name) || WEB_TOOLS.has(c.name));
  const label = memory
    ? live ? "Looking through memory" : "Looked through memory"
    : web
      ? live ? "Searching the web" : "Searched the web"
      : looked
        ? live ? "Looking through memory and the web" : "Looked through memory and the web"
        : live ? "Working" : "Used tools";
  const isOpen = opened.has(key);
  const node = h(`div.steps${isOpen ? ".open" : ""}${live ? ".live" : ""}`);
  // it acts as a button and may hold a link (an app's), which a button may not
  const head = h(
    "div.steps-head",
    { role: "button", tabindex: "0", "aria-expanded": String(isOpen) },
    live ? icon("loader", "spin") : icon(used ? "grid" : web ? "globe" : "layers"),
    used ? h("span", null, used) : h("span", { text: label }),
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
  const toggle = () => {
    const now = !opened.has(key);
    if (now) {
      opened.add(key);
      fill();
    } else opened.delete(key);
    node.classList.toggle("open", now);
    head.setAttribute("aria-expanded", String(now));
  };
  // an app's link opens the app (beside the mind in the shell, else a tab)
  head.addEventListener("click", (e) => e.target.closest?.("a") || toggle());
  head.addEventListener("keydown", (e) => {
    if (e.target !== head || (e.key !== "Enter" && e.key !== " ")) return;
    e.preventDefault();
    toggle();
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

function phaseOf(state) {
  if (running({ state })) return "running";
  if (/^(error|failed|lost)$/.test(state ?? "")) return "error";
  if (state === "stopped") return "stopped";
  return "done";
}

const reveal = (key) => document.dispatchEvent(new CustomEvent("mind:reveal", { detail: key }));

function taskSig(id, fallback, report) {
  const task = S.tasks.get(id);
  const ho = task ? handOff(task) : null;
  const draft = task ? handDraftOf(task) : null;
  const asks = (ho?.steps ?? []).filter((s) => s.kind === "turn.prompt").map((s) => [s.prompt, s.outcome, answering.get(s.prompt), Number.isFinite(s.expiresAt) && Date.now() > s.expiresAt]);
  return JSON.stringify([ho?.state, ho?.steps.length, ho?.steps.at(-1)?.ok, ho?.started, ho?.ended, task?.report?.length, draft?.length, task?.text ?? fallback, report, opened.has(`k:${id}`), opened.has(`r:${id}`), asks, S.me?.principal]);
}

// prompts this page answered, while the agent closes them: prompt -> option
const answering = new Map();

/// An answer to goose's question on a hand-off (docs/chat-records.md,
/// "A prompt's answer"): on `chat`, as the person, once (`pr:<prompt>`).
async function answer(prompt, option) {
  answering.set(prompt, option);
  changed();
  try {
    await F.post("chat", { kind: "prompt_response", prompt, option }, { id: `pr:${prompt}` });
  } catch (e) {
    answering.delete(prompt);
    problem(`Could not answer: ${e.message}`);
  }
}

/// goose asking the person (a `turn.prompt` among a hand-off's steps):
/// its buttons while it is open and theirs to answer, else how it closed.
function askRow(s, phase) {
  const options = Array.isArray(s.options) ? s.options.filter((o) => o && typeof o.id === "string") : [];
  const label = (id) => options.find((o) => o.id === id)?.label || id;
  const expired = Number.isFinite(s.expiresAt) && Date.now() > s.expiresAt;
  const mine = !!S.me && s.asks === S.me.principal;
  const sent = answering.get(s.prompt);
  let foot;
  if (typeof s.outcome === "string") foot = h("div.ask-done", { text: s.outcome === "answered" ? `Answered: ${label(s.option)}` : s.outcome === "expired" ? "No answer in time" : "Stopped" });
  else if (phase !== "running" || expired) foot = h("div.ask-done", { text: "No answer in time" });
  else if (!mine) foot = h("div.ask-done", { text: "Waiting for its owner's answer" });
  else
    foot = h(
      "div.ask-options",
      null,
      options.map((o) => h(`button.ask-opt${o.style === "primary" || o.style === "danger" ? `.${o.style}` : ""}`, { type: "button", disabled: !!sent, onclick: () => answer(s.prompt, o.id) }, sent === o.id ? icon("loader", "spin") : null, o.label || o.id)),
    );
  return h("div.task-ask", null, h("div.ask-text", null, icon("hand"), h("span", { text: s.text || "Your computer asks" })), foot);
}

/// A report long enough to fold.
const longReport = (text) => text.length > 360 || text.split("\n").length > 6;

/// A hand-off's card: what was asked, goose's steps (and questions) as they
/// come, its words as they stream, how it ended. Its report is the thread's
/// `[<task>] …` message (`report`, that item's key) when the thread holds
/// it, shown where it arrived; else the task's own.
function taskNode(id, fallback, report) {
  const task = S.tasks.get(id) ?? null;
  const ho = task ? handOff(task) : { steps: [], state: "running", started: null, ended: null };
  const phase = phaseOf(ho.state);
  const steps = ho.steps;
  const showAll = opened.has(`k:${id}`);
  const shown = showAll ? steps : steps.slice(-CARD_STEPS);
  const draft = task ? handDraftOf(task) : null;
  const text = task?.text || fallback || "";
  const own = !report && task?.report ? task.report : "";
  const long = !!own && longReport(own);
  const reportOpen = !long || opened.has(`r:${id}`);
  const elapsed = ho.started ? duration((ho.ended ?? Date.now()) - ho.started) : "";
  // a question open is what it waits on; else, between steps, it works
  const asking = phase === "running" && steps.some((s) => s.kind === "turn.prompt" && typeof s.outcome !== "string" && !(Number.isFinite(s.expiresAt) && Date.now() > s.expiresAt));
  const [label, ic] = asking ? ["Needs you", "hand"] : TASK_STATE[phase];
  const toggle = (k) => () => {
    if (opened.has(k)) opened.delete(k);
    else opened.add(k);
    changed();
  };
  return h(
    `div.task.task-${phase}`,
    { id: `task-${id}` },
    h(
      "div.task-head",
      null,
      h("span.task-icon", null, icon("monitor")),
      h("span.task-where", { text: "On your computer" }),
      // in the shell, its screen opens beside the chat while it works
      EMBED && phase === "running" ? h("button.task-watch", { type: "button", title: "Watch its screen", onclick: () => document.dispatchEvent(new CustomEvent("mind:screen")) }, icon("monitor"), "Watch") : null,
      h(`span.task-state${asking ? ".asking" : ""}`, null, icon(ic, phase === "running" && !asking ? "spin" : ""), label),
      elapsed ? h("span.task-time", { text: elapsed }) : null,
    ),
    h("div.task-text", { text: text.replace(/\n\n\(task [^)]*\)\s*$/, "") }),
    steps.length
      ? h(
          "div.task-steps",
          null,
          steps.length > shown.length || showAll ? h("button.linkish", { type: "button", onclick: toggle(`k:${id}`) }, showAll ? "Fewer steps" : `${plural(steps.length - shown.length, "earlier step")}`) : null,
          shown.map((s) => {
            if (s.kind === "turn.prompt") return askRow(s, phase);
            // a step is posted once its call ends: ok, failed, or unsaid (null)
            const failed = s.ok === false;
            return h(
              `div.task-step${failed ? ".failed" : ""}${s.ok == null ? ".unsaid" : ""}`,
              { title: typeof s.excerpt === "string" ? s.excerpt : "" },
              failed ? icon("x") : s.ok === true ? icon("check") : icon("dot"),
              h("span.task-tool", { text: s.tool ?? s.name ?? "step" }),
              h("span.task-args", { text: String(s.args ?? s.title ?? s.text ?? "").replace(/^`|`$/g, "") }),
            );
          }),
          // between steps, with no words streaming and nothing asked: still going
          phase === "running" && !asking && !draft ? h("div.task-step.quiet", null, icon("loader", "spin"), h("span.task-args", { text: "Working…" })) : null,
        )
      : phase === "running"
        ? h("div.task-steps", null, h("div.task-step.quiet", null, icon("loader", "spin"), h("span.task-args", { text: "Waking your computer…" })))
        : null,
    draft && phase === "running" ? h("div.task-draft", null, md(draft)) : null,
    report
      ? h("div.task-foot", null, h("button.linkish", { type: "button", onclick: () => reveal(report) }, icon("file"), "Its report, below"))
      : own
        ? h(
            `div.task-report${reportOpen ? ".open" : ""}`,
            null,
            h("div.task-report-label", null, icon("file"), "Report"),
            md(own),
            long ? h("button.linkish", { type: "button", onclick: toggle(`r:${id}`) }, reportOpen ? "Show less" : "Read the whole report") : null,
          )
        : null,
  );
}

/// A hand-off's report, where it reached the thread (the mind logs it as a
/// `user` message `[<task>] …`, which starts its next turn).
function reportNode(m, rep, key) {
  const failed = /^ended: /.test(rep.text);
  const long = longReport(rep.text);
  const isOpen = !long || opened.has(key);
  const task = S.tasks.get(rep.task);
  const node = h(
    `div.report${failed ? ".failed" : ""}${isOpen ? ".open" : ""}`,
    null,
    h(
      "div.report-head",
      null,
      h("span.task-icon", null, icon("monitor")),
      h("span.report-title", { text: failed ? "Your computer stopped" : "Your computer reported back" }),
      h("time", { text: clock(m.at), title: when(m.at) }),
    ),
    md(failed ? rep.text.replace(/^ended: /, "Ended: ") : rep.text),
    // what the computer made, as goose attached it
    m.attachments.length ? filesNode(m.attachments) : null,
    h(
      "div.report-foot",
      null,
      long ? h("button.linkish", { type: "button", onclick: () => (opened.has(key) ? opened.delete(key) : opened.add(key), changed()) }, isOpen ? "Show less" : "Read the whole report") : null,
      h("span.grow"),
      task ? h("button.linkish", { type: "button", onclick: () => reveal(`k:${rep.task}`) }, "What it did", icon("up")) : null,
    ),
  );
  return node;
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
  const list = box ? joined([...box.byI.values()].sort((a, b) => a.i - b.i)) : [];
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

  // the hand-offs whose report this thread holds: task -> the report's key
  const isReport = (rep) => rep && (S.tasks.has(rep.task) || /[\d_-]/.test(rep.task));
  const reports = new Map();
  for (const m of list) {
    const rep = isReportKind(m) ? reportOf(m.text) : null;
    if (isReport(rep)) reports.set(rep.task, `r:${m.i}`);
  }

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
        const report = reports.get(tid);
        out.push({ key: `k:${tid}`, sig: taskSig(tid, text, report), make: () => taskNode(tid, text, report) });
        continue;
      }
      if (!steps) agentSide(p, m.at);
      steps ??= { first: m.i, items: [] };
      steps.items.push(m);
      continue;
    }
    flush();
    if (m.kind === "user" || m.kind === "work") {
      const rep = reportOf(m.text);
      if (isReport(rep)) {
        // the hand-off's report, where it came; the mind answers it next
        side = "you";
        you = `r:${m.i}`;
        const key = you;
        out.push({ key, sig: `${m.text}|${opened.has(key)}|${S.tasks.has(rep.task)}`, make: () => reportNode(m, rep, key) });
        continue;
      }
      side = "you";
      // a message that was shown as sent keeps that node
      you = S.landed.get(m.i) || `u:${m.i}`;
      out.push({ key: you, sig: `${m.text}|${m.attachments.length}`, make: () => h("div.msg.you", { title: when(m.at) }, said(m.text, m.attachments)) });
    } else if (m.kind === "talk") {
      agentSide(p, m.at);
      out.push({ key: `a:${m.i}`, sig: `${m.text}|${m.attachments.length}`, quiet: S.landed.has(m.i), make: () => h("div.msg.agent", null, said(m.text, m.attachments), h("div.msg-tools", null, copyButton(m.text, "Copy reply"))) });
    } else if (m.kind === "note") {
      side = null;
      out.push({ key: `n:${m.i}`, sig: m.text, make: () => h("div.note", null, h("div.note-label", null, icon("note"), "Noted"), md(m.text)) });
    }
  }

  // hand-offs of this thread whose call is not among what is loaded
  // (one whose call is on an earlier page stays there)
  const firstI = list[0]?.i ?? 0;
  const firstAt = list[0]?.at ?? 0;
  for (const t of S.tasks.values()) {
    if (t.thread !== id || placed.has(t.id)) continue;
    if (Number.isInteger(t.i) ? t.i < firstI : (t.started ?? Infinity) < firstAt) continue;
    flush();
    const report = reports.get(t.id);
    out.push({ key: `k:${t.id}`, sig: taskSig(t.id, t.text, report), make: () => taskNode(t.id, t.text, report) });
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
      sig: `${pnd.failed ?? ""}|${!!pnd.uploading}`,
      make: () =>
        h(
          `div.msg.you.pending${pnd.failed ? ".failed" : ""}`,
          null,
          pnd.text ? md(pnd.text) : null,
          pnd.files.length ? pendingFiles(pnd) : null,
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
    const unanswered = (S.pending.get(id)?.some((x) => !x.failed) || last?.kind === "user" || last?.kind === "work") && !(turn && turn.at >= lastAt);
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
  let painted = false;
  let sent = false; // what one says is followed to the bottom, wherever the scroll was
  const comp = composer(id, { onSent: () => (sent = true) });
  const dock = h("div.dock", null, h("div.dock-inner", null, comp.el));
  const el = h("section.screen.thread", null, top, scroll, dock);
  droppable(el, comp);
  // a picture that loads after the thread is painted keeps it at its end, if it was there
  let atEnd = true;
  scroll.addEventListener("scroll", () => (atEnd = scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 160));
  scroll.addEventListener("load", (e) => e.target.tagName === "IMG" && atEnd && (scroll.scrollTop = scroll.scrollHeight), true);
  loadThread(id);

  function render() {
    const t = S.threads.get(id);
    const p = persona(picked.get(id) ?? t?.persona) ?? currentPersona();
    const head = JSON.stringify([p.id, p.emoji, p.name, t?.title, t?.topics, (S.topics ?? []).length]);
    if (title.__sig !== head) {
      title.__sig = head;
      const chips = (t?.topics ?? []).map((x) => topicChip(x.id, x.p)).filter(Boolean);
      title.replaceChildren(avatar(p, "sm"), h("div.top-text", null, h("div.top-name", { text: t?.title || "New chat" }), chips.length ? h("div.chips.top-chips", null, chips) : h("div.top-sub", { text: p.name })));
      document.title = t?.title ? `${t.title} · Mind` : "Mind";
    }

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
    } else if (!painted || nearBottom || sent) scroll.scrollTop = scroll.scrollHeight;
    painted = true;
    sent = false;
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
  // a mind with nothing in it yet: things worth remembering
  first: ["Remember that I…", "Here's what I'm working on", "Help me plan my week"],
};

function prompts(p) {
  const name = (p?.name ?? "").toLowerCase();
  if (S.status?.T === 0) return OPENERS.first;
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
  droppable(el, comp);
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
    sub.textContent = T ? `A fresh page. I still remember all ${T.toLocaleString()} messages before it.` : T === 0 ? "Say anything. Every chat after this one will remember it." : "A fresh page. I still remember everything before it.";
    ideas.replaceChildren(...prompts(p).map((t) => h("button.idea", { type: "button", onclick: () => comp.set(t) }, h("span", { text: t }))));
    document.title = "Mind";
  }
  return { el, render, focus: () => comp.focus() };
}
