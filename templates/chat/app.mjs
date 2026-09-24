// A chat: every message is a record on the `chat` channel. Pages follow it
// live; an agent member can follow it too (`POST /api/a/<agent>/listen`),
// and its answers come back through `say`, as the agent.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  say({ text }, call) {
    call.publish("chat", { text }, "message");
    return { ok: true };
  }
}
