// A chat as the chat template made one before phase 7: a `say` operation
// appends a message to the `chat` channel, and an agent's answers come back
// through it. Kept so the e2e checks such chats still get answers.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  say({ text }, call) {
    call.publish("chat", { text }, "message");
    return { ok: true };
  }
}
