// The pet's browser, for its hands (docs/agent-computer.md, Tools): a stdio
// MCP server goose runs as its `browser` extension (the pet's `do` installs
// it with Stagehand, from this directory's lockfile, and names it in goose's
// config). Stagehand, in local mode, drives the Chrome on the pet's screen
// through each page's structure: at the first call it attaches over CDP (on
// loopback: computer/pet.mjs starts Chrome so) and loads its runtime, an
// extension, there.
//
// act, observe, and extract reason with a model through the hands' own
// endpoint (goose's OPENROUTER_HOST: `fragment model --serve`, signed as this
// computer, billed to its owner): flashx, or Jev first with flashx when
// Jev's answer does not fit Stagehand's JSON schema (answer.mjs reads it).
// Each of those calls is a line of ~/.fragment/agent/browser.log. None sends
// the model a picture; the `screenshot` tool is the one that shows one.
// JSON-RPC by hand, as the task client speaks ACP.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { localBrowser, Stagehand } from "@browserbasehq/stagehand";
import { answer } from "./answer.mjs";

const CDP = "http://127.0.0.1:9222";
// The models Stagehand's calls go to, in order: flashx alone, or, with
// JEV_FIRST, Jev and then flashx. flashx alone until the pet's browser.log
// shows Jev first fits as often and answers sooner (docs/agent-computer.md,
// Browser fixes).
const JEV_FIRST = false;
const [JEV, FLASHX] = ["typesafe/jev-router", "z-ai/glm-5.3-flashx"];
const MODELS = JEV_FIRST ? [JEV, FLASHX] : [FLASHX];
// What each is asked beside Stagehand's messages, with the schema said in
// the system prompt so a model that ignores the response format still knows
// the shape. Jev is a router: it picks a model (stealth ones too, which Paul
// accepted) and that model's reasoning effort (it refused the one we named),
// and is asked for the schema. flashx's one provider (Z.AI) takes JSON mode
// but no schema (it lists `response_format`, not `structured_outputs`, and
// asked for a schema it answered in markdown): only a provider that takes
// JSON mode, the soonest (the platform's default sort, which naming a
// provider replaces).
const ASKS = {
  [JEV]: (name, schema) => ({ response_format: { type: "json_schema", json_schema: { name, schema } } }),
  [FLASHX]: () => ({ response_format: { type: "json_object" }, reasoning: { effort: "low" }, provider: { sort: "latency", require_parameters: true } }),
};
const LOG = path.join(os.homedir(), ".fragment/agent/browser.log");
// the most a tool answers, in characters
const ANSWER_MAX = 8000;

const said = (s) => ({ type: "text", text: String(s).slice(0, ANSWER_MAX) });
const TOOLS = {
  open: {
    description: "Open a web address in the browser on this computer's screen. Answers the page's title.",
    input: { url: { type: "string" } },
    run: async ({ url }) => {
      const page = await active();
      await page.goto(url);
      return said(`${await page.title()} (${await page.url()})`);
    },
  },
  act: {
    description: 'Do one thing on the open page, said plainly: "click Sign in", "type milk into the search box", "press Enter".',
    input: { action: { type: "string" } },
    run: async ({ action }) => {
      const { data } = await (await hands()).act(action);
      return said(`${data.success ? "Done" : "Not done"}: ${data.message}`);
    },
  },
  observe: {
    description: "List what can be done on the open page, or only what matches what you name.",
    input: { about: { type: "string" } },
    run: async ({ about }) => said((await (await hands()).observe(about)).data.map((a) => `- ${a.description}`).join("\n") || "Nothing."),
  },
  extract: {
    description: 'Read something off the open page, said plainly: "the price of the first result".',
    input: { what: { type: "string" } },
    run: async ({ what }) => said((await (await hands()).extract(what)).data.extraction),
  },
  screenshot: {
    description: "A picture of the open page, for when its look matters: the other tools read its structure.",
    input: {},
    run: async () => ({ type: "image", data: Buffer.from(await (await active()).screenshot({ type: "jpeg", quality: 60 })).toString("base64"), mimeType: "image/jpeg" }),
  },
};

// Stagehand on the pet's Chrome, once, at the first call that needs it
let stagehand;
function hands() {
  // its traces go nowhere (by default they go to example.com)
  const telemetry = { traces: { endpoint: "http://127.0.0.1:9/v1/traces" } };
  stagehand ??= localBrowser
    .connect({ cdpUrl: CDP })
    .then((browser) => Stagehand.create({ browser, model: { generate }, logging: { level: "error" }, telemetry }))
    .catch((e) => {
      stagehand = undefined;
      throw new Error(`the browser is not up (${e.message})`);
    });
  return stagehand;
}
async function active() {
  const page = await (await hands()).browser.context.activePage();
  if (!page) throw new Error("the browser has no page open");
  return page;
}

// Stagehand's model call: its messages as text (it sends a picture only when
// asked to, and nothing here asks), answered in its JSON schema
async function generate({ messages, systemPrompt, temperature, responseFormat: { name, schema } }) {
  const text = (c) => [c].flat().map((b) => (b.type === "text" ? b.text : "")).join("\n");
  const system = [systemPrompt, `Answer with one JSON object, and nothing else, in this JSON schema:\n${JSON.stringify(schema)}`].filter(Boolean).join("\n\n");
  const body = {
    messages: [{ role: "system", content: system }, ...messages.map((m) => ({ role: m.role, content: text(m.content) }))],
    ...(temperature === undefined ? {} : { temperature }),
  };
  let why = "";
  for (const model of MODELS) {
    const t0 = Date.now();
    let by;
    try {
      const r = await fetch(`${process.env.OPENROUTER_HOST}/v1/chat/completions`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ...body, ...ASKS[model](name, schema), model }) });
      if (!r.ok) throw new Error(`${r.status}: ${(await r.text()).slice(0, 200)}`);
      const said = await r.json();
      // the model that answered: a router's pick
      by = said.model;
      const structuredContent = answer(said.choices?.[0]?.message?.content, schema);
      note({ model, by, schema: name, ms: Date.now() - t0, ok: true });
      return { role: "assistant", content: { type: "text", text: JSON.stringify(structuredContent) }, outputFormat: "json_schema", structuredContent };
    } catch (e) {
      why = e.message;
      note({ model, by, schema: name, ms: Date.now() - t0, ok: false, why: why.slice(0, 300) });
    }
  }
  throw new Error(`no model answered in Stagehand's schema: ${why}`);
}

function note(call) {
  try {
    fs.appendFileSync(LOG, `${JSON.stringify({ at: new Date().toISOString(), ...call })}\n`);
  } catch {}
}
// a log past 1 MiB keeps its last 512 KiB
try {
  if (fs.statSync(LOG).size > 1 << 20) fs.writeFileSync(LOG, fs.readFileSync(LOG).subarray(-(1 << 19)));
} catch {}

// MCP over stdio: a JSON-RPC message a line
async function respond({ method, params }) {
  if (method === "initialize") return { protocolVersion: params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "browser", version: "1" } };
  if (method === "ping") return {};
  if (method === "tools/list") {
    const required = (input) => Object.keys(input).filter((k) => k !== "about");
    return { tools: Object.entries(TOOLS).map(([name, t]) => ({ name, description: t.description, inputSchema: { type: "object", properties: t.input, required: required(t.input) } })) };
  }
  const tool = method === "tools/call" && TOOLS[params.name];
  if (!tool) throw Object.assign(new Error(`${method} ${params?.name ?? ""} is not offered`), { code: -32601 });
  try {
    return { content: [await tool.run(params.arguments ?? {})] };
  } catch (e) {
    return { content: [said(e.message)], isError: true };
  }
}
const send = (m) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...m })}\n`);
createInterface({ input: process.stdin }).on("line", async (line) => {
  let m;
  try {
    m = JSON.parse(line);
  } catch {
    return;
  }
  if (m.id === undefined || m.method === undefined) return;
  await respond(m).then(
    (result) => send({ id: m.id, result }),
    (e) => send({ id: m.id, error: { code: e.code ?? -32603, message: e.message } }),
  );
});
process.stdin.on("end", () => process.exit(0));
