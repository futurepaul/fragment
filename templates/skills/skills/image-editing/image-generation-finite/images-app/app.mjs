// An images fragment: one job, `draw`, that asks the platform's image model
// (FLUX.1 [schnell] on Workers AI, through the fragment's AI step) for a
// JPEG and writes it to `path` on main. The fragment's owner pays for each.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  async draw({ prompt, path, steps }, job) {
    const image = await job.ai.image({ prompt, path, steps: steps ?? 4 });
    return { path: image.path, size: image.size, sha256: image.sha256 };
  }
}
