// live vault: tell connected viewers when files change. `trigger: "sync"`
// fires this on editor-plane writes (sync pushes, CLI writes — coalesced);
// setState broadcasts to the "vault" room, whose members refresh in place.
export async function run(ctx, input) {
  const paths = input?.sync?.paths || [];
  if (!paths.length) return { notified: 0 };
  const prev = (await ctx.rooms.getState("vault")) || {};
  const state = { at: input.sync.at || Date.now(), paths: paths.slice(-100), n: (prev.n || 0) + 1 };
  await ctx.rooms.setState("vault", state);
  return { notified: paths.length, state };
}
