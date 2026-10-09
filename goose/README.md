# Goose in the fragment

This is the WASM Goose GDK runtime optchat's mind runs inside its app
facet. `StateMachine`, `InferenceRunner` and `ToolOperation` are Goose's,
pinned to the WASM fork used by the former `agent/` Worker. This is the
GDK, not the native Goose CLI and its extension system. In particular,
Summon subagents are not enabled by this change.

`job.agent.run({messages, tools}, {model, tool, commit})` runs a fresh
context. The template provides its effects as callbacks backed by the
job's durable steps. Replaying a Workflow round rebuilds Goose from the
same seed and reuses those steps, including paid model answers and tool
effects. No second session database or memory store is added.

The GDK sees a typed conversation. The transport retains the original
wire messages alongside it, preserving cache hints, the memory prefix,
and opaque provider thinking blocks byte for byte. Its model adapter
uses the platform's existing paid text step. Tool calls use Goose's
sequential dispatcher. A malformed call is answered with a tool error;
an unoffered tool is refused before its host callback runs.

The facet has no ambient network; the callbacks have only the host job's
existing capabilities. The template enforces its model, tool and output
limits; the runtime additionally bounds machine steps. When a Workflow
round suspends at a missing step, the platform cancels pending WASM
callbacks to release the old Rust future before the next replay.

`cargo xtask build` builds this module first with worker-build's pinned
wasm-bindgen and esbuild. The cell embeds its JavaScript and compiled
WASM as Loader modules, content-addressed with the platform code and
loaded only when a job uses the runtime. The artifacts are ignored;
`goose/Cargo.lock` pins the inputs. `cargo xtask check` includes its host
tests and host/WASM clippy checks.

The separate native Goose process (`images/goose`) is still the hands.
It receives the current compressed shared view and read-only memory
tools when the mind hands it computer work.
