// lib/poll.mjs — the cron-poller recipe, done transactionally.
//
//   import { poll } from "./lib/poll.mjs";
//   await poll(ctx, {
//     key: "vault",                      // per-workflow cursor namespace
//     fetch: async () => items,          // the source's current items
//     id: (it) => it.id,                 // stable identity (default: .id)
//     handle: async (item) => {...},     // one unseen item
//   });
//
// Why this shape: any trigger may fire twice, and a run may crash mid-pass.
// poll() diffs against a seen-set kept in ctx.state, handles only unseen
// items, and advances the cursor only after every item was handled without
// throwing — so a crashed pass re-sees its items (wrap side effects in
// lib/once.mjs to make that harmless), and a duplicate trigger sees nothing
// new. The platform's write-suppression makes the file side of this exact.
export async function poll(ctx, { key, fetch: fetchItems, handle, id = (it) => it.id }) {
  const k = `poll:${key}:seen`;
  const seen = (await ctx.state.get(k)) || [];
  const seenSet = new Set(seen);
  const items = await fetchItems();
  const fresh = items.filter((it) => !seenSet.has(id(it)));
  for (const it of fresh) await handle(it);
  // bounded seen-set: the last 5000 identities
  await ctx.state.put(k, [...seen, ...fresh.map(id)].slice(-5000));
  return { scanned: items.length, fresh: fresh.length };
}
