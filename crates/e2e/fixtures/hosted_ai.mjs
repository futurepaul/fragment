// The hosted e2e's live AI check: one short text call on the plan's text
// model (a reasoning model: its cap leaves room to think) and one small
// image, on the fragment's own OpenRouter key.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  async summarize({ text }, job) {
    return await job.ai.text({ model: "z-ai/glm-5.3-flash", prompt: text, max_tokens: 1500, reasoning: { effort: "low" } });
  }

  async draw({ prompt, path }, job) {
    return await job.ai.image({ prompt, path });
  }
}
