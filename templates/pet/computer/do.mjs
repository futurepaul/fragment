// The pet's agent, run by the `do` job (app.mjs) on the pet's computer:
// goose, headless, with Cua Driver as its hands on the display pet.mjs runs
// and frames (:99), so everyone watching sees what it does, and anyone may
// click in between (its next look shows it). Its model is the platform's
// (`fragment model --serve`, on the owner's budget), through a proxy here
// that keeps each request under the platform's 2 MiB: goose sends every
// screenshot it was given, each turn, so only the newest go along. Each tool
// call is a step on `work` (`fragment post`) and marks the agent as the
// pet's driver (~/.pet/agent, which pet.mjs reads). It prints goose's last
// words and exits with goose's code.
import { execFile, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

const { TASK, RUN, ASKER = "", GOOSE_BIN, CUA_BIN, FRAGMENT_NAME: NAME } = process.env;
const CLI = process.env.FRAGMENT_BIN ?? "fragment";
const HOME = os.homedir();
const STATE = path.join(HOME, ".pet");
// the Cua Driver tools goose offers the model (it has 62, each described at length)
const TOOLS = ["get_desktop_state", "get_window_state", "list_windows", "click", "type_text", "press_key", "hotkey", "scroll"];
const TURNS = 40;
// goose's time: the job's exec stops everything at 10 minutes
const TIME_MS = 9 * 60_000;
// what a model request keeps whole: the newest screenshots and tool results
const SHOTS = 3;
const RESULTS = 3;
const RESULT_CUT = 2000;
const INSTRUCTIONS = `You use a Linux computer through the cua tools: a 1024×640 screen with a Chromium browser on it, which people watch live and may click too. Look before you act (get_desktop_state shows the whole screen; get_window_state also lists a window's elements), act with click, type_text, press_key, hotkey, and scroll, then look again to check what happened. To open an address, focus the browser's address bar (ctrl+l), type it, and press Return. When the task is done, say in a sentence or two what you did.`;

const run = promisify(execFile);
const has = (cmd) => spawnSync("sh", ["-c", 'command -v "$0"', cmd]).status === 0;
const kids = [];
process.on("exit", () => kids.forEach((k) => k.kill()));
for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => process.exit(143));

function fail(why) {
  console.error(why);
  process.exit(1);
}

// A model request, cut to fit: the newest SHOTS screenshots go along (each
// earlier one becomes a line of text), and a tool result before the newest
// RESULTS keeps its first RESULT_CUT characters.
function trim(ask) {
  let [shots, results] = [0, 0];
  for (const m of [...(ask.messages ?? [])].reverse()) {
    if (m.role === "tool" && typeof m.content === "string" && ++results > RESULTS && m.content.length > RESULT_CUT) {
      m.content = `${m.content.slice(0, RESULT_CUT)}… (cut)`;
    }
    if (!Array.isArray(m.content)) continue;
    m.content = m.content.map((part) => (part?.type !== "image_url" || ++shots <= SHOTS ? part : { type: "text", text: "(an earlier screenshot, left out)" }));
  }
  return ask;
}

// goose's model endpoint: `fragment model --serve` on a free port, with the
// proxy in front of it; the proxy's port
async function model() {
  const serve = spawn(CLI, ["model", "--serve", "--port", "0"], { stdio: ["ignore", "pipe", "inherit"] });
  kids.push(serve);
  const port = await new Promise((resolve, reject) => {
    createInterface({ input: serve.stdout }).on("line", (line) => {
      const at = line.match(/http:\/\/127\.0\.0\.1:(\d+)\/v1/);
      if (at) resolve(Number(at[1]));
    });
    serve.on("exit", (code) => reject(new Error(`fragment model --serve exited (${code})`)));
  });
  const proxy = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      let body = Buffer.concat(chunks);
      try {
        body = Buffer.from(JSON.stringify(trim(JSON.parse(body))));
      } catch {}
      const headers = { "content-type": "application/json", "content-length": body.length };
      const up = http.request({ host: "127.0.0.1", port, path: req.url, method: req.method, headers }, (answer) => {
        res.writeHead(answer.statusCode, answer.headers);
        answer.pipe(res);
      });
      up.on("error", (e) => res.writeHead(502, { "content-type": "application/json" }).end(JSON.stringify({ error: { message: e.message } })));
      up.end(body);
    });
  });
  await new Promise((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  return proxy.address().port;
}

// A tool call as a step on `work`, posted in order (an id per step, so a
// retry appends nothing), and the agent as the pet's driver
let posted = Promise.resolve();
let steps = 0;
function step(call, said) {
  const n = ++steps;
  fs.writeFileSync(path.join(STATE, "agent"), ASKER);
  const args = JSON.stringify(call.arguments ?? {});
  const body = { run: Number(RUN), kind: "step", n, tool: String(call.name).replace(/^cua__/, ""), args: args.length > 160 ? `${args.slice(0, 159)}…` : args, ...(said && { said: said.slice(-300) }) };
  const post = () => run(CLI, ["post", NAME, "work", "--body", JSON.stringify(body), "--id", `do-${RUN}-${n}`]);
  posted = posted.then(post).catch((e) => console.error(`work: ${String(e.stderr || e.message).trim().slice(0, 300)}`));
}

if (!TASK || !NAME || !GOOSE_BIN || !CUA_BIN) fail("the do job runs this: TASK, RUN, ASKER, GOOSE_BIN, CUA_BIN");
fs.mkdirSync(STATE, { recursive: true });
// it drives the pet's own display, which pet.mjs starts on its Sprite
if (has("sprite-env") && !fs.existsSync("/tmp/.X11-unix/X99")) fail("The pet's screen is not up yet (its first start installs a browser): try again in a minute.");

// goose's config for this run, in a root of its own: Cua Driver's MCP
// server (stdio: it owns its runtime on Linux, and ends with goose) and no
// other extension. JSON is YAML.
const root = path.join(STATE, "goose");
fs.mkdirSync(path.join(root, "config"), { recursive: true });
const cua = { enabled: true, type: "stdio", name: "cua", description: "the pet's screen", cmd: path.join(HOME, CUA_BIN), args: ["mcp"], envs: {}, timeout: 120, available_tools: TOOLS };
fs.writeFileSync(path.join(root, "config/config.yaml"), JSON.stringify({ extensions: { cua } }));
const bus = path.join(STATE, "bus");
const env = {
  ...process.env,
  GOOSE_PATH_ROOT: root,
  GOOSE_PROVIDER: "openai",
  // The platform calls the agents' model whatever this names. goose reads a
  // model's abilities from its catalog by this name and sends a tool's
  // screenshot on only to one that reads images, as gpt-4o does (a name it
  // does not know, such as the builder's, loses every screenshot).
  GOOSE_MODEL: "gpt-4o",
  GOOSE_MODE: "auto",
  GOOSE_MAX_TOKENS: "4096",
  GOOSE_DISABLE_KEYRING: "1",
  OPENAI_BASE_URL: `http://127.0.0.1:${await model()}/v1`,
  // Cua Driver's: the pet's display and its session bus (AT-SPI), no telemetry, no update checks
  DISPLAY: ":99",
  ...(fs.existsSync(bus) && { DBUS_SESSION_BUS_ADDRESS: `unix:path=${bus}` }),
  CUA_DRIVER_RS_TELEMETRY_ENABLED: "false",
  CUA_DRIVER_RS_UPDATE_CHECK: "false",
};

const logged = path.join(STATE, "goose.log");
const args = ["run", "--quiet", "--no-session", "--output-format", "stream-json", "--max-turns", String(TURNS), "--system", INSTRUCTIONS, "--text", TASK];
const goose = spawn(path.join(HOME, GOOSE_BIN), args, { env, stdio: ["ignore", "pipe", fs.openSync(logged, "w")] });
kids.push(goose);
goose.on("error", (e) => fail(`goose: ${e.message}`));
// goose's events (stream-json): its text since its last tool call, and each tool call
let said = "";
let error = "";
createInterface({ input: goose.stdout }).on("line", (line) => {
  let e;
  try {
    e = JSON.parse(line);
  } catch {
    return;
  }
  if (e.type === "error") error = String(e.error);
  if (e.type !== "message" || e.message?.role !== "assistant") return;
  for (const c of e.message.content ?? []) {
    if (c.type === "text") said += c.text;
    if (c.type === "toolRequest" && c.toolCall?.status === "success") {
      step(c.toolCall.value, said.trim());
      said = "";
    }
  }
});
let late = false;
const timer = setTimeout(() => ((late = true), goose.kill()), TIME_MS);
const code = await new Promise((resolve) => goose.on("close", (code) => resolve(late ? 124 : (code ?? 1))));
clearTimeout(timer);
await posted;
console.log(said.trim() || error || (late ? "It ran out of time." : "(goose said nothing)"));
if (code !== 0) console.error(fs.readFileSync(logged, "utf8").slice(-2000));
process.exit(code);
