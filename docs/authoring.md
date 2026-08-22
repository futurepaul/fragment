# Authoring workflows: the whole contract

The platform carries the failure leg — retries with backoff, held runs,
auto-pause, loop protection, write-suppression — as defaults. You get all of
it without learning anything. What's left for you is three habits:

1. **Throw, don't catch-and-continue.** If something unexpected happens,
   `throw` and let the run fail. The host retries what's transient, parks
   what isn't (`held`, replayable with `fragment replay`), and pauses the
   workflow if it keeps failing. Recovery code you don't write is recovery
   code you can't get wrong.

2. **Any trigger can fire twice; key effects by cause.** Writing files is
   already safe — identical content is a recorded no-op. For external side
   effects (`ctx.http` calls out), wrap them in `lib/once.mjs`
   (`fragment add once`): `await once(ctx, \`notify:${item.id}\`, fn)`.
   That's the entire idempotency burden.

3. **React to state, write state.** Read the current state of what you
   watch (`ctx.files`, or `lib/poll.mjs` for a remote source), write your
   output as a function of it. Fragments that mirror state converge;
   fragments that emit events in response to events oscillate.

That's it. When a poller is the shape, `fragment add poll` gives you the
transactional diff-and-handle recipe; when something breaks,
`fragment runs <name> --status held` shows you what and
`fragment replay <name> <run-id>` re-runs it after the fix.
