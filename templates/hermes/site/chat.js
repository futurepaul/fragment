// The chat: Hermes' chats in the sidebar, one open at a time, and a turn
// streamed over its gateway socket as it happens. Hermes keeps every chat;
// this page keeps nothing but which one is open (`#chat=<id>`).

import { Hermes, HermesError } from "./hermes.js";

const $ = (id) => document.getElementById(id);
const hermes = new Hermes();
/** A turn's ping, so the node sees a turn in flight and keeps Hermes awake
 * (Hermes' own busy signal misses a chat's turns: docs/hermes-chat.md). */
const PING_MS = 15_000;
const TITLE_MAX = 60;

let gateway = null;
/** The open chat's stored id; null for a new chat before its first message. */
let current = null;
/** Stored ids → their live handles on this socket. */
const live = new Map();
/** The turn in flight: `{handle, row, text, tools, ping}`. */
let turn = null;

function state(s, text) {
  document.body.dataset.state = s;
  $("status").textContent = text;
}

function row(role, text) {
  const div = document.createElement("div");
  div.className = `row ${role}`;
  div.textContent = text;
  $("transcript").append(div);
  div.scrollIntoView({ block: "end" });
  return div;
}

function toolRow(name) {
  const div = document.createElement("div");
  div.className = "row tool running";
  div.textContent = name;
  turn.row.before(div);
  return div;
}

async function start() {
  state("connecting", "Connecting…");
  try {
    await hermes.grant();
  } catch (e) {
    return refused(e);
  }
  state("ready", "");
  await listChats();
  const asked = new URLSearchParams(location.hash.slice(1)).get("chat");
  if (asked) await openChat(asked);
  $("text").focus();
}

function refused(e) {
  const kind = e instanceof HermesError ? e.kind : "request";
  if (kind === "signin") {
    state("signin", "Sign in to chat with this Hermes.");
    const a = Object.assign(document.createElement("a"), { href: `./__signin?return=${encodeURIComponent(location.pathname + location.hash)}`, textContent: "Sign in" });
    $("status").append(" ", a);
    return;
  }
  if (kind === "access") return state("access", "Only its owner and editors chat with this Hermes.");
  if (kind === "starting") state("starting", "Starting your Hermes…");
  else state("error", `${e.message}; trying again…`);
  setTimeout(start, kind === "starting" ? 3000 : 5000);
}

async function listChats() {
  const chats = await hermes.sessions();
  $("chats").replaceChildren(...chats.map((c) => {
    const li = document.createElement("li");
    li.dataset.id = c.id;
    li.textContent = c.title || c.preview.split("\n")[0] || "Untitled chat";
    li.classList.toggle("open", c.id === current);
    li.onclick = () => openChat(c.id);
    return li;
  }));
}

function select(id) {
  current = id;
  history.replaceState(null, "", id ? `#chat=${encodeURIComponent(id)}` : location.pathname);
  for (const li of $("chats").children) li.classList.toggle("open", li.dataset.id === id);
}

async function openChat(id) {
  if (turn) return;
  select(id);
  $("transcript").replaceChildren();
  let rows;
  try {
    rows = await hermes.messages(id);
  } catch (e) {
    return row("note", `This chat did not load: ${e.message}`);
  }
  for (const m of rows) {
    if (m.role === "user" && m.text) row("user", m.text);
    else if (m.role === "assistant") {
      for (const name of m.calls) row("tool", name);
      if (m.text) row("assistant", m.text);
    }
  }
}

function newChat() {
  if (turn) return;
  select(null);
  $("transcript").replaceChildren();
  $("text").focus();
}

/** The socket, opened (again) when it is not. */
async function socket() {
  if (gateway?.open) return gateway;
  gateway = await hermes.gateway();
  live.clear();
  const g = gateway;
  g.onEvent(heard);
  g.closed.then(() => {
    if (turn) lost();
    if (gateway === g) gateway = null;
  });
  return g;
}

async function send(text) {
  const g = await socket();
  let handle = current && live.get(current);
  if (!handle && current) {
    handle = (await g.request("session.resume", { session_id: current })).session_id;
    live.set(current, handle);
  }
  if (!handle) {
    const made = await g.request("session.create", { title: text.slice(0, TITLE_MAX) });
    handle = made.session_id;
    live.set(made.stored_session_id, handle);
    select(made.stored_session_id);
  }
  row("user", text);
  turn = { handle, row: row("assistant streaming", ""), text: "", tools: new Map(), ping: setInterval(() => g.request("gateway.ping").catch(() => {}), PING_MS) };
  document.body.dataset.turn = "on";
  await g.request("prompt.submit", { session_id: handle, text });
}

function heard(e) {
  if (!turn || e.session_id !== turn.handle) return;
  const p = e.payload ?? {};
  switch (e.type) {
    case "message.delta":
      turn.text += p.text ?? "";
      turn.row.textContent = turn.text;
      turn.row.scrollIntoView({ block: "end" });
      break;
    case "tool.start": {
      const name = p.name ?? p.tool_name ?? "a tool";
      turn.tools.set(p.tool_id ?? p.id ?? name, toolRow(p.context ? `${name}: ${p.context}` : name));
      break;
    }
    case "tool.complete": {
      const r = turn.tools.get(p.tool_id ?? p.id ?? p.name ?? p.tool_name);
      r?.classList.remove("running");
      break;
    }
    case "message.complete":
      turn.row.textContent = typeof p.text === "string" && p.text ? p.text : turn.text;
      if (p.status === "error") turn.row.classList.add("failed");
      done();
      listChats().catch(() => {});
      break;
  }
}

function done() {
  clearInterval(turn.ping);
  turn.row.classList.remove("streaming");
  for (const r of turn.tools.values()) r.classList.remove("running");
  turn = null;
  delete document.body.dataset.turn;
}

function lost() {
  turn.row.classList.add("failed");
  turn.row.textContent = `${turn.text}\n(the connection dropped: its reply will be in this chat when you open it again)`;
  done();
}

$("composer").onsubmit = async (e) => {
  e.preventDefault();
  const text = $("text").value.trim();
  if (!text || turn || document.body.dataset.state !== "ready") return;
  $("text").value = "";
  try {
    await send(text);
  } catch (err) {
    if (turn) done();
    row("note", `Not sent: ${err.message}`);
    $("text").value = text;
  }
};
$("text").onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
};
$("stop").onclick = () => turn && gateway?.request("session.interrupt", { session_id: turn.handle }).catch(() => {});
$("new").onclick = newChat;

start();
