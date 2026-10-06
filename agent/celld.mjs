// The agents' Worker where the runtime has no Secrets Store (celld:
// docs/self-host.md, seam 12): its build, each class taking its env
// through the cell's secrets shim (cell/secrets.mjs), as the cell's
// entry.mjs does, so agent/src/keys.rs reads its host secret as
// `await env.HOST_SECRET.get()` on either runtime. Only celld's render of
// this project runs it (crates/devstack/src/celld.rs); Cloudflare and
// `wrangler dev` run the build itself (wrangler.jsonc).
import Agents, { Agent as AgentCell } from "./build/index.js";
import { withSecrets } from "../cell/secrets.mjs";

export default class extends Agents {
  constructor(ctx, env) {
    super(ctx, withSecrets(env));
  }
}

export class Agent extends AgentCell {
  constructor(ctx, env) {
    super(ctx, withSecrets(env));
  }
}
