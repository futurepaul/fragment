// A channel people may post to, and a mutation that fills it faster than
// posts can (64 records a call), so the e2e can pass its retention.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  fill({ n }, call) {
    for (let i = 0; i < n; i++) call.publish("wall", { i });
    return { n };
  }
}
