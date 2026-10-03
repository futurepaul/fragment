// Tries what an app may not do: code from strings, a blocked thread, and
// the parts of its Durable Object the platform takes away (platform.mjs,
// `lock`).
import { DurableObject } from "cloudflare:workers";

function attempt(f) {
  try {
    return { ok: true, value: String(f()) };
  } catch (e) {
    return { ok: false, error: `${e.name}: ${e.message}` };
  }
}

export class App extends DurableObject {
  try_eval() {
    return attempt(() => eval("1 + 1"));
  }

  try_function() {
    return attempt(() => new Function("return 2")());
  }

  try_wait() {
    return attempt(() => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10));
  }

  try_alarm() {
    return attempt(() => this.ctx.storage.setAlarm(Date.now() + 1000));
  }

  // the original, sought on the prototype
  try_alarm_proto() {
    return attempt(() => Object.getPrototypeOf(this.ctx.storage).setAlarm.call(this.ctx.storage, Date.now() + 1000));
  }

  try_transaction() {
    return attempt(() => this.ctx.storage.transaction(async () => {}));
  }

  try_put() {
    return attempt(() => this.ctx.storage.put("k", "v"));
  }

  try_facets() {
    return attempt(() => this.ctx.facets.get("mine", () => ({})));
  }

  hello() {
    return { hello: "still here" };
  }
}
