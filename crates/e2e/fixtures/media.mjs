// Deliveries and AI (slice F): pushes from a mutation and a job, and
// text (the model route) and images (Workers AI) as a job's steps; a video
// step, which the platform refuses.
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

  async decide(input, job) {
    return await job.ai.decide(input);
  }

  // the high tier is off (decision 23): the step says so
  async summarize_high({ text }, job) {
    return await job.ai.text({ model: "high", prompt: text });
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
