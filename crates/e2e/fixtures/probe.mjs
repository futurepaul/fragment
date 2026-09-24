// Tries what a loaded worker may not do (docs/hardening.md, H3).
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

  // grows its heap 8 MiB at a time until the node ends it
  bomb() {
    const keep = [];
    for (let i = 0; i < 512; i++) keep.push(new Array(1 << 20).fill(i + 0.5));
    return { survived: keep.length };
  }

  hello() {
    return { hello: "still here" };
  }
}
