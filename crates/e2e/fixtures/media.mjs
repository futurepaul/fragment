// Deliveries and AI (slice F): pushes from a mutation and a job, and
// text (the model route: tools, drafts), decisions (Clef) and images
// (Workers AI) as a job's steps; a video step, which the platform refuses.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  notify_all({ title }, call) {
    call.push("*", { title, body: "from a mutation" });
    return { ok: true };
  }

  // a record on a channel a member may subscribe a URL to
  headline({ text }, call) {
    call.publish("news", { text });
    return { ok: true };
  }

  async announce({ who, title }, job) {
    return await job.push(who, { title, body: "from a job" });
  }

  async summarize({ text }, job) {
    return await job.ai.text({ model: "medium", prompt: text, reasoning_effort: "high" });
  }

  // the high tier is off (decision 23): the step says so
  async summarize_high({ text }, job) {
    return await job.ai.text({ model: "high", prompt: text });
  }

  // a text step as the input names it (tools, a draft, …)
  async ask_text(input, job) {
    return await job.ai.text(input);
  }

  // a tool turn: the model calls a tool, the job runs it, and the model
  // answers from its result, the conversation passed back as it came
  // (`role`, when named, is whose model its payer chose runs it: chat or memory)
  async tool_turn({ ask, role }, job) {
    const tools = [{ type: "function", function: { name: "lookup", description: "Looks a word up.", parameters: { type: "object", properties: { word: { type: "string" } }, required: ["word"] } } }];
    const messages = [{ role: "user", content: ask }];
    const first = await job.ai.text({ messages, tools, tool_choice: "auto", role });
    const call = first.message.tool_calls?.[0];
    if (!call) return { first };
    const { word } = JSON.parse(call.function.arguments);
    messages.push(first.message, { role: "tool", tool_call_id: call.id, content: `${word}: a small piece broken off` });
    const second = await job.ai.text({ messages, tools, role });
    return { first, second };
  }

  // a decision (Clef), as the input names it
  async sort(input, job) {
    return await job.ai.decide(input);
  }

  // whatever else the input names goes to the step too (steps, or a key it refuses)
  async draw({ prompt, path, ...rest }, job) {
    return await job.ai.image({ prompt, path, ...rest });
  }

  // videos are off until they run on Cloudflare: the step says so
  async film({ prompt, path }, job) {
    return await job.ai.video({ prompt, path, duration: 6, resolution: "768p" });
  }
}
