// The hands' task client (docs/agent-computer.md): one task for this
// computer's goose, in the session of the chat that asked. The platform
// writes it to ~/.fragment/agent/task.mjs at each sync, beside hands.sh, the
// service that runs `goose serve` (ACP over a WebSocket, on the port it
// records, with the secret it made) on `fragment model --serve`
// (fragment_core::computer::HANDS). A job runs it through
// `job.computer.exec`: the pet's `do`, the builder's `build`.
//
// Each chat has one goose session, its id kept in sessions/<chat>: the first
// task makes it (session/new), each later one loads it (session/load), so
// goose's history, and the model's cached prefix, carry over. Each tool call
// goose makes is a step on the chat's `work` channel, posted as this
// computer once the call ended, under the hand-off's turn
// (fragment_core::work::handoff_turn); the answer is the job's to say. It
// prints goose's last words, and exits 0 when goose ended its turn (124 past
// its time).
//
// Its env: PROMPT, the task as goose reads it; CHAT, the asking chat
// (<fragment>/<channel>; none: this fragment's own `work`); RUN, the job's
// run; ASKER, who asked; MARK, a file ASKER is written to at each step (the
// pet's driver); TIME_S, its time (540 s).
import { execFile } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const { PROMPT, RUN = "0", ASKER = "", MARK, FRAGMENT_NAME: NAME } = process.env;
const CHAT = process.env.CHAT || `${NAME}/work`;
const CLI = process.env.FRAGMENT_BIN ?? "fragment";
const TIME_MS = Number(process.env.TIME_S ?? 540) * 1000;
const DIR = path.join(os.homedir(), ".fragment/agent");
// how long goose serve may take to come up (its first start fetches goose)
const UP_MS = 180_000;
// a step's arguments and excerpts, as the chat's records keep them (fragment_core::work)
const ARGS_MAX = 140;
const EXCERPT_MAX = 300;
// a post that fails is tried again after these (the hand-off's grant may come late)
const RETRY_S = [2, 4, 8, 16, 30];

const run = promisify(execFile);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cut = (s, n) => (s.length > n ? `${s.slice(0, n - 1)}…` : s);
const read = (f) => {
  try {
    return fs.readFileSync(f, "utf8").trim();
  } catch {
    return "";
  }
};
function fail(why, code = 1) {
  console.error(why);
  process.exit(code);
}

if (!PROMPT || !NAME) fail("a job runs this, with PROMPT (and CHAT, RUN, ASKER)");
if (!/^[a-z0-9-]+\.[a-z0-9-]+\/[a-z][a-z0-9_-]*$/.test(CHAT)) fail(`CHAT is <fragment>/<channel>, not ${CHAT}`);
const [where] = CHAT.split("/");
const turn = `hand-off:${NAME}:${RUN}`;
const saved = path.join(DIR, "sessions", CHAT);
const cwd = path.join(os.homedir(), "chats", CHAT);

// goose serve, once hands.sh says where: its port and secret
async function connect() {
  const t0 = Date.now();
  for (;;) {
    const [port, secret] = [read(path.join(DIR, "port")), read(path.join(DIR, "secret"))];
    const ws = port && secret && (await open(`ws://127.0.0.1:${port}/acp?token=${encodeURIComponent(secret)}`).catch(() => null));
    if (ws) return ws;
    if (Date.now() - t0 > UP_MS) fail(`goose serve is not up on this computer: ${read(path.join(DIR, "hands.log")).slice(-600)}`);
    await sleep(1000);
  }
}
function open(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.onopen = () => resolve(ws);
    ws.onerror = () => reject(new Error("no answer"));
  });
}

const ws = await connect();
// JSON-RPC over the socket: our asks, goose's updates, and goose's asks
let ids = 0;
const waiting = new Map();
const send = (m) => ws.send(JSON.stringify({ jsonrpc: "2.0", ...m }));
function ask(method, params) {
  const id = ++ids;
  send({ id, method, params });
  return new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
}
ws.onmessage = ({ data }) => {
  const m = JSON.parse(data);
  if (m.method === "session/update") return updated(m.params.update);
  if (m.method !== undefined) {
    // goose asks: a permission (none in auto mode) is allowed, anything else refused
    const allow = m.params?.options?.find((o) => String(o.kind).startsWith("allow"));
    if (m.id === undefined) return;
    return send(allow ? { id: m.id, result: { outcome: { outcome: "selected", optionId: allow.optionId } } } : { id: m.id, error: { code: -32601, message: `${m.method} is not offered` } });
  }
  const w = waiting.get(m.id);
  waiting.delete(m.id);
  if (m.error) w?.reject(Object.assign(new Error(m.error.message), { code: m.error.code }));
  else w?.resolve(m.result);
};
// its service restarted (a CLI updated, its script changed) or goose died: said, not retried
ws.onclose = () => waiting.forEach((w) => w.reject(new Error("goose serve closed the connection (its service restarted, or goose stopped): the task did not finish")));

// goose's updates while the task runs: its words, and each tool call, a step once it ended
let [loading, said, last, steps] = [false, "", "", 0];
const calls = new Map();
function updated(u) {
  if (loading) return;
  if (u.sessionUpdate === "agent_message_chunk" && u.content?.type === "text") said += u.content.text;
  if (u.sessionUpdate === "tool_call") {
    const tool = String(u._meta?.goose?.toolCall?.toolName ?? u.title ?? "tool").replace(/^.*?__/, "");
    calls.set(u.toolCallId, { n: ++steps, tool, args: cut(JSON.stringify(u.rawInput ?? {}), ARGS_MAX), text: said.trim() });
    last = said.trim() || last;
    said = "";
    if (MARK) fs.writeFileSync(MARK, ASKER);
  }
  const call = calls.get(u.toolCallId);
  if (u.sessionUpdate === "tool_call_update" && call && ["completed", "failed"].includes(u.status)) {
    calls.delete(u.toolCallId);
    const out = (u.content ?? []).map((c) => (c.content?.type === "text" ? c.content.text : c.content?.type ? `(${c.content.type})` : "")).join(" ");
    const body = { kind: "turn.step", turn, run: Number(RUN), step: call.n, tool: call.tool, args: call.args, ok: u.status === "completed", excerpt: cut(out.trim(), EXCERPT_MAX) };
    if (call.text) body.text = cut(call.text, EXCERPT_MAX);
    post(body, call.n);
  }
}

// each step on `work`, in order, once (its id), as this computer
let posted = Promise.resolve();
function post(body, n) {
  const once = () => run(CLI, ["post", where, "work", "--body", JSON.stringify(body), "--id", `st:${NAME}:${RUN}:${n}`]);
  posted = posted.then(async () => {
    for (const wait of [...RETRY_S, null]) {
      try {
        return await once();
      } catch (e) {
        if (wait === null) return console.error(`step ${n} not posted: ${String(e.stderr || e.message).trim().slice(0, 300)}`);
        await sleep(wait * 1000);
      }
    }
  });
}

// the chat's session: loaded, or made the first time (or when goose lost it)
await ask("initialize", { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: "fragment", version: "1" } });
fs.mkdirSync(cwd, { recursive: true });
let session = read(saved);
if (session) {
  loading = true;
  await ask("session/load", { sessionId: session, cwd, mcpServers: [], _meta: { replayTail: 1 } }).catch((e) => {
    if (e.code !== -32002) fail(`loading session ${session}: ${e.message}`);
    console.error(`session ${session} is gone: a new one`);
    session = "";
  });
  loading = false;
}
if (!session) {
  session = (await ask("session/new", { cwd, mcpServers: [] })).sessionId;
  fs.mkdirSync(path.dirname(saved), { recursive: true });
  fs.writeFileSync(saved, session);
}

let late = false;
const timer = setTimeout(() => ((late = true), send({ method: "session/cancel", params: { sessionId: session } })), TIME_MS);
const ended = await ask("session/prompt", { sessionId: session, prompt: [{ type: "text", text: PROMPT }] }).catch((e) => ({ error: e.message }));
clearTimeout(timer);
ws.close();
await posted;
const words = said.trim() || last;
console.log([words, ended.error].filter(Boolean).join("\n\n") || (late ? "It ran out of time." : "(goose said nothing)"));
if (ended.error) console.error(ended.error);
process.exit(late ? 124 : ended.stopReason === "end_turn" ? 0 : 1);
