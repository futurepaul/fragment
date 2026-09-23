// Spike 3: a libfx agent turn as a Workflow. Every model call and every
// tool call is a numbered step, so a replay after a crash answers them from
// the step ledger instead of repeating them. The request body of each model
// call is hashed into its step record; a replay that asks a different
// question fails the instance instead of reusing a mismatched answer.
import { WorkflowEntrypoint } from "cloudflare:workers";
import { NonRetryableError } from "cloudflare:workflows";
import { createFxAgent } from "libfx/wasm";
import fxWasm from "./node_modules/libfx/fx-core.wasm";

const GATEWAY = "https://ai-gateway.vercel.sh";
const MODEL = "spike/fake";
const STEP = { retries: { limit: 3, delay: 500, backoff: "constant" }, timeout: 60_000 };

async function sha256hex(text) {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export class AgentTurn extends WorkflowEntrypoint {
  async run(event, step) {
    const { turnId, prompt, fake } = event.payload;
    const replay = { model: 0, tool: 0 };

    const fetchThroughSteps = async (input, init = {}) => {
      const url = new URL(typeof input === "string" ? input : input.url);
      if (url.origin !== GATEWAY) throw new Error(`fx transport: unexpected origin ${url.origin}`);
      if ((init.method || "GET") === "GET" && url.pathname === "/coding-agent/v1/models") {
        return Response.json({ data: [{ id: MODEL, type: "language", context_window: 100_000, max_tokens: 4096, tags: ["tool-use"] }] });
      }
      if (init.method !== "POST" || url.pathname !== "/v4/ai/language-model") throw new Error(`fx transport: unexpected route ${url.pathname}`);
      const body = await new Response(init.body).text();
      const bodySha = await sha256hex(body);
      const n = replay.model++;
      const record = await step.do(`model-${n}`, STEP, async () => {
        const r = await fetch(`${fake}/model`, { method: "POST", body, headers: { "x-turn": turnId, "x-call": String(n) } });
        if (!r.ok) throw new Error(`model call ${n}: ${r.status}`);
        return { bodySha, sse: await r.text() };
      });
      if (record.bodySha !== bodySha) throw new NonRetryableError(`replay diverged at model-${n}: the request differs from the recorded one`);
      return new Response(record.sse, { headers: { "content-type": "text/event-stream" } });
    };

    const addNote = {
      name: "add_note",
      description: "Add a note to the shared list.",
      inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
      execute: async (input) => {
        const n = replay.tool++;
        // The step name is the operation id's suffix: a tool effect that
        // crashed mid-flight runs again with the same key, and the effect
        // (an operation) dedupes by it.
        return step.do(`tool-${n}`, STEP, async () => {
          const r = await fetch(`${fake}/effect`, { method: "POST", body: JSON.stringify({ key: `${turnId}:tool-${n}`, text: input.text }) });
          if (!r.ok) throw new Error(`tool ${n}: ${r.status}`);
          return await r.json();
        });
      },
    };

    const agent = await createFxAgent({
      wasm: fxWasm,
      apiKey: "host-managed",
      model: MODEL,
      fetch: fetchThroughSteps,
      instructions: "You add notes when asked.",
      tools: [addNote],
    });
    try {
      const turn = agent.prompt(prompt);
      let text = "";
      for await (const ev of turn) if (ev.type === "text_delta") text += ev.delta;
      const result = await turn.result;
      const checkpoint = await agent.checkpoint();
      return { text, stopReason: result.stopReason, modelCalls: replay.model, toolCalls: replay.tool, checkpointBytes: checkpoint.byteLength };
    } finally {
      await agent.close();
    }
  }
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    if (req.method === "POST" && url.pathname === "/turn") {
      const { id, prompt, fake } = await req.json();
      const instance = await env.TURNS.create({ id, params: { turnId: id, prompt, fake } });
      return Response.json({ id: instance.id });
    }
    const m = url.pathname.match(/^\/turn\/([A-Za-z0-9_-]{1,100})$/);
    if (req.method === "GET" && m) {
      const instance = await env.TURNS.get(m[1]);
      return Response.json(await instance.status());
    }
    return new Response("not found", { status: 404 });
  },
};
