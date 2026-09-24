// The hosted e2e's egress check: a job fetches a URL and answers what
// happened (a refused step throws, and the job catches it).
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  async probe({ url }, job) {
    try {
      const r = await job.fetch(url);
      return { status: r.status };
    } catch (e) {
      return { error: String(e?.message ?? e) };
    }
  }
}
