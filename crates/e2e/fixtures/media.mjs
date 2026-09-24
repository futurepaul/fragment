// Deliveries and AI (slice F): pushes from a mutation and a job, and
// OpenRouter text, images, and video as a job's steps.
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
    return await job.ai.text({ model: "openai/gpt-5-mini", prompt: text, reasoning: { effort: "low" } });
  }

  async draw({ prompt, path }, job) {
    return await job.ai.image({ prompt, path });
  }

  async film({ prompt, path }, job) {
    return await job.ai.video({ prompt, path, duration: 6, resolution: "768p" });
  }
}
