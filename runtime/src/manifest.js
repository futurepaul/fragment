// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { hexFromNpub } from "./bech32.js";
import { parseCron } from "./cron.js";
const WorkflowSchema = Type.Object({
  name: Type.String({ minLength: 1, maxLength: 64 }),
  file: Type.String({ minLength: 1 }),
  cron: Type.Optional(Type.String()),
  trigger: Type.Optional(Type.Union([Type.Literal("inbox"), Type.Literal("sync")])),
  paused: Type.Optional(Type.Boolean()),
  cycles: Type.Optional(Type.Boolean()),
  // retry policy: false = never retry; otherwise defaults apply
  retry: Type.Optional(Type.Union([
    Type.Boolean(),
    Type.Object({
      attempts: Type.Optional(Type.Integer({ minimum: 1, maximum: 10 })),
      backoffMs: Type.Optional(Type.Integer({ minimum: 100, maximum: 36e5 })),
      maxBackoffMs: Type.Optional(Type.Integer({ minimum: 1e3, maximum: 36e5 }))
    })
  ])),
  maxRunsPerHour: Type.Optional(Type.Integer({ minimum: 2, maximum: 1e5 }))
});
const ManifestSchema = Type.Object({
  name: Type.Optional(Type.String()),
  visibility: Type.Union([Type.Literal("public"), Type.Literal("viewers"), Type.Literal("token")]),
  editors: Type.Array(Type.String()),
  viewers: Type.Array(Type.String()),
  workflows: Type.Array(WorkflowSchema),
  secrets: Type.Array(Type.String()),
  liveFiles: Type.Optional(Type.Boolean()),
  // sync-trigger coalescing window (debounce); the knob is named, the
  // behavior is old
  debounceMs: Type.Optional(Type.Integer({ minimum: 250, maximum: 3e5 }))
});
function normalizeManifest(m) {
  if (!m || typeof m !== "object") return { error: "manifest must be a JSON object" };
  if (!Value.Check(ManifestSchema, m)) {
    const e = Value.Errors(ManifestSchema, m).First();
    return { error: e ? `manifest${e.path}: ${e.message}` : "manifest does not match schema" };
  }
  const out = Value.Clone(m);
  for (const k of ["editors", "viewers"]) {
    for (const n of out[k]) {
      try {
        hexFromNpub(n);
      } catch {
        return { error: `bad npub in ${k}: ${n}` };
      }
    }
  }
  for (const wf of out.workflows) {
    if (wf.cron) {
      try {
        parseCron(wf.cron);
      } catch (e) {
        return { error: `workflow ${wf.name}: ${e.message}` };
      }
    }
    if (wf.retry === false || typeof wf.retry === "boolean") continue;
  }
  if (out.debounceMs === void 0) out.debounceMs = 4e3;
  for (const wf of out.workflows) {
    if (wf.maxRunsPerHour === void 0) wf.maxRunsPerHour = 120;
    if (wf.retry === true) wf.retry = {};
  }
  return { manifest: out };
}
export {
  normalizeManifest
};
