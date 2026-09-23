# Spike 4: celld v0.4.0 → v0.5.1

Status: done 2026-09-23. Verdict: **adopt v0.5.1**, with one upstream
regression to report (alarm completion held until the operation deadline).

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

**Not reproduced minimally.** `no-repro-alarm.js` with `no-repro-sweep.mjs`
re-arms to now at 0–40 ms after an alarm whose handler re-arms to +4 s
(small writes, 80 KB, and 4 MB of writes in the handler): 0 of 92 lost. The
difference from fragment's cell is not yet known (fragment's handler reads
a manifest, runs sync triggers, and writes events and runs; its request
path does the same writes as the POST above).

**Likely area.** `crates/celld/actor.rs` `Effect::FireAlarm`: the shell
reports `AlarmFinished` after the consuming commit's proof ("the deadline
that stands after the handler"); a request that re-arms inside that window
appears to leave the core waiting until the deadline.

**Our exposure.** The model multiplexes every schedule and retry on one
alarm per fragment (`docs/MODEL.md`), so a 15 s stall on a re-arm right
after a firing is a correctness-visible latency bug (cron, retries, inbox
runs). Recorded in `docs/technical-debt-ledger.md`. Reporting it upstream
is Paul's call (an issue on github.com/denoland/celld is public).
