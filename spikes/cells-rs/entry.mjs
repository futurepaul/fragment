// The thin JavaScript shim: everything but the class declarations is Rust.
// workers-rs 0.8.5 classes do not extend DurableObject, so celld refuses RPC
// to them; this class extends it and forwards every handler to the Rust
// object. Channel is a capability entrypoint the supervisor hands to its
// app facet (`ctx.exports.Channel({ props })`); workers-rs 0.8.5 has no
// WorkerEntrypoint classes.
import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import * as rs from "./build/index.js";

export default rs.default;

export class Supervisor extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.Supervisor(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
  alarm(info) { return this.rs.alarm(info); }
  webSocketMessage(ws, message) { return this.rs.webSocketMessage(ws, message); }
  webSocketClose(ws, code, reason, clean) { return this.rs.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.rs.webSocketError(ws, error); }
  capAppend(channel, body) { return this.rs.capAppend(channel, body); }
}

export class Channel extends WorkerEntrypoint {
  append(channel, body) {
    const ns = this.env.SUPERVISOR;
    const stub = ns.get(ns.idFromString(this.ctx.props.cell));
    return stub.capAppend(String(channel), JSON.stringify(body ?? null));
  }
}
