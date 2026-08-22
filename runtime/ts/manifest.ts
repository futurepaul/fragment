// The manifest, decoded exactly once at the door. The TypeBox schema is the
// single source of truth for the author-facing shape: PUT validates against
// it, applies defaults, and stores the normalized form. Everything
// downstream reads the parsed object — nothing re-parses or re-validates.
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
      backoffMs: Type.Optional(Type.Integer({ minimum: 100, maximum: 3_600_000 })),
      maxBackoffMs: Type.Optional(Type.Integer({ minimum: 1000, maximum: 3_600_000 })),
    }),
  ])),
  maxRunsPerHour: Type.Optional(Type.Integer({ minimum: 2, maximum: 100_000 })),
});

const ManifestSchema = Type.Object({
  name: Type.Optional(Type.String()),
  visibility: Type.Union([Type.Literal("public"), Type.Literal("viewers"), Type.Literal("token")]),
  editors: Type.Array(Type.String()),
  viewers: Type.Array(Type.String()),
  workflows: Type.Array(WorkflowSchema),
  secrets: Type.Array(Type.String()),
  liveFiles: Type.Optional(Type.Boolean()),
  // append-only prefixes: writers may add paths under these, never modify
  // or delete existing ones (identical bytes are a no-op; the owner is
  // exempt). Makes many-writer folders race-free by construction.
  appendOnly: Type.Optional(Type.Array(Type.String({ minLength: 1 }))),
  // POST {type:"changed", fragment, rev, paths} to each URL when files
  // change (best-effort, coalesced, 3 retries). The push half of "bots
  // watching bots" — the socket (__watch) stays for CLI sync clients.
  notifyUrls: Type.Optional(Type.Array(Type.String({ minLength: 8 }), { maxItems: 3 })),
  // sync-trigger coalescing window (debounce); the knob is named, the
  // behavior is old
  debounceMs: Type.Optional(Type.Integer({ minimum: 250, maximum: 300_000 })),
});

// Validate + normalize. Returns { error } or { manifest } with defaults
// applied. Field checks TypeBox can't express (npub format, cron syntax)
// run here — this is the only place those rules live.
export function normalizeManifest(m) {
  if (!m || typeof m !== "object") return { error: "manifest must be a JSON object" };
  if (!Value.Check(ManifestSchema, m)) {
    const e = Value.Errors(ManifestSchema, m).First();
    return { error: e ? `manifest${e.path}: ${e.message}` : "manifest does not match schema" };
  }
  const out = Value.Clone(m);
  for (const k of ["editors", "viewers"]) {
    for (const n of out[k]) { try { hexFromNpub(n); } catch { return { error: `bad npub in ${k}: ${n}` }; } }
  }
  for (const wf of out.workflows) {
    if (wf.cron) { try { parseCron(wf.cron); } catch (e) { return { error: `workflow ${wf.name}: ${e.message}` }; } }
    if (wf.retry === false || typeof wf.retry === "boolean") continue;
  }
  if (out.debounceMs === undefined) out.debounceMs = 4000;
  if (out.appendOnly) out.appendOnly = out.appendOnly.map((p) => (p.endsWith("/") ? p : p + "/"));
  for (const wf of out.workflows) {
    if (wf.maxRunsPerHour === undefined) wf.maxRunsPerHour = 120;
    if (wf.retry === true) wf.retry = {};
  }
  return { manifest: out };
}
