// lib/once.mjs — run an effect exactly once per key.
//
//   import { once } from "../lib/once.mjs";
//   const r = await once(ctx, `notify:${item.id}`, async () => {
//     await ctx.http(webhook, { method: "POST", body: JSON.stringify(item) });
//   });
//   if (r.skipped) ctx.log("already notified");
//
// The marker lives in ctx.state, which is per-workflow and survives crashes,
// restarts, and replays — a replayed run re-derives "did I already do this"
// instead of doing it twice. This is the answer to the one irreducible
// at-least-once edge (external side effects); file writes don't need it
// (identical writes are suppressed by the platform).
export async function once(ctx, key, fn) {
  const k = `once:${key}`;
  if (await ctx.state.get(k)) return { skipped: true };
  const out = await fn();
  await ctx.state.put(k, Date.now());
  return { ran: true, out };
}
