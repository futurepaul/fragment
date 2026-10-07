// The chat template's page (docs/chat-records.md; docs/cloudflare-v1.md,
// decisions 8, 9 and 40). A chat is a fragment with two channels, you and
// your agents its members, and one job of the template's (app.mjs, served
// from the platform's release like this page) that pushes an agent's reply
// to the chat's people who are away:
//
//   - `chat`: what is said. People's messages (`{text, to?, attachments?}`,
//     posted here with an id of their own), agents' replies (`{text, turn,
//     attachments?}`), Stop (`{kind: "stop", turn}`), and prompt answers
//     (`{kind: "prompt_response", prompt, option}`, id `pr:<prompt>`).
//   - `work`: an agent's progress, which only editors (the chat's agents)
//     post: `turn.start`, `turn.step`, `turn.prompt`, `turn.prompt.closed`,
//     `turn.end`.
//
// The page follows both (`subscribe`, from near their ends), and the chat's
// drafts: a turn's draft shows after the turn's last record, and the reply
// with the same `turn` replaces it. It lays them out in time: a person's
// message, an agent's steps as one card, its approvals as cards with
// buttons (enabled only for the identity the card asks), its replies, and
// how the turn ended when that was not plainly. Names come from the
// fragment (`__members`: its agents, the lead first; `__people`: names and
// pictures); an agent's color is chosen from its identity until agents
// carry one, and the chat takes its lead's. Files go both ways as the chat's
// blobs (`fragment.blob`, read at `__blob/<sha256>`); a voice memo is one,
// recorded here (MediaRecorder) and shown as a player wherever audio is.
// "Notify me" subscribes this browser for its person, from a click
// (`fragment.push.register`), and the page's presence says whether the chat
// is on screen (`looking`), so its person is not pushed while it is.
// `@` lists the chat's agents, then (when the shell framing it is its
// owner's) the owner's other agents, which the shell hands over on asking
// (`{fragment: "agents?"}`); a message to one of those asks the shell to
// add it first (`{fragment: "add-agent"}`; the shell asks its person, in
// its own dialog), then names it in `to`.
//
// It speaks only chat records: nothing here knows which runtime an agent
// runs. The look is Skyler's (the Fragment UI handoff, 2026-10-02).

import * as fragment from "./__fragment.js";
import { svg } from "./icons.js";
import { inline, renderMarkdown } from "./markdown.js";
import "./tooltips.js";

// A message's text and files (docs/chat-records.md).
const TEXT_MAX_BYTES = 32 * 1024;
const ATTACHMENTS_MAX = 8;
const ATTACHMENT_MAX_BYTES = 25 * 1024 * 1024;
// The backlog a page reads: the chat's last messages, and enough of `work`
// for their turns (each turn writes two records or more there).
const CHAT_LAST = 400;
const WORK_LAST = 1000;
// After a message of one's own, a working line shows until the agent's
// turn starts (or this long passes: an agent may not be listening).
const PENDING_MS = 15000;
// Typing shows to others until this long after the last keystroke.
const TYPING_MS = 4000;
// A draft no word came for in this long is let go (its writer went away).
const DRAFT_STALE_MS = 120000;
// Faces shown in the "here" line; more are counted.
const HERE_FACES_MAX = 5;
// Identities one `__people` call names (the platform's limit).
const PROFILES_PER_ASK = 64;
// The members are read again at most this often (an agent added later).
const MEMBERS_AGAIN_MS = 5000;
// A timer's longest wait (setTimeout's own bound).
const TIMER_MAX_MS = 2 ** 31 - 1;
// The shell's answer to adding an agent is waited for this long: its person
// answers its own dialog first (it gives up on that after 90 s).
const ADD_WAIT_MS = 120000;
// The owner's agents a shell may hand over, at most (a computer runs 32).
const ROSTER_MAX = 64;
const HANDLE = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
const ROLES = ["public", "viewer", "editor", "owner"];
const atLeast = (role, floor) => ROLES.indexOf(role) >= ROLES.indexOf(floor);
// Skyler's agent colors; an agent's is chosen by its identity.
const AGENT_COLORS = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];
const SUGGESTIONS = ["Build something", "Explore an idea", "Make a plan"];
const SHA256 = /^[0-9a-f]{64}$/;
// The images `__blob` serves as themselves (passive media), shown inline.
const SHOWN_IMAGE = /^image\/(png|jpeg|webp|gif)$/;
// The audio it serves as itself, shown as a player (a voice memo).
const SHOWN_AUDIO = /^audio\/(webm|ogg|mp4|mpeg|wav)$/;
// A voice memo: the recorder's audio, asked for in this order (Chrome and
// Firefox record Opus, Safari AAC), at a voice's bitrate, for at most this
// long (it stops itself, and is sent), named by its type.
const MEMO_TYPES = ["audio/webm;codecs=opus", "audio/ogg;codecs=opus", "audio/mp4", "audio/webm"];
const MEMO_BITS_PER_S = 64000;
const MEMO_MAX_MS = 5 * 60 * 1000;
const MEMO_EXT = { "audio/webm": "webm", "audio/ogg": "ogg", "audio/mp4": "m4a", "audio/mpeg": "mp3", "audio/wav": "wav" };

/// A media type without its parameters (`audio/webm;codecs=opus` is `audio/webm`).
export function essence(type) {
  return String(type ?? "").split(";")[0].trim().toLowerCase();
}

/// A duration as a clock shows it: `m:ss`.
export function clock(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/// The `@name` words of a message, lowercased, in order, as an agent's
/// bridge reads them: a word is `[A-Za-z0-9_-]+` right after an `@` that
/// does not follow a word character, so an email address is no mention.
export function mentions(text) {
  return [...String(text).matchAll(/(?<![A-Za-z0-9_])@([A-Za-z0-9_-]+)/g)].map((m) => m[1].toLowerCase());
}

/// An agent's color, one of Skyler's, the same for its identity everywhere.
export function colorOf(id) {
  if (!id) return AGENT_COLORS[0];
  // FNV-1a over the id's UTF-16 units
  let h = 0x811c9dc5;
  for (let i = 0; i < id.length; i++) h = Math.imul(h ^ id.charCodeAt(i), 0x01000193) >>> 0;
  return AGENT_COLORS[h % AGENT_COLORS.length];
}

/// `text` cut to at most `max` bytes of UTF-8, at a character's edge.
export function cutBytes(text, max) {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length <= max) return text;
  let end = max;
  // back to the first byte of a character (not 10xxxxxx)
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
  return new TextDecoder().decode(bytes.subarray(0, end));
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const time = (at) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
const capital = (s) => s.charAt(0).toUpperCase() + s.slice(1);
const plural = (n, one) => `${n} ${one}${n === 1 ? "" : "s"}`;

function size(bytes) {
  if (!Number.isFinite(bytes)) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function avatar(color, extra = "") {
  const mark = el("span", `agent-avatar ${extra}`);
  mark.style.setProperty("--agent-color", color);
  mark.setAttribute("aria-hidden", "true");
  return mark;
}

/// A record's files, as docs/chat-records.md names them: at most 8, each
/// a blob of this fragment by its hash.
function attachmentsOf(body) {
  if (!Array.isArray(body.attachments)) return [];
  return body.attachments
    .filter((a) => a && typeof a === "object" && SHA256.test(a.sha256))
    .slice(0, ATTACHMENTS_MAX)
    .map((a) => ({ sha256: a.sha256, size: Number(a.size), type: typeof a.type === "string" ? a.type : "", name: typeof a.name === "string" ? a.name : "" }));
}

/// Mounts the chat in `root` (the page's body).
export function mount(root) {
  const framed = window.parent !== window;
  root.classList.add("fragment-chat");
  root.innerHTML = `
    <header class="chat-head" id="head" hidden><div class="agent-heading"><span class="marks" id="head-marks"></span><span class="label" id="head-label">Chat</span></div></header>
    <div class="scroll" id="scroll"><div class="column" id="messages"></div></div>
    <div class="composer-wrap">
      <div class="banner" id="banner" role="alert" hidden><span id="banner-text"></span><button type="button" id="banner-dismiss">Dismiss</button></div>
      <div class="here" id="here" hidden></div>
      <form class="composer" id="say">
        <div class="mentions" id="mentions" role="listbox" aria-label="Agents" hidden></div>
        <input id="attachment-picker" type="file" multiple hidden>
        <div class="attachments" id="attachments" hidden></div>
        <textarea id="text" rows="1" aria-label="Message" maxlength="${TEXT_MAX_BYTES}" disabled></textarea>
        <div class="composer-actions">
          <button class="icon-button" id="attach" type="button" title="Attach files" aria-label="Attach files" disabled>${svg("plus")}</button>
          <button class="icon-button" id="record" type="button" title="Record a voice memo" aria-label="Record a voice memo" aria-pressed="false" hidden disabled>${svg("mic")}</button>
          <button class="icon-button" id="discard" type="button" title="Discard the voice memo" aria-label="Discard the voice memo" hidden>${svg("x")}</button>
          <span class="recording" id="recording" role="status" hidden><i aria-hidden="true"></i><span id="recording-time">0:00</span></span>
          <span class="note" id="note"></span>
          <button class="icon-button" id="notify" type="button" hidden>${svg("bell")}</button>
          <button class="send stop" id="stop" type="button" title="Stop reply" aria-label="Stop reply" hidden>${svg("stop")}</button>
          <button class="send" id="send" type="submit" title="Send message (Enter)" aria-label="Send" disabled>${svg("send")}</button>
        </div>
      </form>
    </div>`;
  const $ = (id) => root.querySelector(`#${id}`);
  const input = $("text");
  const composer = $("say");
  $("head").hidden = framed;

  const state = {
    me: null, // { id, principal, role } once the socket said hello
    members: [], // the fragment's members (`__members`), the first added first
    chat: [], // messages and replies, in seq order: { seq, at, n, principal, text, turn, to, attachments }
    seen: new Set(), // the chat's seqs taken
    turns: new Map(), // turn -> its records (`turnOf`)
    drafts: new Map(), // turn -> { principal, text, at }: a reply as it streams
    answers: new Map(), // prompt -> { option, by }: a prompt_response on `chat`
    answering: new Map(), // prompt -> the option this page posted, until its card closes
    stopping: new Set(), // turns this page asked to stop
    pending: null, // { seq, at, n, target }: this page's message no turn has started on yet
    here: [], // the fragment's presence: [{ id, principal, data }]
    toggled: new Map(), // a step card's key -> open, once someone opened or closed it
    roster: [], // the owner's agents, from the shell that frames the chat: [{ identity, name, title }]
  };
  // where the shell that handed the roster is (its answers come from there)
  let rosterOrigin = null;
  // a record's arrival: ties of `at` across the two channels keep it
  let arrivals = 0;

  // ---- who: names and pictures from the fragment (`__people`), asked in
  // batches; an agent by its name, a person by username ----
  const profiles = new Map(); // principal -> its profile, or null while asked
  const asking = new Set();
  let askTimer = 0;
  function want(principal) {
    if (typeof principal !== "string" || !principal.startsWith("id:") || profiles.has(principal)) return;
    profiles.set(principal, null);
    asking.add(principal);
    if (!askTimer) askTimer = setTimeout(ask, 20);
  }
  async function ask() {
    askTimer = 0;
    const ids = [...asking];
    asking.clear();
    // bounded: one call per PROFILES_PER_ASK ids asked
    for (let i = 0; i < ids.length; i += PROFILES_PER_ASK) {
      const batch = ids.slice(i, i + PROFILES_PER_ASK);
      let found = {};
      try {
        const query = batch.map((id) => `id=${encodeURIComponent(id)}`).join("&");
        const r = await fetch(`./__people?${query}`, { credentials: "same-origin" });
        if (r.ok) found = (await r.json()).profiles ?? {};
      } catch {
        // unnamed for now: shown by what the records say
      }
      for (const id of batch) profiles.set(id, found[id] ?? {});
    }
    schedule();
  }
  const inRoster = (principal) => state.roster.find((r) => r.identity === principal) ?? null;
  function isAgent(principal) {
    const p = profiles.get(principal);
    if (p && p.kind) return p.kind === "agent";
    return state.members.some((m) => m.principal === principal && m.kind === "agent") || [...state.turns.values()].some((t) => t.agent === principal) || !!inRoster(principal);
  }
  /// How a principal shows: `{agent, name, color?, picture?, initial?}`.
  function who(principal) {
    if (typeof principal !== "string" || !principal.startsWith("id:")) return { agent: false, name: "a visitor", initial: "?" };
    want(principal);
    const p = profiles.get(principal);
    if (isAgent(principal)) {
      const r = inRoster(principal);
      const name = p?.title ?? r?.title ?? (p?.name ? capital(p.name) : r ? capital(r.name) : p?.username ? `${p.username}'s agent` : "Agent");
      return { agent: true, name, color: colorOf(principal) };
    }
    const name = p?.username ?? (p ? `id:…${principal.slice(-6)}` : "…");
    return { agent: false, name, picture: typeof p?.picture === "string" ? p.picture : null, initial: name.replace(/^id:…/, "").charAt(0).toUpperCase() || "?" };
  }
  /// The word that @mentions an agent: its name (its fragment's label).
  function handleOf(principal) {
    const p = profiles.get(principal);
    if (typeof p?.name === "string") return p.name.toLowerCase();
    return inRoster(principal)?.name ?? null;
  }
  const chatAgents = () => state.members.filter((m) => m.kind === "agent").map((m) => m.principal);
  /// Who `@` may name: the chat's agents (the lead first), then the
  /// owner's others the shell handed over, which a message adds first.
  const mentionable = () => {
    const here = chatAgents();
    return [...here, ...state.roster.map((r) => r.identity).filter((id) => !here.includes(id))];
  };
  const lead = () => chatAgents()[0] ?? [...state.turns.values()].find((t) => t.agent)?.agent ?? null;

  function face(principal, w = who(principal), agentSize = "tiny") {
    if (w.agent) return avatar(w.color, agentSize);
    const f = el("span", "face");
    if (w.picture) {
      const img = el("img");
      img.src = w.picture;
      img.alt = "";
      f.append(img);
    } else f.textContent = w.initial ?? "?";
    return f;
  }
  function byline(principal, w = who(principal)) {
    const line = el("div", "who");
    line.append(face(principal, w, "small"), w.name);
    return line;
  }

  // ---- the members: the chat's agents, its lead the first added ----
  let membersAt = 0;
  async function readMembers() {
    membersAt = Date.now();
    try {
      const r = await fetch("./__members", { credentials: "same-origin" });
      if (!r.ok) return;
      const v = await r.json();
      state.members = Array.isArray(v.members) ? v.members.filter((m) => m && typeof m.principal === "string") : [];
      for (const m of state.members) want(m.principal);
      schedule();
    } catch {
      // a visitor who may not list them: the records name who wrote
    }
  }
  function membersAgain() {
    if (Date.now() - membersAt > MEMBERS_AGAIN_MS) readMembers();
  }

  // ---- records ----
  function turnOf(id) {
    if (!state.turns.has(id)) {
      state.turns.set(id, { id, agent: null, asker: null, cause: null, startAt: null, lastAt: null, lastN: 0, steps: new Map(), prompts: new Map(), closed: new Map(), end: null });
    }
    return state.turns.get(id);
  }
  // the turn's latest record, by time then arrival: its draft shows after it
  function touch(t, at, n) {
    if (t.lastAt === null || at > t.lastAt || (at === t.lastAt && n > t.lastN)) {
      t.lastAt = at;
      t.lastN = n;
    }
  }
  const running = (t) => t.startAt !== null && !t.end;
  const expired = (p) => Number.isFinite(p.expiresAt) && Date.now() > p.expiresAt;
  // a turn waiting on a person: its open card says so, not a working line
  const waiting = (t) => [...t.prompts.values()].some((p) => !t.closed.has(p.prompt) && !expired(p));

  function onChat(record) {
    if (state.seen.has(record.seq)) return;
    state.seen.add(record.seq);
    let body = record.body;
    // a bare string is a message with that text
    if (typeof body === "string") body = { text: body };
    if (body === null || typeof body !== "object" || Array.isArray(body)) return;
    const n = ++arrivals;
    if (body.kind === "prompt_response") {
      // the first answer wins (the agent closes the card with it)
      if (typeof body.prompt === "string" && !state.answers.has(body.prompt)) state.answers.set(body.prompt, { option: body.option, by: record.principal });
      return schedule();
    }
    // Stop shows as how its turn ended; any other kind is a page's own
    if (body.kind !== undefined && body.kind !== "message") return;
    const turn = typeof body.turn === "string" ? body.turn : null;
    const to = Array.isArray(body.to) ? body.to.filter((x) => typeof x === "string") : [];
    state.chat.push({ seq: record.seq, at: record.at, n, principal: record.principal, text: typeof body.text === "string" ? body.text : "", turn, to, attachments: attachmentsOf(body) });
    if (turn) {
      const t = turnOf(turn);
      if (t.agent === null || t.agent === record.principal) touch(t, record.at, n);
      // its record replaces the draft as it stood (a later part drafts again)
      if (state.drafts.get(turn)?.principal === record.principal) state.drafts.delete(turn);
    }
    if (state.pending && record.seq === state.pending.seq) state.pending.n = n;
    want(record.principal);
    schedule();
  }

  function onWork(record) {
    const b = record.body;
    if (b === null || typeof b !== "object" || typeof b.turn !== "string") return;
    const n = ++arrivals;
    const t = turnOf(b.turn);
    t.agent ??= typeof b.agent === "string" ? b.agent : record.principal;
    touch(t, record.at, n);
    if (b.kind === "turn.start") {
      t.asker = typeof b.asker === "string" ? b.asker : null;
      t.cause = b.cause ?? null;
      t.startAt = record.at;
      if (state.pending && b.cause?.channel === "chat" && b.cause?.seq === state.pending.seq) state.pending = null;
    } else if (b.kind === "turn.step" && Number.isInteger(b.step)) {
      t.steps.set(b.step, { ...b, at: record.at, n });
    } else if (b.kind === "turn.prompt" && typeof b.prompt === "string") {
      t.prompts.set(b.prompt, { ...b, options: Array.isArray(b.options) ? b.options.filter((o) => o && typeof o.id === "string") : [], at: record.at, n });
      expiryTimer();
    } else if (b.kind === "turn.prompt.closed" && typeof b.prompt === "string") {
      t.closed.set(b.prompt, b);
      state.answering.delete(b.prompt);
    } else if (b.kind === "turn.end") {
      t.end = { outcome: b.outcome, error: typeof b.error === "string" ? b.error : "", at: record.at, n };
      state.drafts.delete(b.turn);
      state.stopping.delete(b.turn);
    }
    want(t.agent);
    // an agent this page did not know is in the chat: it was added since
    if (t.agent && !state.members.some((m) => m.principal === t.agent)) membersAgain();
    schedule();
  }

  function onDraft(d) {
    if (typeof d.turn !== "string" || typeof d.principal !== "string") return;
    if (typeof d.text !== "string") state.drafts.delete(d.turn);
    else state.drafts.set(d.turn, { principal: d.principal, text: d.text, at: Date.now() });
    want(d.principal);
    schedule();
  }

  // A card's expiry shows without a record: the page looks again then.
  let expiryAt = 0;
  let expiryHandle = 0;
  function expiryTimer() {
    const next = Math.min(...[...state.turns.values()].flatMap((t) => [...t.prompts.values()].filter((p) => !t.closed.has(p.prompt) && Number.isFinite(p.expiresAt) && p.expiresAt > Date.now()).map((p) => p.expiresAt)));
    if (!Number.isFinite(next) || next === expiryAt) return;
    clearTimeout(expiryHandle);
    expiryAt = next;
    expiryHandle = setTimeout(() => {
      expiryAt = 0;
      schedule();
      expiryTimer();
    }, Math.min(next - Date.now() + 50, TIMER_MAX_MS));
  }

  // ---- the timeline: everything in time, a turn's draft (or its working
  // line) after its last record, and a turn's consecutive steps as one card ----
  function timeline() {
    const items = [];
    for (const m of state.chat) items.push({ at: m.at, n: m.n, type: "message", m });
    for (const t of state.turns.values()) {
      for (const s of t.steps.values()) items.push({ at: s.at, n: s.n, type: "step", t, s });
      for (const p of t.prompts.values()) items.push({ at: p.at, n: p.n, type: "prompt", t, p });
      if (t.end && t.end.outcome !== "idle") items.push({ at: t.end.at, n: t.end.n, type: "end", t });
    }
    const now = Date.now();
    for (const [turn, d] of state.drafts) if (now - d.at > DRAFT_STALE_MS) state.drafts.delete(turn);
    for (const [turn, d] of state.drafts) {
      const t = state.turns.get(turn);
      items.push({ at: t?.lastAt ?? Infinity, n: (t?.lastN ?? 0) + 0.5, type: "draft", turn, d });
    }
    for (const t of state.turns.values()) {
      if (running(t) && !state.drafts.has(t.id) && !waiting(t)) items.push({ at: t.lastAt, n: t.lastN + 0.5, type: "working", t });
    }
    if (state.pending && now - state.pending.since > PENDING_MS) state.pending = null;
    if (state.pending) items.push({ at: state.pending.at, n: state.pending.n + 0.5, type: "working", t: null });
    items.sort((a, b) => a.at - b.at || a.n - b.n);
    const out = [];
    for (const it of items) {
      const prev = out[out.length - 1];
      if (it.type === "step" && prev?.type === "steps" && prev.t === it.t) prev.steps.push(it.s);
      else if (it.type === "step") out.push({ type: "steps", t: it.t, steps: [it.s] });
      else out.push(it);
    }
    return out;
  }

  // ---- rendering: one node per item, made again only when what it shows
  // changed, so a card being clicked is never swapped out under the hand ----
  const nodes = new Map(); // key -> { sig, node }
  const fresh = new Set(); // bubbles made since the last fit
  function cached(key, sig, make) {
    const c = nodes.get(key);
    if (c && c.sig === sig) return c.node;
    const node = make();
    nodes.set(key, { sig, node });
    return node;
  }

  let scheduled = false;
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      render();
    });
  }

  function itemNode(it) {
    switch (it.type) {
      case "message":
        return messageNode(it.m);
      case "steps":
        return stepsNode(it.t, it.steps);
      case "prompt":
        return promptNode(it.t, it.p);
      case "end":
        return endNode(it.t);
      case "draft":
        return draftNode(it.turn, it.d);
      default:
        return workingNode(it.t);
    }
  }

  function render() {
    const col = $("messages");
    const used = new Set();
    const shown = [];
    for (const it of timeline()) {
      const [key, node] = itemNode(it);
      used.add(key);
      shown.push(node);
    }
    if (!shown.length) {
      const [key, node] = emptyNode();
      used.add(key);
      shown.push(node);
    }
    for (const key of nodes.keys()) if (!used.has(key)) nodes.delete(key);
    // only what moved is moved
    shown.forEach((node, i) => {
      if (col.children[i] !== node) col.insertBefore(node, col.children[i] ?? null);
    });
    while (col.children.length > shown.length) col.lastElementChild.remove();
    fitUserBubbles([...fresh]);
    fresh.clear();
    renderChrome();
    renderHere();
    if (stuck) toEnd();
  }

  // The chat's color (its lead's), its name, Stop, and the placeholder.
  function renderChrome() {
    const agents = chatAgents();
    const first = lead();
    root.style.setProperty("--chat-agent-color", colorOf(first));
    const names = agents.map((a) => who(a).name);
    const title = names.length ? names.join(", ") : "Chat";
    if (document.title !== title) document.title = title;
    $("head-label").textContent = title;
    $("head-marks").replaceChildren(...(agents.length ? agents : [first]).slice(0, 3).map((a) => avatar(colorOf(a), "tiny")));
    const mine = [...state.turns.values()].filter((t) => running(t) && t.asker && t.asker === state.me?.principal).sort((a, b) => b.startAt - a.startAt)[0];
    const stop = $("stop");
    stop.hidden = !mine;
    $("send").hidden = !!mine;
    stop.disabled = !mine || state.stopping.has(mine.id);
    stop.dataset.turn = mine?.id ?? "";
    if (canPost()) {
      const firstName = first ? who(first).name : null;
      const others = mentionable().length > 1;
      input.placeholder = mine ? `Add to what ${who(mine.agent).name} is doing` : !firstName ? "Message" : others ? `Message ${firstName}, or @ someone else` : `Message ${firstName}`;
    }
    refreshSend();
  }

  function messageNode(m) {
    const w = who(m.principal);
    const mine = m.principal === state.me?.principal;
    const key = `m:${m.seq}`;
    const sig = [w.name, w.agent, mine, w.picture ?? "", w.color ?? ""].join("|");
    return [key, cached(key, sig, () => (w.agent ? agentMessage(m, w) : userMessage(m, w, mine)))];
  }

  function userMessage(m, w, mine) {
    const wrap = el("div", `msg user ${mine ? "mine" : "other"}`);
    wrap.dataset.seq = m.seq;
    if (!mine) wrap.append(byline(m.principal, w));
    if (m.attachments.length) wrap.append(attachmentsNode(m.attachments));
    if (m.text) {
      const bubble = el("div", "bubble", m.text);
      fresh.add(bubble);
      wrap.append(bubble);
    }
    wrap.append(el("div", "time", time(m.at)));
    return wrap;
  }

  function agentMessage(m, w) {
    const wrap = el("div", "msg agent");
    wrap.dataset.seq = m.seq;
    if (m.turn) wrap.dataset.turn = m.turn;
    const body = el("div", "md");
    body.append(renderMarkdown(m.text));
    wrap.append(byline(m.principal, w), body);
    if (m.attachments.length) wrap.append(attachmentsNode(m.attachments));
    const actions = el("div", "actions");
    const copy = el("button", "icon-button copy");
    copy.type = "button";
    copy.title = "Copy";
    copy.innerHTML = svg("copy");
    copy.onclick = () => copyText(m.text, copy);
    actions.append(copy, el("span", "time", time(m.at)));
    wrap.append(actions);
    return wrap;
  }

  // Files: an image the fragment serves as one shows, audio (a voice memo)
  // plays; anything else is a chip that downloads it.
  function attachmentsNode(list) {
    const files = el("div", "message-attachments");
    for (const a of list) {
      const href = `./__blob/${a.sha256}`;
      if (SHOWN_AUDIO.test(essence(a.type))) {
        const memo = el("div", "attachment-audio");
        memo.title = a.name || "a voice memo";
        memo.innerHTML = svg("mic");
        const audio = el("audio");
        audio.controls = true;
        audio.preload = "metadata";
        audio.src = href;
        audio.setAttribute("aria-label", a.name || "a voice memo");
        memo.append(audio);
        files.append(memo);
      } else if (SHOWN_IMAGE.test(a.type)) {
        const link = el("a", "attachment-image");
        link.href = href;
        link.target = "_blank";
        link.rel = "noopener";
        link.title = a.name;
        const img = el("img");
        img.src = href;
        img.alt = a.name || "an image";
        img.loading = "lazy";
        link.append(img);
        files.append(link);
      } else {
        const chip = el("a", "attachment-chip");
        chip.href = href;
        chip.download = a.name || a.sha256.slice(0, 12);
        chip.innerHTML = svg("file");
        chip.append(el("span", "attachment-name", a.name || "a file"), el("span", "attachment-size", size(a.size)));
        files.append(chip);
      }
    }
    return files;
  }

  // A turn's steps, one card: open while it works ("Working"), folded once
  // done ("Worked through N steps") unless someone opened it.
  function stepsNode(t, steps) {
    const key = `s:${t.id}:${steps[0].step}`;
    const live = running(t) && steps[steps.length - 1].n === t.lastN;
    const sig = `${steps.length}|${live}`;
    return [
      key,
      cached(key, sig, () => {
        const d = el("details", `tools${live ? " live" : ""}`);
        d.dataset.turn = t.id;
        d.open = state.toggled.has(key) ? state.toggled.get(key) : live;
        d.ontoggle = () => {
          if (d.open !== live) state.toggled.set(key, d.open);
          else state.toggled.delete(key);
        };
        const summary = el("summary");
        summary.innerHTML = svg(live ? "loader" : "wrench", live ? "spin" : "");
        summary.append(el("span", "grow", live ? `Working · ${plural(steps.length, "step")}` : `Worked through ${plural(steps.length, "step")}`));
        summary.insertAdjacentHTML("beforeend", svg("chevron", "chev"));
        d.append(summary);
        for (const s of steps) {
          if (s.text) d.append(el("div", "step-text", s.text));
          const step = el("div", `step${s.ok === false ? " error" : ""}`);
          const pre = el("pre");
          pre.append(el("span", "step-name", `${s.tool ?? "tool"}${s.args ? ` ${s.args}` : ""}\n`), s.excerpt || (s.ok === false ? "failed" : "done"));
          step.append(pre);
          d.append(step);
        }
        return d;
      }),
    ];
  }

  // An approval: the agent's question and its options, which only the
  // identity it asks may press (docs/chat-records.md, decision 42); once
  // closed, what came of it.
  function promptNode(t, p) {
    const key = `p:${t.id}:${p.prompt}`;
    const closed = t.closed.get(p.prompt) ?? null;
    const sent = state.answering.get(p.prompt) ?? state.answers.get(p.prompt)?.option ?? null;
    const gone = !closed && (expired(p) || !!t.end);
    const mayAnswer = !!state.me && p.asks === state.me.principal && !closed && !gone;
    const asks = who(p.asks);
    const by = closed?.by ? who(closed.by).name : "";
    const sig = JSON.stringify([closed?.outcome, closed?.option, by, sent, gone, mayAnswer, asks.name, who(t.agent).name]);
    return [
      key,
      cached(key, sig, () => {
        const card = el("div", `prompt${closed || gone ? " closed" : ""}`);
        card.dataset.prompt = p.prompt;
        card.dataset.turn = t.id;
        const head = el("div", "prompt-head");
        head.innerHTML = svg("shield");
        head.append(closed || gone ? `${who(t.agent).name} asked` : `${who(t.agent).name} asks`);
        card.append(head, inline(el("div", "prompt-text"), typeof p.text === "string" ? p.text : ""));
        const label = (id) => p.options.find((o) => o.id === id)?.label ?? id;
        if (closed?.outcome === "answered") card.append(outcome("answered", "check", `${label(closed.option)}${by ? ` · ${by}` : ""}`));
        else if (closed?.outcome === "expired") card.append(outcome("expired", "clock", "Expired: no answer in time"));
        else if (closed) card.append(outcome("stopped", "stopped", "Stopped"));
        else if (gone) card.append(outcome("expired", "clock", t.end ? "Closed" : "Expired: no answer in time"));
        else {
          const options = el("div", "options");
          for (const o of p.options) {
            const b = el("button", o.style === "primary" || o.style === "danger" ? o.style : "", o.label || o.id);
            b.type = "button";
            b.dataset.option = o.id;
            b.disabled = !mayAnswer || sent !== null;
            if (sent === o.id) b.classList.add("chosen");
            b.onclick = () => answer(p.prompt, o.id);
            options.append(b);
          }
          card.append(options);
          const note = !mayAnswer ? `Only ${asks.name} can answer this.` : sent !== null ? "Sent…" : "";
          if (note) card.append(el("div", "prompt-note", note));
        }
        return card;
      }),
    ];
  }
  function outcome(kind, icon, text) {
    const line = el("div", `outcome ${kind}`);
    line.innerHTML = svg(icon);
    line.append(text);
    return line;
  }

  // How a turn ended when it was not plainly (`idle`): quietly.
  function endNode(t) {
    const key = `e:${t.id}`;
    const name = who(t.agent).name;
    const sig = `${t.end.outcome}|${name}`;
    return [
      key,
      cached(key, sig, () => {
        const line = el("div", `msg notice ${t.end.outcome}`);
        line.dataset.turn = t.id;
        const said = el("span");
        if (t.end.outcome === "stopped") {
          line.innerHTML = svg("stopped");
          said.textContent = "Stopped.";
        } else if (t.end.outcome === "error") {
          line.innerHTML = svg("alert");
          said.append(`${name} could not finish`);
          if (t.end.error) said.append(": ", el("span", "why", t.end.error));
          else said.append(".");
        } else {
          line.innerHTML = svg("alert");
          said.textContent = `${name} stopped (${t.end.outcome}).`;
        }
        line.append(said);
        return line;
      }),
    ];
  }

  // A reply as it streams: its draft, the cursor at the end of its text.
  function draftNode(turn, d) {
    const key = `d:${turn}`;
    const w = who(d.principal);
    let c = nodes.get(key);
    if (!c || c.sig !== w.name) {
      const node = el("div", "msg agent streaming");
      node.dataset.turn = turn;
      node.append(byline(d.principal, w), el("div", "md"));
      c = { sig: w.name, node, text: null };
      nodes.set(key, c);
    }
    if (c.text !== d.text) {
      const md = c.node.querySelector(".md");
      md.replaceChildren(renderMarkdown(d.text));
      const last = md.lastElementChild;
      (last && last.tagName === "P" ? last : md).append(el("span", "cursor"));
      c.text = d.text;
    }
    return [key, c.node];
  }

  function workingNode(t) {
    const agent = t ? t.agent : state.pending?.target;
    const forWhom = t?.asker && t.asker !== state.me?.principal ? ` for ${who(t.asker).name}` : "";
    const said = `${agent ? who(agent).name : "The agent"} is working${forWhom}`;
    const key = `w:${t ? t.id : "pending"}`;
    return [
      key,
      cached(key, said, () => {
        const line = el("div", "msg working");
        const hint = el("span", "hint");
        const dots = el("span", "dots");
        dots.append(el("i"), el("i"), el("i"));
        hint.append(dots, said);
        line.append(hint);
        return line;
      }),
    ];
  }

  // A new chat: its lead, and a start.
  function emptyNode() {
    const agents = chatAgents();
    const names = agents.map((a) => who(a).name);
    const sig = `${names.join(",")}|${canPost()}`;
    return [
      "empty",
      cached("empty", sig, () => {
        const wrap = el("div", "empty");
        wrap.append(avatar(colorOf(agents[0]), "large"), el("h2", null, names.length ? names.join(", ") : "New chat"));
        wrap.append(el("p", null, !canPost() ? "Nothing has been said here yet." : names.length ? "A fresh start. What should we work on together?" : "No agent is in this chat yet."));
        if (canPost() && names.length) {
          const chips = el("div", "suggestions");
          for (const s of SUGGESTIONS) {
            const b = el("button", null, s);
            b.type = "button";
            b.onclick = () => send(s);
            chips.append(b);
          }
          wrap.append(chips);
        }
        return wrap;
      }),
    ];
  }

  // Who else is here (each person once, whatever their tabs), and who of
  // them is writing.
  function renderHere() {
    const others = new Map();
    for (const p of state.here) {
      if (p.principal === state.me?.principal) continue;
      const was = others.get(p.principal);
      others.set(p.principal, { typing: !!(was?.typing || p.data?.typing) });
    }
    const line = $("here");
    line.hidden = others.size === 0;
    if (!others.size) return line.replaceChildren();
    const faces = el("span", "faces");
    for (const principal of [...others.keys()].slice(0, HERE_FACES_MAX)) faces.append(face(principal));
    const names = [...others.keys()].map((p) => who(p).name);
    const typing = [...others].filter(([, v]) => v.typing).map(([p]) => who(p).name);
    const list = (xs) => (xs.length === 1 ? xs[0] : xs.length === 2 ? `${xs[0]} and ${xs[1]}` : `${xs[0]} and ${xs.length - 1} others`);
    const said = typing.length ? `${list(typing)} ${typing.length === 1 ? "is" : "are"} writing…` : `${list(names)} ${names.length === 1 ? "is" : "are"} here`;
    line.replaceChildren(faces, el("span", null, said));
  }

  // A wrapped bubble shrinks to its longest line, not the width cap.
  function fitUserBubbles(bubbles = [...$("messages").querySelectorAll(".bubble")]) {
    const live = bubbles.filter((b) => b.isConnected);
    for (const bubble of live) bubble.style.removeProperty("width");
    const widths = live.map((bubble) => {
      const range = document.createRange();
      range.selectNodeContents(bubble);
      const longest = Math.max(0, ...Array.from(range.getClientRects(), (rect) => rect.width));
      const css = getComputedStyle(bubble);
      const inset = [css.paddingLeft, css.paddingRight, css.borderLeftWidth, css.borderRightWidth].reduce((sum, v) => sum + parseFloat(v), 0);
      return Math.min(bubble.getBoundingClientRect().width, Math.ceil(longest + inset));
    });
    live.forEach((bubble, i) => {
      bubble.style.width = `${widths[i]}px`;
    });
  }
  // A column that changed width fits its bubbles again; one that grew (a
  // picture that loaded, a font) stays at its end while the reader is there.
  let columnWidth = 0;
  new ResizeObserver(([entry]) => {
    if (entry.contentRect.width !== columnWidth) {
      columnWidth = entry.contentRect.width;
      fitUserBubbles();
    }
    if (stuck) toEnd();
  }).observe($("messages"));
  document.fonts?.ready.then(() => fitUserBubbles());

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
    button.setAttribute("aria-label", "Copied");
    setTimeout(() => {
      button.innerHTML = svg("copy");
      button.setAttribute("aria-label", "Copy");
    }, 1200);
  }

  // ---- scrolling: follow the end while the reader is there. Only the
  // reader scrolling up lets go of it: content that grows (a picture that
  // loads) moves nothing back, so it never does ----
  let stuck = true;
  let lastTop = 0;
  const toEnd = () => {
    const s = $("scroll");
    s.scrollTop = s.scrollHeight;
    lastTop = s.scrollTop;
  };
  $("scroll").addEventListener("scroll", () => {
    const s = $("scroll");
    if (s.scrollHeight - s.scrollTop - s.clientHeight < 160) stuck = true;
    else if (s.scrollTop < lastTop) stuck = false;
    lastTop = s.scrollTop;
  });

  // ---- the composer's edge: a subtle glow that follows the cursor near it ----
  const hoverPointer = matchMedia("(hover: hover) and (pointer: fine)");
  let edgeFrame = 0;
  let edgePointer = null;
  function clearEdgeLight() {
    cancelAnimationFrame(edgeFrame);
    edgeFrame = 0;
    composer.style.setProperty("--edge-light", "0");
  }
  root.addEventListener("pointermove", (event) => {
    if (!hoverPointer.matches || event.pointerType === "touch") return;
    edgePointer = { x: event.clientX, y: event.clientY };
    if (edgeFrame) return;
    edgeFrame = requestAnimationFrame(() => {
      edgeFrame = 0;
      const rect = composer.getBoundingClientRect();
      const x = edgePointer.x - rect.left;
      const y = edgePointer.y - rect.top;
      const radius = parseFloat(getComputedStyle(composer).borderRadius);
      // lit throughout the input; it fades only outside its edge
      const qx = Math.abs(x - rect.width / 2) - rect.width / 2 + radius;
      const qy = Math.abs(y - rect.height / 2) - rect.height / 2 + radius;
      const distance = Math.max(0, Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - radius);
      const proximity = Math.max(0, 1 - distance / 64);
      composer.style.setProperty("--edge-radius", `${Math.max(180, rect.height)}px`);
      composer.style.setProperty("--edge-x", `${x}px`);
      composer.style.setProperty("--edge-y", `${y}px`);
      composer.style.setProperty("--edge-light", String(proximity * proximity));
    });
  });
  root.addEventListener("pointerleave", clearEdgeLight);
  addEventListener("blur", clearEdgeLight);

  // ---- the composer: one line that grows; Enter sends, Shift+Enter adds a line ----
  const canPost = () => !!state.me && atLeast(state.me.role, "viewer");
  // a file is one of the chat's blobs, which editors upload (the chat's people)
  const canAttach = () => canPost() && atLeast(state.me.role, "editor") && state.me.principal.startsWith("id:");
  let attachments = []; // { file, name, size, type, uploading }
  let sending = false;
  let recording = null; // the voice memo being recorded (`startMemo`)
  function refreshSend() {
    $("send").disabled = sending || (!input.value.trim() && !attachments.length && !recording) || !canPost();
  }
  function grow() {
    input.style.height = "auto";
    input.style.height = `${Math.min(Math.max(input.scrollHeight, 27), 220)}px`;
    refreshSend();
  }
  function renderAttachments() {
    const list = $("attachments");
    list.hidden = !attachments.length;
    list.replaceChildren(
      ...attachments.map((a) => {
        const chip = el("div", `attachment-chip${a.uploading ? " uploading" : ""}`);
        chip.innerHTML = svg(a.uploading ? "loader" : "file");
        chip.append(el("span", "attachment-name", a.name), el("span", "attachment-size", size(a.size)));
        if (!a.uploading) {
          const remove = el("button", "icon-button attachment-remove");
          remove.type = "button";
          remove.setAttribute("aria-label", `Remove ${a.name}`);
          remove.innerHTML = svg("x");
          remove.onclick = () => {
            attachments = attachments.filter((x) => x !== a);
            renderAttachments();
            grow();
          };
          chip.append(remove);
        }
        return chip;
      }),
    );
  }
  function addFiles(files) {
    if (!canAttach()) return;
    for (const file of files) {
      if (attachments.length >= ATTACHMENTS_MAX) {
        problem(`A message carries at most ${ATTACHMENTS_MAX} files.`);
        break;
      }
      if (file.size > ATTACHMENT_MAX_BYTES) {
        problem(`${file.name} is over 25 MB, the most a chat takes.`);
        continue;
      }
      attachments.push({ file, name: file.name || "a file", size: file.size, type: file.type, uploading: false });
    }
    renderAttachments();
    grow();
  }
  $("attach").onclick = () => $("attachment-picker").click();
  $("attachment-picker").onchange = (event) => {
    addFiles([...event.target.files]);
    event.target.value = "";
    input.focus();
  };
  input.addEventListener("paste", (e) => {
    const files = [...(e.clipboardData?.files ?? [])];
    if (files.length && canAttach()) {
      e.preventDefault();
      addFiles(files);
    }
  });
  composer.addEventListener("dragover", (e) => {
    if (canAttach() && e.dataTransfer?.types.includes("Files")) e.preventDefault();
  });
  composer.addEventListener("drop", (e) => {
    if (!canAttach() || !e.dataTransfer?.files.length) return;
    e.preventDefault();
    addFiles([...e.dataTransfer.files]);
  });

  // ---- a voice memo: the mic records (MediaRecorder) until it is pressed
  // again (or Send, or MEMO_MAX_MS), then the clip is sent as the message's
  // audio file, with whatever was typed; the x lets it go. A chat's files
  // are its editors' (`canAttach`), so only they record. The platform does
  // nothing with the audio: an agent's runtime hears it ----
  const recordable = () => typeof MediaRecorder !== "undefined" && !!navigator.mediaDevices?.getUserMedia;
  // the mic's face changes only with the state, so a press is never lost
  // to its button being drawn again; the clock ticks on its own
  function renderRecording() {
    const on = !!recording;
    const mic = $("record");
    if (mic.classList.contains("on") !== on) {
      mic.classList.toggle("on", on);
      mic.innerHTML = svg(on ? "stop" : "mic");
      const label = on ? "Stop and send the voice memo" : "Record a voice memo";
      mic.setAttribute("aria-label", label);
      if (mic.hasAttribute("title")) mic.title = label;
      else mic.dataset.actionTitle = label;
      mic.setAttribute("aria-pressed", String(on));
      $("discard").hidden = !on;
      $("recording").hidden = !on;
    }
    tickRecording();
    refreshSend();
  }
  function tickRecording() {
    if (recording) $("recording-time").textContent = clock(Date.now() - recording.startedAt);
  }
  async function startMemo() {
    if (recording || !canAttach() || !recordable()) return;
    if (attachments.length >= ATTACHMENTS_MAX) return problem(`A message carries at most ${ATTACHMENTS_MAX} files.`);
    const memo = { recorder: null, stream: null, chunks: [], bytes: 0, startedAt: Date.now(), timer: 0, tick: 0, stopped: false, discard: false, failed: null, tooLarge: false };
    // held while the browser asks for the microphone, so a second press waits
    recording = memo;
    renderRecording();
    try {
      memo.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const type = MEMO_TYPES.find((t) => MediaRecorder.isTypeSupported(t));
      memo.recorder = new MediaRecorder(memo.stream, { ...(type ? { mimeType: type } : {}), audioBitsPerSecond: MEMO_BITS_PER_S });
    } catch (err) {
      memo.stream?.getTracks().forEach((t) => t.stop());
      recording = null;
      renderRecording();
      return problem(err?.name === "NotAllowedError" ? "The microphone is off for this chat: allow it in your browser to record." : `Could not record: ${err?.message ?? err}`);
    }
    // pressed again while the browser asked: nothing was recorded to send
    if (memo.stopped) {
      memo.discard = true;
      return finishMemo(memo);
    }
    memo.recorder.ondataavailable = (e) => {
      if (!e.data?.size) return;
      memo.chunks.push(e.data);
      memo.bytes += e.data.size;
      if (memo.bytes > ATTACHMENT_MAX_BYTES && memo.recorder.state !== "inactive") {
        memo.tooLarge = true;
        memo.recorder.stop();
      }
    };
    memo.recorder.onerror = (e) => {
      memo.failed = e.error?.message ?? "the recorder failed";
      if (memo.recorder.state !== "inactive") memo.recorder.stop();
    };
    memo.recorder.onstop = () => finishMemo(memo);
    memo.startedAt = Date.now();
    // a piece a second, so its size is known as it grows
    memo.recorder.start(1000);
    memo.timer = setTimeout(() => stopMemo(false), MEMO_MAX_MS);
    memo.tick = setInterval(tickRecording, 500);
    renderRecording();
  }
  function stopMemo(discard) {
    const memo = recording;
    if (!memo) return;
    memo.stopped = true;
    memo.discard ||= discard;
    // still asking for the microphone: it ends once the browser answers
    if (!memo.recorder) return;
    if (memo.recorder.state !== "inactive") memo.recorder.stop();
    else finishMemo(memo);
  }
  function finishMemo(memo) {
    clearTimeout(memo.timer);
    clearInterval(memo.tick);
    memo.stream?.getTracks().forEach((t) => t.stop());
    if (recording !== memo) return;
    recording = null;
    renderRecording();
    if (memo.discard) return;
    if (memo.failed) return problem(`Your voice memo was not kept: ${memo.failed}`);
    if (memo.tooLarge) return problem("That voice memo is over 25 MB, the most a chat takes.");
    const type = essence(memo.recorder?.mimeType || memo.chunks[0]?.type);
    if (!memo.bytes || !SHOWN_AUDIO.test(type)) return problem("Nothing was recorded.");
    const file = new File(memo.chunks, `voice-memo.${MEMO_EXT[type]}`, { type });
    attachments.push({ file, name: file.name, size: file.size, type, uploading: false });
    renderAttachments();
    send(input.value);
  }
  $("record").onclick = () => (recording ? stopMemo(false) : startMemo());
  $("discard").onclick = () => {
    stopMemo(true);
    input.focus();
  };

  // @mentions: the agents whose name the word before the caret starts, the
  // chat's first, then the owner's others (a message adds them first)
  let picking = null; // { start, end, matches, index }
  function closeMentions() {
    picking = null;
    $("mentions").hidden = true;
  }
  function updateMentions() {
    const caret = input.selectionStart;
    if (caret !== input.selectionEnd) return closeMentions();
    const typed = input.value.slice(0, caret).match(/(?<![A-Za-z0-9_])@([A-Za-z0-9_-]*)$/);
    if (!typed) return closeMentions();
    const partial = typed[1].toLowerCase();
    const matches = mentionable().filter((a) => handleOf(a)?.startsWith(partial));
    if (!matches.length) return closeMentions();
    picking = { start: caret - typed[1].length - 1, end: caret, matches, index: Math.min(picking?.index ?? 0, matches.length - 1) };
    const here = chatAgents();
    $("mentions").hidden = false;
    $("mentions").replaceChildren(
      ...matches.map((a, i) => {
        const b = el("button");
        b.type = "button";
        b.setAttribute("role", "option");
        b.setAttribute("aria-selected", String(i === picking.index));
        b.dataset.agent = a;
        b.append(avatar(colorOf(a), "tiny"), who(a).name);
        if (!here.includes(a)) {
          b.classList.add("outside");
          b.append(el("span", "mention-note", "adds them to this chat"));
        }
        // before the textarea's blur
        b.onpointerdown = (e) => e.preventDefault();
        b.onclick = () => pick(a);
        return b;
      }),
    );
  }
  function pick(agent) {
    if (!picking) return;
    input.setRangeText(`@${handleOf(agent)} `, picking.start, picking.end, "end");
    closeMentions();
    grow();
    input.focus();
  }
  /// The agents a message's @mentions name: its `to` (once each is in the
  /// chat: `send` adds the owner's others first).
  function addressed(text) {
    const words = mentions(text);
    return mentionable().filter((a) => words.includes(handleOf(a)));
  }

  // ---- the owner's other agents: the shell that frames the chat hands
  // them over when its person owns it (a guest's shell, or no shell, hands
  // none, and `@` lists the chat's own), and adds one on asking ----
  const adding = new Map(); // nonce -> { resolve, reject, timer }
  function addAgent(identity) {
    return new Promise((resolve, reject) => {
      if (!framed || !rosterOrigin) return reject(new Error("only the shell adds an agent to a chat"));
      const nonce = crypto.randomUUID();
      const timer = setTimeout(() => {
        adding.delete(nonce);
        reject(new Error("the shell did not answer"));
      }, ADD_WAIT_MS);
      adding.set(nonce, { resolve, reject, timer });
      window.parent.postMessage({ fragment: "add-agent", identity, nonce }, rosterOrigin);
    });
  }
  function fromShell(event) {
    const d = event.data;
    if (d?.fragment === "agents" && Array.isArray(d.agents)) {
      rosterOrigin = event.origin;
      state.roster = d.agents
        .filter((a) => a && typeof a.identity === "string" && a.identity.startsWith("id:") && typeof a.name === "string" && HANDLE.test(a.name))
        .slice(0, ROSTER_MAX)
        .map((a) => ({ identity: a.identity, name: a.name, title: typeof a.title === "string" && a.title ? a.title : capital(a.name) }));
      schedule();
      if (picking) updateMentions();
    } else if (d?.fragment === "agent-added" && typeof d.nonce === "string" && event.origin === rosterOrigin) {
      const waiting = adding.get(d.nonce);
      if (!waiting) return;
      adding.delete(d.nonce);
      clearTimeout(waiting.timer);
      if (d.ok === true) waiting.resolve();
      else waiting.reject(new Error(typeof d.error === "string" ? d.error : "it was not added"));
    }
  }

  // this page's typing, to everyone here: on while there is text and a key
  // came lately, off after a send or TYPING_MS of quiet
  // with whether the chat is on screen (`looking`), which keeps the
  // chat's code from pushing its replies to this page's person (app.mjs)
  const onScreen = () => document.visibilityState === "visible";
  const shared = { typing: false, looking: onScreen() };
  let typingTimer = 0;
  function setTyping(on) {
    clearTimeout(typingTimer);
    if (on) typingTimer = setTimeout(() => setTyping(false), TYPING_MS);
    if (on === shared.typing) return;
    shared.typing = on;
    fragment.presence.set({ ...shared });
  }
  document.addEventListener("visibilitychange", () => {
    if (onScreen() === shared.looking) return;
    shared.looking = onScreen();
    fragment.presence.set({ ...shared });
  });
  input.addEventListener("input", () => {
    grow();
    setTyping(!!input.value.trim());
    updateMentions();
  });
  input.addEventListener("click", updateMentions);
  input.addEventListener("blur", closeMentions);
  input.addEventListener("keyup", (e) => {
    if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) updateMentions();
  });
  input.addEventListener("keydown", (e) => {
    if (picking) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        picking.index = (picking.index + (e.key === "ArrowDown" ? 1 : picking.matches.length - 1)) % picking.matches.length;
        return updateMentions();
      }
      if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
        e.preventDefault();
        return pick(picking.matches[picking.index]);
      }
      if (e.key === "Escape") return closeMentions();
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      composer.requestSubmit();
    }
  });
  composer.addEventListener("submit", (e) => {
    e.preventDefault();
    // a memo being recorded goes with the message: it stops, then sends
    if (recording) return stopMemo(false);
    send(input.value);
  });

  function problem(text) {
    $("banner-text").textContent = text;
    $("banner").hidden = false;
  }
  $("banner-dismiss").onclick = () => {
    $("banner").hidden = true;
  };

  // A message, its files uploaded first. Its id is its own: a send that
  // failed and is sent again unchanged is the same record (docs/api.md).
  let unsent = null; // { id, body }
  async function send(text) {
    if (sending || !canPost()) return;
    text = cutBytes(text.trim(), TEXT_MAX_BYTES);
    const files = attachments.slice();
    if (!text && !files.length) return;
    sending = true;
    const typed = input.value;
    input.value = "";
    grow();
    setTyping(false);
    closeMentions();
    for (const f of files) f.uploading = true;
    renderAttachments();
    try {
      const to = addressed(text);
      // an agent of the owner's not in the chat yet is added first, so its
      // bridge follows the chat and takes the message (bounded: the roster)
      const outside = to.filter((a) => !chatAgents().includes(a));
      for (const a of outside) {
        try {
          await addAgent(a);
        } catch (err) {
          const why = err.message === "declined" ? "you did not add them" : err.message;
          throw new Error(`${who(a).name} was not added to this chat (${why})`);
        }
      }
      if (outside.length) await readMembers();
      const named = [];
      // bounded: at most ATTACHMENTS_MAX files
      for (const f of files) named.push(await fragment.blob(f.file, { name: f.name }));
      const body = { text, ...(to.length ? { to } : {}), ...(named.length ? { attachments: named } : {}) };
      const id = unsent && JSON.stringify(unsent.body) === JSON.stringify(body) ? unsent.id : crypto.randomUUID();
      unsent = { id, body };
      const record = await fragment.post("chat", body, { id });
      unsent = null;
      attachments = attachments.filter((a) => !files.includes(a));
      renderAttachments();
      $("banner").hidden = true;
      expectTurn(record, to, text);
    } catch (err) {
      if (!input.value) input.value = typed;
      for (const f of files) f.uploading = false;
      renderAttachments();
      problem(`Your message was not sent: ${err.message}`);
    } finally {
      sending = false;
      grow();
    }
    input.focus();
  }

  // Someone signed in, in a chat with an agent: a turn starts on it soon
  // (the one it names, else the lead), unless that agent is busy.
  function expectTurn(record, to, text) {
    if (!record || !state.me?.principal.startsWith("id:")) return;
    const target = to[0] ?? chatAgents().find((a) => mentions(text).includes(handleOf(a))) ?? lead();
    if (!target) return;
    const turns = [...state.turns.values()];
    if (turns.some((t) => t.cause?.channel === "chat" && t.cause?.seq === record.seq)) return;
    if (turns.some((t) => t.agent === target && running(t))) return;
    const n = state.chat.find((m) => m.seq === record.seq)?.n ?? Number.MAX_SAFE_INTEGER;
    state.pending = { seq: record.seq, at: record.at, n, target, since: Date.now() };
    schedule();
    setTimeout(schedule, PENDING_MS + 50);
  }

  // Answering a card: one record, its id the card's, so a second tap is the same.
  async function answer(prompt, option) {
    state.answering.set(prompt, option);
    schedule();
    try {
      await fragment.post("chat", { kind: "prompt_response", prompt, option }, { id: `pr:${prompt}` });
    } catch (err) {
      // 409: another option was sent first, from another page: the card says which once closed
      if (err.status !== 409) {
        state.answering.delete(prompt);
        problem(`Your answer was not sent: ${err.message}`);
      }
      schedule();
    }
  }

  $("stop").onclick = async () => {
    const turn = $("stop").dataset.turn;
    if (!turn) return;
    state.stopping.add(turn);
    render();
    try {
      await fragment.post("chat", { kind: "stop", turn }, { id: `stop:${turn}` });
    } catch (err) {
      state.stopping.delete(turn);
      render();
      problem(`Could not stop it: ${err.message}`);
    }
  };

  // ---- "Notify me": this browser subscribes for its person, from a click
  // only (`fragment.push.register(<their identity>)`), and the chat's code
  // pushes an agent's reply to its people while they are away (app.mjs).
  // A browser keeps a framed chat from asking (a cross-origin frame may not
  // ask for notifications): it says so, and opens the chat in a tab ----
  const pushable = () => "serviceWorker" in navigator && typeof PushManager !== "undefined" && typeof Notification !== "undefined";
  const canNotify = () => pushable() && canPost() && state.me.principal.startsWith("id:");
  const NOTIFY = {
    off: ["bell", "Notify me of replies while I'm away"],
    working: ["loader", "Turning notifications on…"],
    on: ["bell-ring", "Notifications are on: click to turn them off"],
    denied: ["bell-off", "Notifications are blocked for this chat in your browser's settings"],
    framed: ["bell", "Open this chat in its own tab to turn on notifications"],
    failed: ["bell", "Notifications could not be turned on: click to try again"],
  };
  let notifying = "off";
  function setNotify(to) {
    notifying = to;
    const b = $("notify");
    const [icon, title] = NOTIFY[to];
    b.innerHTML = svg(icon, to === "working" ? "spin" : "");
    b.title = title;
    b.setAttribute("aria-label", title);
    b.classList.toggle("on", to === "on");
    b.dataset.state = to;
    b.disabled = to === "working" || to === "denied";
  }
  // what this browser holds already, asked without asking anything of the person
  async function readNotify() {
    $("notify").hidden = !canNotify();
    if (!canNotify()) return;
    if (Notification.permission === "denied") return setNotify(framed ? "framed" : "denied");
    try {
      const registration = await navigator.serviceWorker.getRegistration(location.href);
      const subscribed = Notification.permission === "granted" && !!(await registration?.pushManager.getSubscription());
      setNotify(subscribed ? "on" : "off");
    } catch {
      setNotify("off");
    }
  }
  $("notify").onclick = async () => {
    if (notifying === "framed") return void window.open(location.href, "_blank", "noopener");
    if (notifying === "working" || !canNotify()) return;
    const was = notifying;
    setNotify("working");
    const answer = was === "on" ? await fragment.push.unregister() : await fragment.push.register(state.me.principal);
    if (answer.ok !== false) return setNotify(was === "on" ? "off" : "on");
    if (answer.reason === "denied") return setNotify(framed ? "framed" : "denied");
    setNotify(was === "on" ? "on" : "failed");
    problem(`Notifications were not turned ${was === "on" ? "off" : "on"}: ${answer.error ?? answer.reason}`);
  };

  // The shell that frames the chat may say light or dark, hand over its
  // person's agents, and answer an add. Only its frame listens, and only to
  // its parent.
  addEventListener("message", (event) => {
    if (!framed || event.source !== window.parent) return;
    const d = event.data;
    if (d && d.fragment === "theme" && (d.mode === "light" || d.mode === "dark")) document.documentElement.dataset.theme = d.mode;
    else fromShell(event);
  });
  // asks once: a shell that is its owner's answers (and again on a change)
  if (framed) window.parent.postMessage({ fragment: "agents?" }, "*");

  // ---- who this page is, then the channels it may read, and who is here ----
  fragment.subscribe("chat", onChat, { last: CHAT_LAST, onDraft });
  readMembers();
  fragment.presence.set({ ...shared });
  fragment.presence.on((list) => {
    state.here = list;
    schedule();
  });
  fragment.me().then((hello) => {
    state.me = hello;
    // mine and theirs, and who may answer a card, are known now
    nodes.clear();
    if (atLeast(hello.role, "viewer")) fragment.subscribe("work", onWork, { last: WORK_LAST });
    const note = $("note");
    note.replaceChildren();
    if (!canPost()) {
      note.textContent = "You can read this chat.";
      input.placeholder = "Only people in this chat can write here";
    } else {
      input.disabled = false;
      if (hello.principal.startsWith("anon:")) {
        const a = el("a", null, "Sign in");
        a.href = "./__signin?return=/";
        note.append(a, " so an agent answers you.");
      }
    }
    $("attach").disabled = !canAttach();
    $("record").hidden = !recordable();
    $("record").disabled = !canAttach();
    readNotify();
    composer.dataset.ready = "1";
    render();
    grow();
  });
  fragment.closed(({ code }) => {
    problem(code === 4004 ? "This chat was deleted." : "Your access to this chat changed: reload the page.");
    input.disabled = true;
    $("attach").disabled = true;
    stopMemo(true);
    $("record").disabled = true;
  });
  render();
}
