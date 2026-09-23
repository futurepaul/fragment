# Spike 4: celld v0.4.0 → v0.5.1

Status: done 2026-09-23. Verdict: **adopt v0.5.1 with a one-hunk fork
fix** for an alarm regression (an alarm held open while its handler's
timers run); the fix goes upstream.

## What changed for us

| Change in 0.5 | What we did |
|---|---|
| `CELLD_WORKER_LOADER` is removed ("declare `worker_loaders` in the project config") | `runtime/wrangler.jsonc` declares `"worker_loaders": [{ "binding": "LOADER" }]` |
| `CELLD_VAR_*` passthrough is removed ("set `vars` in the Wrangler config, or `.dev.vars` for `celld dev`") | `scripts/dev` writes `runtime/.dev.vars` (mode 600, gitignored), one line per value; the runtime already turns literal `\n` back into newlines for the PEM |
| `celld deploy` reads no `.dev.vars` ("a local credential cannot reach a fleet") | Production secrets must be `vars` in the deployed config, so they live in the fleet bucket's deployment record. Phase 3 decides how `xtask deploy` renders them (the bucket credentials already give full control of the fleet) |
| Upgrading from 0.4.x to 0.5.0 needs a whole-fleet stop; 0.5.0 → 0.5.1 is rolling | dev state wiped (`scripts/dev wipe`); nothing hosted yet |

Install: the release asset `celld-aarch64-apple-darwin.gz` from
github.com/denoland/celld v0.5.1, checked with `gh attestation verify`
(signer: `denoland/celld/.github/workflows/release.yml`); 0.4.0 is kept as
`~/.local/bin/celld-0.4.0` for the finite-next stack, whose state is 0.4.0.

What 0.4.1–0.5.1 bring that the model needs: Durable Object facets
(`ctx.facets`, 0.4.1), `worker_loaders` in config and loaded Workers with
service bindings and a custom outbound (0.5.0), `WorkerCode.limits`
(`cpuMs`, `subRequests`) enforced and Tail Workers for loaded code (0.5.1),
Workflow run retention (0.5.0), `celld r2` (0.5.1).

## Baseline on 0.5.1

- Runtime unit tests: 32/32.
- E2E (`E2E_FAL_FAKE=1`): 311 of 312 checks; the one failure is the
  regression below (reported twice by the harness: the timeout and the
  assertion).

## Regression: an alarm handler's completion is held until the operation deadline

**Symptom.** `node scripts/e2e.mjs --only paused` fails "paused trigger
recorded as blocked, not run" on 10 of 12 runs (two full suites, ten
`--only paused`); both passes came right after a runtime rebuild. The
inbox POST writes a `pending` run and re-arms the alarm to now; the alarm
handler should record the run as `blocked` within milliseconds. The run
stays `pending` for ~15 s.

**Trace** (one fragment, console lines added to `alarms.ts`, since
removed):

```
15:41:06.559 rearm 0 ms        (git refresh after the first commit)
15:41:06.565 alarm enter       (no pending runs)
15:41:06.571 rearm +3987 ms    (the handler's own re-arm, in its finally)
15:41:06.584 rearm 0 ms        (the inbox POST: a pending run due now)
15:41:21.899 alarm enter       (pending run 15 316 ms overdue)
```

Neither the "now" alarm nor the +3987 ms one fired. The next firing came
15.33 s after the previous handler *started*, which is
`CELLD_OPERATION_DEADLINE_MS` (15 000): celld treated the finished handler
as still running and re-fired at the deadline.

**Confirmation.** With `CELLD_OPERATION_DEADLINE_MS=5000` the same section
passes 3 of 3 (the stall shrinks to ~5 s, inside the test's 15 s wait). On
v0.4.0 the same code passed 312/312.

**Root cause** (found by the sibling session "fragment next (fork)",
2026-09-23; introduced in v0.5.0, v0.4.1 is fine). An `alarm()` that leaves
a timer pending holds the alarm "firing" until the timer ends or
`CELLD_OPERATION_DEADLINE_MS` expires it. That covers `setTimeout` and
`AbortSignal.timeout()`, and fragment's `codestorage.ts` `csFetch` uses a
30 s one. A re-arm made meanwhile waits, even one from `alarm()` itself.
v0.5 added `|| self.completed_cell_event` to `InFlight::keeps_native_ops()`
(`crates/celld/js.rs`), so a completed event keeps its pending I/O, and
`RuntimeManager::fire_alarm` (`crates/celld/runtime.rs`) awaited that whole
drive before reporting `AlarmFinished`; the core staged every new
`setAlarm` in `AlarmState::Firing` until then. My attempted repro
(`no-repro-alarm.js`) left no timer, which is why it never failed. The
sibling's minimal repro: 0.4.0 and 0.4.1 re-arm in 2–6 ms; 0.5.0 and 0.5.1
wait 14.5 s with a 60 s timer or `AbortSignal.timeout(60 s)`, 2.5 s with a
3 s timer, and a self-re-arming 100 ms alarm fires at 15 ms, 15.5 s, 31 s.

**Fix** (candidate, one hunk in `crates/celld/runtime.rs`, in the fork
worktree `celld-worktrees/alarm-fix`, branch `fix/alarm-completion`): on a
successful alarm, `fire_alarm` detaches the drive instead of awaiting it.
The handler already settled its claim and replied with its final snapshot
in the completion turn; a failed or cancelled alarm still awaits the drive.
With the patched release build, `node scripts/e2e.mjs --only paused`
passes 10 of 10 (the unpatched source build: 0 of 3), and the full e2e
passes 312 of 312.

**Our exposure.** The model multiplexes every schedule and retry on one
alarm per fragment (`docs/MODEL.md`), so a 15 s stall on a re-arm right
after a firing is a correctness-visible latency bug (cron, retries, inbox
runs). Recorded in `docs/technical-debt-ledger.md`. Paul (2026-09-23):
run a fork that fixes it and send the fix upstream.
