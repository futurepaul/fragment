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
// goose's history, and the model's cached prefix, carry over, with the tools
// goose's config names now (`retool`; a task after they changed is told so
// first). Each tool call
// goose makes is a step on the chat's `work` channel, posted as this
// computer once the call ended, under the hand-off's turn
// (fragment_core::work::handoff_turn), and goose's answer, once it ended its
// turn, is this computer's post on the chat's own channel, `{text, turn}`
// (the asking agent reads it from the run, never from the chat). It prints
// goose's last words, and exits 0 when goose ended its turn and a chat that
// asked has its answer (124 past its time). Before and after the task it
// syncs the owner's memory (`synced`, below).
//
// Its env: PROMPT, the task as goose reads it; CHAT, the asking chat
// (<fragment>/<channel>; none: this fragment's own `work`); RUN, the job's
// run; ASKER, who asked; MARK, a file ASKER is written to at each step (the
// pet's driver); TIME_S, its time (540 s).
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
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
// the answer, at most a chat message's most (cell/chat.mjs)
const ANSWER_MAX = 16000;
// a post that fails is tried again after these (the hand-off's grant may come late)
const RETRY_S = [2, 4, 8, 16, 30];
// what a known extension's tools are for, said to the model when they change
const HINTS = {
  browser: "They drive Chrome through the page itself: use them for anything on a web page.",
  cua: "They are the desktop tools (Cua): use them only for native apps.",
};

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
// a line of hands.log (the service's), about this chat's session, where a
// computer's owner sees it; a failure also on stderr
function note(what, failed = false) {
  const line = `${CHAT} session ${session}: ${what}`;
  if (failed) console.error(line);
  try {
    fs.appendFileSync(path.join(DIR, "hands.log"), `[task] ${new Date().toISOString().slice(0, 19)}Z ${line}\n`);
  } catch {}
}

if (!PROMPT || !NAME) fail("a job runs this, with PROMPT (and CHAT, RUN, ASKER)");
if (!/^[a-z0-9-]+\.[a-z0-9-]+\/[a-z][a-z0-9_-]*$/.test(CHAT)) fail(`CHAT is <fragment>/<channel>, not ${CHAT}`);
const [where, channel] = CHAT.split("/");
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
    post("work", body, `st:${NAME}:${RUN}:${call.n}`);
  }
}

// each post in order, once (its id), as this computer: whether it is in
let posted = Promise.resolve(true);
function post(on, body, id) {
  const once = () => run(CLI, ["post", where, on, "--body", JSON.stringify(body), "--id", id]);
  posted = posted.then(async () => {
    for (const wait of [...RETRY_S, null]) {
      try {
        await once();
        return true;
      } catch (e) {
        if (wait === null) {
          console.error(`${id} not posted: ${String(e.stderr || e.message).trim().slice(0, 300)}`);
          return false;
        }
        await sleep(wait * 1000);
      }
    }
  });
}

// The owner's memory (docs/agent-computer.md, slice 3): their private
// fragment the platform records (its name in ./memory, written at each sync;
// none: no memory), kept at ~/memories/<name>, which ~/memory links to (its
// owner may name another: `fragment memory use`), synced both ways before and
// after each task (this computer is an editor there). goose writes it with its own tools, as
// its hints say (fragment_core::computer HANDS_SH), and what it changed is
// committed to main as this computer when the task ends: git history is the
// review and the undo. Its skills (skills/<name>/SKILL.md) are goose's own,
// at ~/.agents/skills, which goose lists for the model. Its facts
// (memory/*.md) reach the session as prompts, so nothing the session holds is
// rewritten: every one in the session's first task, then, at the start of a
// task, those changed since (what the session was told is kept beside its
// id), at most MEMORY_MAX characters, the rest named.
const MEMORY = read(path.join(DIR, "memory"));
const MEMORY_DIR = path.join(os.homedir(), "memory");
const MEMORY_MAX = 8000;
// ~/memory, the recorded memory's folder: a link, moved when it changes (a
// folder of before, slice 3's, is kept under the name it synced)
function placed() {
  const [home, dir] = [path.join(os.homedir(), "memories"), path.join(os.homedir(), "memories", MEMORY)];
  const at = fs.lstatSync(MEMORY_DIR, { throwIfNoEntry: false });
  if (at && !at.isSymbolicLink()) {
    const was = JSON.parse(read(path.join(MEMORY_DIR, ".fragment/state.json")) || "{}").name || `before-${Date.now()}`;
    fs.mkdirSync(home, { recursive: true });
    fs.renameSync(MEMORY_DIR, path.join(home, was));
  }
  fs.mkdirSync(dir, { recursive: true });
  if (at?.isSymbolicLink() && fs.readlinkSync(MEMORY_DIR) === dir) return dir;
  if (at?.isSymbolicLink()) fs.unlinkSync(MEMORY_DIR);
  fs.symlinkSync(dir, MEMORY_DIR);
  return dir;
}
async function synced() {
  if (!MEMORY) return;
  let dir;
  try {
    dir = placed();
  } catch (e) {
    return note(`${MEMORY} has no folder: ${e.message}`, true);
  }
  const out = await run(CLI, ["sync", MEMORY, "--dir", dir, "--apply-mass-delete", "--json"]).then((r) => r.stdout, (e) => String(e.stdout || e.message));
  let said = {};
  try {
    said = JSON.parse(out.trim().split("\n").pop());
  } catch {}
  if (!said.ok && !["not_found", "forbidden"].includes(said.error?.code)) note(`${MEMORY} was not synced: ${cut(String(said.error?.message ?? out).trim(), 300)}`, true);
}
function recall() {
  if (!MEMORY) return { files: {}, note: "" };
  const dir = path.join(MEMORY_DIR, "memory");
  const facts = (fs.existsSync(dir) ? fs.readdirSync(dir) : []).filter((f) => f.endsWith(".md")).sort().map((f) => [`memory/${f}`, read(path.join(dir, f))]);
  const files = Object.fromEntries(facts.map(([f, text]) => [f, createHash("sha256").update(text).digest("hex")]));
  let was = null;
  try {
    const told = JSON.parse(fs.readFileSync(`${saved}.memory`, "utf8"));
    if (told.session === session) was = told.files;
  } catch {}
  const [shown, unshown] = [[], []];
  let left = MEMORY_MAX;
  for (const [f, text] of facts.filter(([f]) => files[f] !== was?.[f])) {
    if (left < 200) unshown.push(f);
    else left -= shown[shown.push(`### ${f}\n${cut(text, left)}`) - 1].length;
  }
  const said = [...shown, ...Object.keys(was ?? {}).filter((f) => !(f in files)).map((f) => `${f} was removed.`)];
  if (unshown.length) said.push(`Not shown here (read them when they matter): ${unshown.join(", ")}.`);
  const head = was ? "Your owner's memory changed since your earlier work here:" : `Your owner's memory (${MEMORY}, at ~/memory):`;
  return { files, note: said.length ? [head, ...said].join("\n\n") : "" };
}

// the chat's session: loaded, or made the first time (or when goose lost it)
await ask("initialize", { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: "fragment", version: "1" } });
fs.mkdirSync(cwd, { recursive: true });
let [session, news] = [read(saved), ""];
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
} else news = await retool().catch((e) => note(`its tools were left as they were: ${e.message}`, true));
// the memory, synced, its skills goose's, and what the session is yet to be told of it
await synced();
const skills = path.join(os.homedir(), ".agents/skills");
try {
  if (MEMORY) fs.mkdirSync(path.join(MEMORY_DIR, "skills"), { recursive: true });
  if (MEMORY && !fs.lstatSync(skills, { throwIfNoEntry: false })) fs.mkdirSync(path.dirname(skills), { recursive: true }), fs.symlinkSync(path.join(MEMORY_DIR, "skills"), skills);
} catch (e) {
  note(`its skills are not linked: ${e.message}`, true);
}
const memory = recall();

// goose keeps the extensions a session was made with (or those that loaded
// last time) and loads it with them, not with its config's: a session made
// before the hands' tools changed (~/.config/goose/config.yaml) would keep
// the old ones. So a loaded session's MCP extensions become the config's
// enabled ones, through goose's own ACP methods (its built-in ones stay). One
// is added (goose restarts a same-named one on the new definition) when the
// session's copy is not the config's whole definition as goose says it
// (command, args, tool allowlist, timeout, description), or when goose's
// tool list, what the model is sent, has none of its tools or one its
// allowlist leaves out; one no longer configured is removed. A change breaks
// the cached prefix once, none otherwise, and is a line of hands.log, with
// whether goose then offers the tools as configured. goose says no
// extension's `envs` and adds none that way, so a config puts them in its
// command. What it answers is a note for this task's prompt, none without a
// change: the session's history shows the tools of before, which a model
// otherwise goes on using, so it says what changed, from what goose now
// offers, with a known extension's hint.
async function retool() {
  const on = (method, params) => ask(`_goose/unstable/${method}`, { sessionId: session, ...params });
  const key = (v) => JSON.stringify(v, (_, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x));
  const mcp = (e) => e.extension?.type === "mcp";
  const want = (await ask("_goose/unstable/config/extensions/list", {})).extensions.filter((e) => e.enabled && mcp(e));
  const have = (await on("session/extensions/list")).extensions.filter(mcp);
  const offered = async () => (await on("tools/list")).tools.map((t) => t.name);
  // goose offers an extension's tools as <its key>__<tool>: some, and only those its allowlist names
  const own = (tools, k) => tools.filter((t) => t.startsWith(`${k}__`)).map((t) => t.slice(k.length + 2));
  const fits = (tools, { configKey: k, extension: { available_tools: only } }) => own(tools, k).length > 0 && own(tools, k).every((t) => !only || only.includes(t));
  const [tools, changed] = [await offered(), []];
  const change = (method, params, name, done) => on(`session/extensions/${method}`, params).then(() => changed.push([name, done]), (e) => note(`${method} ${name} failed: ${e.message}`, true));
  for (const gone of have.filter((h) => !want.some((w) => w.configKey === h.extensionKey))) await change("remove", { extensionKey: gone.extensionKey }, gone.extensionKey, "removed");
  for (const w of want) {
    const h = have.find((h) => h.extensionKey === w.configKey);
    if (!h || key(h.extension) !== key(w.extension) || !fits(tools, w)) await change("add", { extension: w.extension }, w.configKey, h ? "replaced" : "added");
  }
  if (!changed.length) return "";
  const now = await offered();
  const off = want.filter((w) => !fits(now, w)).map((w) => w.configKey);
  note(`${changed.map((c) => c.join(" ")).join(", ")}; goose offers ${off.length ? `not the configured tools of ${off.join(", ")}` : "the configured tools"}`);
  const lines = changed.map(([k, done]) => {
    const list = own(now, k).join(", ");
    if (!list) return `The ${k} tools are gone.`;
    return [done === "added" ? `New ${k} tools: ${list}.` : `The ${k} tools are now: ${list}.`, HINTS[k]].filter(Boolean).join(" ");
  });
  return ["Your tools changed since your earlier work here.", ...lines].join(" ");
}

let late = false;
const timer = setTimeout(() => ((late = true), send({ method: "session/cancel", params: { sessionId: session } })), TIME_MS);
const ended = await ask("session/prompt", { sessionId: session, prompt: [{ type: "text", text: [news, memory.note, PROMPT].filter(Boolean).join("\n\n") }] }).catch((e) => ({ error: e.message }));
clearTimeout(timer);
ws.close();
// goose took the prompt: what it was told of the memory is kept for the next task
if (ended.stopReason) fs.writeFileSync(`${saved}.memory`, JSON.stringify({ session, files: memory.files }));
const words = said.trim() || last;
const done = !late && ended.stopReason === "end_turn";
// once goose ended its turn, its answer, in the chat that asked (none: this fragment's own work)
if (done && process.env.CHAT) post(channel, { text: cut(words || "(goose said nothing)", ANSWER_MAX), turn }, `an:${NAME}:${RUN}`);
const told = (await posted) || !process.env.CHAT;
// what goose remembered, committed (and what other chats and computers did, pulled)
await synced();
console.log([words, ended.error].filter(Boolean).join("\n\n") || (late ? "It ran out of time." : "(goose said nothing)"));
if (ended.error) console.error(ended.error);
process.exit(late ? 124 : done && told ? 0 : 1);
