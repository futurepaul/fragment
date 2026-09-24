// Budgets (phase 4 slice C): jobs with paid steps.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  async summarize({ text }, job) {
    return await job.ai.text({ model: "openai/gpt-5-mini", prompt: text });
  }

  // two paid steps: a replay after the second was refused pays only for it
  async twice({ a, b }, job) {
    const one = await job.ai.text({ model: "openai/gpt-5-mini", prompt: a });
    const two = await job.ai.text({ model: "openai/gpt-5-mini", prompt: b });
    return { one: one.text, two: two.text };
  }

  // anyone who can see the fragment may call it; its owner pays
  async ask({ text }, job) {
    return await job.ai.text({ model: "openai/gpt-5-mini", prompt: text });
  }
}
