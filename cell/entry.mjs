// The only hand-written JavaScript on the platform side (besides
// platform.mjs, which runs inside the app facet). Everything else is Rust.
// celld refuses RPC to a class that does not extend DurableObject, and
// workers-rs classes do not, so these classes do and forward each handler.
import { DurableObject } from "cloudflare:workers";
import * as rs from "./build/index.js";

export default rs.default;

export class Fragment extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.FragmentCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
  alarm(info) { return this.rs.alarm(info); }
  webSocketMessage(ws, message) { return this.rs.webSocketMessage(ws, message); }
  webSocketClose(ws, code, reason, clean) { return this.rs.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.rs.webSocketError(ws, error); }
}

export class Principal extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.PrincipalCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
}
