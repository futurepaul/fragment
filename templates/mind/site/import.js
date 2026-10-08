// Chat upload (docs/optchat.md, "Importing chats"): one file a person
// picks, read here in the page, played into the mind as `fragment mind
// import` plays it: claude.ai's export (`conversations.json`), a Claude
// Code session (`~/.claude/projects/*/<session>.jsonl`) or a Codex rollout
// (`~/.codex/sessions/**/rollout-*.jsonl`). The person's words and each
// turn's final reply go, oldest conversation first, each in parts of the
// mind's `import` mutation from where `imported` says it stopped (a second
// upload of the same file sends nothing again); then the page follows the
// compactor through `status`. The parsing is the CLI's (cli/src/import.rs),
// kept to these three formats; the CLI reads more (Hermes, folders).
//
// mountImport(container) fills `container` and keeps it current while it
// is in the document. The settings (settings.js) show it.
import { F } from "./store.js";
import { h, plural } from "./ui.js";

// A message's most characters, as the mind caps one at logging (its CAP);
// a part's messages and JSON, as `import` takes them (fragment.json).
const CAP = 30_000;
const CAP_NOTE_ROOM = 80;
const NODE = 512;
const PART_MESSAGES = 64;
const PART_BYTES = 192 * 1024;
const PASTE_MIN_BYTES = 1024;
const ASK_MAX = 200;
// A file larger than this is the CLI's to read (a browser holds it whole).
const FILE_MAX_BYTES = 512 * 1024 * 1024;
const POLL_MS = 5000;

const utf8 = (s) => new TextEncoder().encode(s).length;

/// The mind's capText: at most CAP characters, head and tail kept.
function capText(text) {
  const cps = Array.from(text);
  if (cps.length <= CAP) return text;
  const room = CAP - CAP_NOTE_ROOM;
  const head = Math.ceil(room / 2);
  const tail = room - head;
  return `${cps.slice(0, head).join("")}\n\n[… ${cps.length - head - tail} characters cut here …]\n\n${cps.slice(cps.length - tail).join("")}`;
}

/// Ms since the epoch: an ISO 8601 string (no zone: UTC), or seconds or ms.
function timeOf(v) {
  if (typeof v === "number" && Number.isFinite(v) && v > 0) return Math.round(v < 1e11 ? v * 1000 : v);
  if (typeof v !== "string" || !/^\d{4}-\d{2}-\d{2}/.test(v)) return null;
  const zoned = /(Z|[+-]\d{2}:?\d{2})$/i.test(v) || v.length === 10 ? v : `${v}Z`;
  const ms = Date.parse(zoned);
  return Number.isFinite(ms) ? ms : null;
}

function stripBlocks(text, tag) {
  const open = `<${tag}>`;
  const close = `</${tag}>`;
  let out = "";
  let rest = text;
  for (let at = rest.indexOf(open); at >= 0; at = rest.indexOf(open)) {
    out += rest.slice(0, at);
    const end = rest.indexOf(close, at);
    rest = end < 0 ? "" : rest.slice(end + close.length);
  }
  return out + rest;
}

const between = (text, tag) => {
  const m = new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`).exec(text);
  return m ? m[1] : null;
};

/// One block of a harness's markup (`<environment_context>…`), not typed words.
const wrapped = (t) => /^<[a-z][a-z0-9_-]*[\s\S]*>$/.test(t.trim());

/// The texts of a content list's parts of these types, joined.
function partsText(content, types = ["text"]) {
  if (typeof content === "string") return content.trim();
  if (!Array.isArray(content)) return "";
  return content
    .filter((p) => p && types.includes(p.type) && typeof p.text === "string")
    .map((p) => p.text.trim())
    .filter(Boolean)
    .join("\n\n");
}

/// The person's words and, of each turn, the agent's last text after its
/// last tool call (its final reply).
class Turns {
  out = [];
  reply = null;
  user(text, at, key = null) {
    this.flush();
    const t = String(text).trim();
    if (!t) return;
    const last = this.out[this.out.length - 1];
    if (last && last.role === "user" && last.text === t) return;
    this.out.push({ role: "user", text: t, at, key });
  }
  said(text, at, group = null) {
    const t = String(text ?? "").trim();
    if (!t) return;
    const r = this.reply;
    if (r && !r.stale && group !== null && r.group === group) r.text += `\n\n${t}`;
    else this.reply = { text: t, at, group, stale: false };
  }
  saidIfNone(text, at) {
    if (!this.reply || this.reply.stale) this.said(text, at);
  }
  tool() {
    if (this.reply) this.reply.stale = true;
  }
  flush() {
    if (this.reply && !this.reply.stale) this.out.push({ role: "assistant", text: this.reply.text, at: this.reply.at, key: null });
    this.reply = null;
  }
  finish() {
    this.flush();
    return this.out;
  }
}

function conversation(source, id, title, started, messages) {
  if (!messages.some((m) => m.role === "user")) return null;
  const s = Math.min(started ?? Infinity, messages[0]?.at ?? Infinity);
  const named = typeof title === "string" ? title.replace(/\s+/g, " ").trim().slice(0, 200) : "";
  return { source, id: String(id).slice(0, 200), title: named || null, started: Number.isFinite(s) ? s : 0, messages };
}

const HARNESS = [
  "<local-command-stdout>", "<local-command-stderr>", "<local-command-caveat>", "<command-message>",
  "Caveat: The messages below were generated", "<bash-input>", "<bash-stdout>", "<bash-stderr>",
  "<task-notification>", "[Request interrupted", "This session is being continued from a previous conversation",
];

function claudeCodeUserText(raw) {
  const t = stripBlocks(raw, "system-reminder").trim();
  if (t.includes("<command-name>")) {
    const name = (between(t, "command-name") ?? "").trim();
    const args = (between(t, "command-args") ?? "").trim();
    return args ? `${name} ${args}` : null;
  }
  return !t || HARNESS.some((x) => t.startsWith(x)) ? null : t;
}

const humanOrigin = (r) => r?.origin == null || r.origin.kind === "human";

/// A Claude Code session file: a conversation for each session its records
/// name (a fork carries its original's records under that one's id).
function claudeCode(records, stem) {
  const sessions = new Map();
  for (const r of records) {
    if (r.isSidechain === true) continue;
    const sid = typeof r.sessionId === "string" ? r.sessionId : stem;
    if (!sessions.has(sid)) sessions.set(sid, { turns: new Turns(), title: null, summary: null });
    const s = sessions.get(sid);
    const at = timeOf(r.timestamp);
    if (r.type === "custom-title" && typeof r.customTitle === "string") s.title = r.customTitle;
    else if (r.type === "summary" && !s.summary && typeof r.summary === "string") s.summary = r.summary;
    else if (r.type === "user") {
      const content = r.message?.content;
      if (r.isMeta === true || r.isVisibleInTranscriptOnly === true || r.toolUseResult != null) continue;
      if (Array.isArray(content) && content.some((p) => p?.type === "tool_result")) continue;
      const raw = partsText(content);
      if (r.isCompactSummary === true || !humanOrigin(r) || raw.trimStart().startsWith("<task-notification>")) {
        s.turns.flush();
        continue;
      }
      const text = claudeCodeUserText(raw);
      if (at !== null && text) s.turns.user(text, at, r.uuid ?? null);
    } else if (r.type === "assistant") {
      const m = r.message ?? {};
      if (r.isApiErrorMessage === true || m.model === "<synthetic>" || at === null || !Array.isArray(m.content)) continue;
      for (const p of m.content) {
        if (p?.type === "text") s.turns.said(p.text, at, m.id ?? null);
        else if (p?.type === "tool_use" || p?.type === "server_tool_use" || p?.type === "mcp_tool_use") s.turns.tool();
      }
    } else if (r.type === "attachment") {
      const a = r.attachment ?? {};
      if (a.type !== "queued_command" || a.commandMode !== "prompt" || a.isMeta === true || r.isMeta === true || !humanOrigin(a)) continue;
      const text = claudeCodeUserText(partsText(a.prompt));
      const t = at ?? timeOf(a.timestamp);
      if (t !== null && text) s.turns.user(text, t, a.source_uuid ? `q:${a.source_uuid}` : null);
    }
  }
  return [...sessions].map(([id, s]) => conversation("claude-code", id, s.title ?? s.summary, null, s.turns.finish())).filter(Boolean);
}

function codexUserText(raw) {
  const k = raw.indexOf("## My request for Codex:");
  const t = (k >= 0 ? raw.slice(k + "## My request for Codex:".length) : raw).trim();
  return !t || wrapped(t) || t.startsWith("# AGENTS.md instructions") ? null : t;
}

const CODEX_TOOLS = ["function_call", "custom_tool_call", "local_shell_call", "web_search_call", "tool_search_call", "image_generation_call"];

function codexResponse(turns, p, at) {
  if (p?.type === "message" && p.role === "user") {
    const words = (Array.isArray(p.content) ? p.content : []).filter((c) => c?.type === "input_text").map((c) => codexUserText(String(c.text ?? ""))).filter(Boolean);
    if (words.length) turns.user(words.join("\n\n"), at);
  } else if (p?.type === "message" && p.role === "assistant") turns.said(partsText(p.content, ["output_text"]), at);
  else if (CODEX_TOOLS.includes(p?.type)) turns.tool();
}

/// A Codex rollout, each of its formats; a subagent's is none.
function codex(records, stem) {
  const first = records[0];
  if (!first) return null;
  const meta = first.type === "session_meta" ? first.payload ?? {} : first;
  const old = first.type === "session_meta" ? false : first.type === undefined;
  if (meta.source?.subagent !== undefined || meta.thread_source?.subagent !== undefined) return null;
  const id = typeof meta.id === "string" ? meta.id : stem;
  const started = timeOf(meta.timestamp);
  const isItem = (r) => r.type === "event_msg" && r.payload?.type === "item_completed" && ["UserMessage", "AgentMessage"].includes(r.payload?.item?.type);
  const items = records.some(isItem);
  const events = records.some((r) => r.type === "event_msg" && ["user_message", "agent_message"].includes(r.payload?.type));
  const turns = new Turns();
  let tick = started ?? 0;
  for (const r of records) {
    tick++;
    const at = timeOf(r.timestamp) ?? tick;
    const p = r.payload ?? {};
    if (items) {
      if (r.type !== "event_msg" || p.type !== "item_completed" || (typeof p.thread_id === "string" && p.thread_id !== id)) continue;
      const item = p.item ?? {};
      const t = timeOf(p.completed_at_ms) ?? at;
      if (item.type === "UserMessage") {
        const text = codexUserText(partsText(item.content));
        if (text) turns.user(text, t);
      } else if (item.type === "AgentMessage") {
        if (item.phase === "final_answer" || item.phase == null) turns.said(partsText(item.content, ["Text", "text"]), t);
      } else if (item.type !== "Reasoning" && item.type !== "ContextCompaction") turns.tool();
    } else if (events) {
      if (r.type !== "event_msg") continue;
      if (p.type === "user_message") {
        const text = codexUserText(String(p.message ?? ""));
        if (text) turns.user(text, at);
      } else if (p.type === "agent_message") turns.said(p.message, at);
      else if (p.type === "task_complete") turns.saidIfNone(p.last_agent_message, at);
      else if (/_(begin|end)$/.test(String(p.type))) turns.tool();
    } else if (old) codexResponse(turns, r, at);
    else if (r.type === "response_item") codexResponse(turns, p, at);
  }
  return conversation("codex", id, null, started, turns.finish());
}

/// claude.ai's export: every conversation of the account.
function claudeExport(list) {
  const out = [];
  for (const c of Array.isArray(list) ? list : []) {
    if (typeof c?.uuid !== "string") continue;
    const turns = new Turns();
    for (const m of Array.isArray(c.chat_messages) ? c.chat_messages : []) {
      const at = timeOf(m?.created_at);
      if (at === null) continue;
      const text = Array.isArray(m.content) && m.content.length ? partsText(m.content) : String(m.text ?? "").trim();
      if (m.sender === "human") turns.user(text, at, m.uuid ?? null);
      else if (m.sender === "assistant") {
        turns.said(text, at);
        turns.flush();
      }
    }
    const conv = conversation("claude-export", c.uuid, c.name, timeOf(c.created_at), turns.finish());
    if (conv) out.push(conv);
  }
  return out;
}

const CLAUDE_CODE_TYPES = ["user", "assistant", "summary", "system", "attachment", "file-history-snapshot", "queue-operation", "custom-title", "agent-name", "last-prompt", "mode", "permission-mode"];

/// A file's conversations, by what it holds; throws for a file of no
/// format this reads.
export function parseFile(name, text) {
  const stem = name.replace(/\.(jsonl?|txt)$/i, "");
  if (/\.json$/i.test(name)) {
    const v = JSON.parse(text);
    if (Array.isArray(v) && (v.length === 0 || v.some((c) => Array.isArray(c?.chat_messages)))) return claudeExport(v);
    throw new Error("this JSON is not claude.ai's conversations.json");
  }
  const records = [];
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    try {
      records.push(JSON.parse(line));
    } catch {
      // a line cut by a crash
    }
  }
  const first = records[0] ?? {};
  if (["session_meta", "response_item", "event_msg", "turn_context"].includes(first.type) || (first.type === undefined && typeof first.id === "string" && "instructions" in first)) {
    const id = stem.startsWith("rollout-") ? stem.slice(-36) : stem;
    const c = codex(records, id);
    return c ? [c] : [];
  }
  if (typeof first.sessionId === "string" || (CLAUDE_CODE_TYPES.includes(first.type) && first.role === undefined)) return claudeCode(records, stem);
  throw new Error("this file is not a Claude Code session, a Codex rollout, or claude.ai's export");
}

/// As the CLI's `prepare`: oldest first, each message capped, a repeated
/// paste and a record copied in twice dropped.
export function prepare(convs) {
  convs.sort((a, b) => a.started - b.started || a.id.localeCompare(b.id));
  const keys = new Set();
  const pastes = new Set();
  for (const c of convs) {
    c.messages = c.messages.filter((m) => {
      if (m.key && keys.has(m.key)) return false;
      if (m.key) keys.add(m.key);
      if (m.role !== "user" || utf8(m.text) < PASTE_MIN_BYTES) return true;
      const words = m.text.split(/\s+/).filter(Boolean).join(" ");
      if (pastes.has(words)) return false;
      pastes.add(words);
      return true;
    });
    for (const m of c.messages) m.text = capText(m.text);
  }
  return convs.filter((c) => c.messages.some((m) => m.role === "user"));
}

/// A conversation's messages from `from` on, as `import` takes them.
function* parts(c, from) {
  for (let start = from; start < c.messages.length; ) {
    const batch = [];
    let bytes = 512 + utf8(c.id) + utf8(c.title ?? "");
    while (start + batch.length < c.messages.length && batch.length < PART_MESSAGES) {
      const m = c.messages[start + batch.length];
      const one = { role: m.role, text: m.text, at: m.at };
      const size = utf8(JSON.stringify(one)) + 1;
      if (batch.length && bytes + size > PART_BYTES) break;
      bytes += size;
      batch.push(one);
    }
    yield { from: start, messages: batch };
    start += batch.length;
  }
}

// in the platform's look (./__fragment.css), which the page links
const STYLE = `
.imp { display: grid; gap: 12px; justify-items: start; }
.imp > * { max-width: 100%; }
.imp-lede { margin: 0; color: var(--muted); font-size: 14px; }
.imp-pick { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
.imp-btn { display: inline-flex; align-items: center; height: 34px; padding: 0 16px; border: 1px solid var(--line-strong); border-radius: 999px; background: none; color: var(--fg); font: 600 13.5px var(--font); cursor: pointer; }
.imp-btn.primary { border-color: transparent; background: var(--fg); color: var(--bg); }
.imp-btn:disabled { opacity: .5; cursor: default; }
.imp-file { color: var(--muted); font-size: 13px; overflow-wrap: anywhere; }
.imp-facts { margin: 0; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); font-size: 14px; }
.imp-bar { justify-self: stretch; height: 6px; border-radius: 3px; background: var(--soft); overflow: hidden; }
.imp-bar > div { height: 100%; width: 0; background: var(--accent); transition: width .3s ease; }
.imp-status { margin: 0; color: var(--muted); font-size: 13.5px; }
.imp-status:empty { display: none; }
.imp-note { margin: 0; color: var(--faint); font-size: 13px; }
.imp-error { margin: 0; color: var(--danger); font-size: 14px; }
`;

/// Fills `container` with the upload: a file to pick, what it holds, and
/// the import's and then the compactor's progress.
export function mountImport(container) {
  const input = h("input", { type: "file", accept: ".json,.jsonl", hidden: true });
  const pick = h("button.imp-btn", { type: "button", text: "Choose a file", onclick: () => input.click() });
  const fileName = h("span.imp-file");
  const facts = h("p.imp-facts", { hidden: true });
  const go = h("button.imp-btn.primary", { type: "button", text: "Import", hidden: true });
  const bar = h("div.imp-bar", { hidden: true }, h("div"));
  const status = h("p.imp-status", { "aria-live": "polite" });
  const error = h("p.imp-error", { hidden: true });
  let convs = [];
  let busy = false;

  const fail = (e) => {
    error.textContent = e instanceof Error ? e.message : String(e);
    error.hidden = false;
    busy = false;
    pick.disabled = false;
    go.disabled = false;
  };
  const progress = (done, all) => {
    bar.hidden = false;
    bar.firstChild.style.width = `${all ? Math.min(100, (100 * done) / all) : 100}%`;
  };

  input.addEventListener("change", async () => {
    const file = input.files?.[0];
    if (!file || busy) return;
    error.hidden = true;
    facts.hidden = true;
    go.hidden = true;
    fileName.textContent = file.name;
    try {
      if (file.size > FILE_MAX_BYTES) throw new Error(`${file.name} is over ${FILE_MAX_BYTES / 1024 / 1024} MB: import it with \`fragment mind import\` instead`);
      status.textContent = "Reading…";
      convs = prepare(parseFile(file.name, await file.text()));
      const messages = convs.reduce((n, c) => n + c.messages.length, 0);
      const long = convs.reduce((n, c) => n + c.messages.filter((m) => utf8(m.text) + 6 > NODE).length, 0);
      status.textContent = "";
      if (!convs.length) throw new Error("this file holds no words of yours to import");
      facts.textContent =
        `${plural(convs.length, "conversation")}, ${plural(messages, "message")}: your words and the final replies, no tool calls. ` +
        `Your memory summarizes them as it does every chat, in about ${plural(Math.ceil(long / 8) + Math.ceil(messages / 8), "call")} of its cheap model.`;
      facts.hidden = false;
      go.hidden = false;
    } catch (e) {
      status.textContent = "";
      fail(e);
    }
  });

  go.addEventListener("click", async () => {
    if (busy || !convs.length) return;
    busy = true;
    pick.disabled = true;
    go.disabled = true;
    error.hidden = true;
    try {
      const landed = [];
      for (let k = 0; k < convs.length; k += ASK_MAX) {
        const ask = convs.slice(k, k + ASK_MAX).map((c) => ({ source: c.source, id: c.id }));
        landed.push(...(await F.call("imported", { conversations: ask })).landed);
      }
      const total = convs.reduce((n, c, k) => n + Math.max(0, c.messages.length - landed[k]), 0);
      let sent = 0;
      for (const [k, c] of convs.entries()) {
        for (const part of parts(c, landed[k])) {
          if (!container.isConnected) return;
          await F.call("import", {
            source: c.source,
            conversation: { id: c.id, title: c.title, started: c.started },
            from: part.from,
            total: c.messages.length,
            messages: part.messages,
          });
          sent += part.messages.length;
          progress(sent, total);
          status.textContent = `Importing: ${plural(k + 1, "conversation")} of ${convs.length.toLocaleString()}, ${plural(sent, "message")}…`;
        }
      }
      status.textContent = total ? `Imported ${plural(total, "message")}.` : "All of it was imported already.";
      go.hidden = true;
      busy = false;
      pick.disabled = false;
      follow();
    } catch (e) {
      fail(new Error(`${e instanceof Error ? e.message : e} (importing again resumes where it stopped)`));
    }
  });

  // the compactor, until every message is summarized
  async function follow() {
    while (container.isConnected) {
      let s;
      try {
        s = await F.call("status", {});
      } catch {
        s = null;
      }
      if (s) {
        progress(s.T - s.unbuilt, s.T);
        if (s.unbuilt === 0 && !s.ready) {
          status.textContent = `Your memory has summarized all ${plural(s.T, "message")}.`;
          return;
        }
        status.textContent = `Summarizing: ${(s.T - s.unbuilt).toLocaleString()} of ${plural(s.T, "message")}. You can close this; it goes on.`;
      }
      await new Promise((r) => setTimeout(r, POLL_MS));
    }
  }

  container.replaceChildren(
    h("style", { text: STYLE }),
    h(
      "div.imp",
      null,
      h("p.imp-lede", {
        text: "Bring in your chats from claude.ai (its export's conversations.json), a Claude Code session (.jsonl) or a Codex session (.jsonl). They join your memory as if said here, each its own chat, oldest first.",
      }),
      h("div.imp-pick", null, pick, fileName, input),
      facts,
      go,
      bar,
      status,
      error,
      h("p.imp-note", { text: "For whole folders, and Hermes: fragment mind import <folder> --dry-run, on your computer." }),
    ),
  );
}
