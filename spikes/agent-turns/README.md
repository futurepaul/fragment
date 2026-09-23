# Spike 3: deterministic agent turns in Workflows

Status: done 2026-09-23. Verdict: **an agent turn is a Workflow.** libfx
0.0.10 (WebAssembly) runs inside a celld Workflow; every model call and
every tool call is a numbered `step.do`, and a node killed mid-turn resumes
the turn with no finished call repeated. finite-next's lease, watchdog,
and operation log are not needed as the primary mechanism.

## Shape (`index.js`)

- `run()` creates the libfx agent with a host `fetch` that answers the
  model catalog locally and sends each language-model request through
  `step.do("model-<n>")`. The step returns the whole SSE body and the
  SHA-256 of the request; on replay a different request fails the
  instance (`NonRetryableError`) instead of reusing a mismatched answer.
- Each tool's `execute` is `step.do("tool-<n>")`, and the effect carries
  the key `<turn>:tool-<n>`, the operation id the effect dedupes on.
- Everything outside a step runs again on replay (celld re-enters `run()`
  from the top), so libfx is rebuilt from the turn's payload each time.

## Checks (`../driver`, `turns . <out.json>`: 13 of 13)

A fake model (scripted: two `add_note` tool calls, then text) and a fake
effect service count every call. `celld dev` runs the node as a child
process; the driver SIGKILLs that node.

| Run | Model calls (attempts) | Tool effects (executions) | Result |
|---|---|---|---|
| control | 1, 1, 1 | 1, 1 | answer intact; 555 ms; checkpoint 1,863 bytes |
| killed during the third model call | 1, 1, **2** | 1, 1 | answer intact; the in-flight call ran again |
| killed during the second tool effect | 1, 1, 1 | 1, **2** | answer intact; the effect applied once (key dedupe) |

The request hashes matched on every replay: libfx asks the same questions
from the same payload, so the step ledger lines up. Restart to completion
took 4.4 s, which is the `celld dev` start time; the driver set
`CELLD_WAKER_TICK_MS=2000`, although the restarted node resumed its own
instance without waiting for a scan.

## Rules this establishes

- A step is at-least-once: whatever was in flight when a node died runs
  again. Model calls tolerate that (a repeated question); tool effects must
  be operations keyed by `<turn>:tool-<n>`, which the operation ledger
  already dedupes (spike 2).
- Step results are capped at 1 MiB (Workflows), so a model response or a
  tool result above that must go to R2 and be referenced by key.
- Non-step work may stay pending for at most 60 s (celld), so libfx's own
  work between steps must stay short; it does.
- The fake speaks AI SDK LanguageModelV3 stream parts (`finishReason:
  {unified, raw}`, nested usage), as `@ai-sdk/openai-compatible` 3.x emits
  them. The OpenRouter adapter (phase 5) runs inside the model step.

## Not covered here

A real model (OpenRouter `z-ai/glm-5.3-flash`) through this path, streaming
partial text to watchers while a step runs (the step returns only when the
response is complete, so live deltas need a side channel), and
cancellation of a running turn (`instance.terminate()` plus libfx's
signal). All three are phase 5.

A parallel spike (worktree `fragment-next-worktrees/goose-agent`, branch
`spike/goose-agent`) tests goose's agent loop as a replacement for both
libfx and fx. The rule here does not depend on the kernel: any loop whose
model and tool calls pass through host callbacks can put each call in a
numbered step, and goose's state machine (one inference or one tool batch
per step, effects returned to the caller) maps onto steps directly.
