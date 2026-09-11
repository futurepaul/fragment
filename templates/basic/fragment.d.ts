/**
 * fragment platform types — ambient declarations for workflow and app code.
 *
 * Author in .ts (the fragment CLI compiles it away at deploy) or plain .mjs
 * with JSDoc `@type {import("./fragment.d.ts").Ctx}` — either way you get
 * these hints. Every doc comment here is a real contract; the ones marked
 * LESSON were learned from production incidents.
 */

/** One pending inbox message, as `ctx.inbox()` returns it. */
export interface InboxMessage {
  id: number;
  at: number;
  source: string;
  payload: any;
}

/** Result of `ctx.files.write`. */
export interface WriteResult {
  ok: boolean;
  /** true when the content was byte-identical to the branch head — no
   *  commit was made (write-suppression; the fuel of copy-loops). */
  deduped: boolean;
  /** The git blob sha of the content written (or already present when
   *  deduped) — the identity `stat().sha` reports. */
  sha: string;
  /** The commit that carried the write; the head sha when deduped; null
   *  when nothing changed at all. */
  commitSha: string | null;
}

/** Path identity at the working-copy pin, from `ctx.files.stat`. */
export interface FileStat {
  path: string;
  /** The git blob sha — the CONTENT identity the ifSha CAS pins to. */
  sha: string;
  /** The most recent commit that touched the path. */
  lastCommitSha: string;
  size: number;
  /** false when the path is absent at the pin — deleted and never-existed
   *  are the same thing under git (no tombstones at a ref). */
  present: boolean;
}

/** Thrown by `ctx.files.write` when `{ ifSha }` loses the race. */
export class ContentConflict extends Error {
  conflict: true;
  currentSha: string;
}

export interface FilesApi {
  /** Read a file's bytes as text (from the pinned working copy). Throws
   *  over the 8MiB decode ceiling — consume giants streamed instead. */
  read(path: string): Promise<string>;
  readBytes(path: string): Promise<ArrayBuffer>;
  /**
   * Write a file: one git commit to `main` under expected-parent CAS.
   * Identical content is a recorded no-op (deduped) that makes no commit —
   *  that is the write-suppression layer of loop protection.
   *
   * Pass `{ ifSha }` (from `ctx.files.stat`) for read-modify-write: the
   * write only lands if the path still carries that content sha, else it
   * rejects with `e.conflict === true` and `e.currentSha`.
   *
   * LESSON — always pin multi-step updates: a workflow that holds a
   * snapshot across a slow await (an LLM call, an outbound fetch) WILL race
   * other writers. `stat` → merge → `write({ ifSha })` makes the stale
   * write fail loudly instead of clobbering a concurrent edit — or
   * resurrecting a file someone deleted mid-flight.
   *
   * Replays are safe by construction: a run re-executed after a crash
   * re-resolves the branch head and commits each logical change exactly
   * once (identical bytes dedup; racing writers trigger a bounded
   * refetch-and-retry).
   */
  write(path: string, data: string | ArrayBuffer | Uint8Array, opts?: { ifSha?: string }): Promise<WriteResult>;
  /**
   * Delete a path (append-only prefixes refuse; deleting an absent path
   * is a recorded no-op).
   */
  delete(path: string): Promise<WriteResult>;
  /** Fetch a remote URL and commit it at path (dedup + append-only gates
   *  as usual). Media-scale outputs (32MiB+) belong to the CLI's direct
   *  commit path. */
  ingest(url: string, path: string): Promise<{ path: string; sha: string; size: number; url: string }>;
  list(prefix?: string): Promise<string[]>;
  /** Like list(), but with metadata: [{path, size, mode, lastCommitSha}]. */
  index(prefix?: string): Promise<Array<{ path: string; size: number; mode: string; lastCommitSha: string }>>;
  /**
   * Path identity at the pinned working copy — the read half of the ifSha
   * pattern. `present: false` covers both "never existed" and "deleted".
   */
  stat(path: string): Promise<FileStat>;
}

export interface Ctx {
  files: FilesApi;
  /** Declared secrets, by name. Never logged, never in files. */
  secrets: Record<string, string>;

  /** Outbound fetch. Stamps x-fragment-hops for cycle detection. */
  http(url: string, init?: RequestInit): Promise<Response>;
  /**
   * Drain pending inbox messages. Delivery is at-least-once.
   *
   * LESSON — ack what you drain, ALWAYS: `await ctx.inboxAck(ids.map(m =>
   * m.id))` once you've handled (or durably recorded an error for) every
   * message. The runtime's claim reaper returns un-acked messages to
   * pending after 10 minutes, and the next drain will replay them — a
   * handler that never acks re-applies its entire history roughly every
   * ten minutes forever. (Ask meatproxy.)
   */
  inbox(): Promise<InboxMessage[]>;
  /** Mark messages done by id — the other half of the LESSON above. */
  inboxAck(ids: number[]): Promise<void>;
  /** Append to the event ledger — the fragment's "what happened". */
  events: {
    append(kind: string, data?: unknown): Promise<void>;
  };
  /** Per-workflow key/value state. */
  state: {
    get(key: string): Promise<any>;
    put(key: string, value: any): Promise<void>;
  };
  /**
   * Send a Web Push notification to every device `who` has subscribed
   * (browsers subscribe via fragment.push.register() from the page).
   * Delivery is best-effort and self-healing: 404/410 from the push
   * service drops that subscription permanently; five other failures
   * drops it too. No subscriptions for `who` is a quiet success
   * ({sent: 0}), not an error. Payload caps: title 80, body 200, url 500,
   * tag 100 (collapse key).
   */
  push(who: string, payload: { title: string; body?: string; url?: string; tag?: string }): Promise<{ sent: number; dropped: number; detail: string }>;
  log(msg: string): void;
}

/**
 * The served-app handler shape (app.mjs / app.ts). GETs should return the
 * page or API responses; everything not matching a real site/ file or a
 * reserved platform path arrives here.
 */
export interface FragmentApp {
  fetch(req: Request): Promise<Response>;
}

declare global {
  /** The workflow entry point: `export async function run(ctx)`. */
  async function run(ctx: Ctx): Promise<unknown>;
}

/**
 * The platform "ai" module — `import { … } from "fragment:ai"`. One call shape and
 * one result shape across text, image, and video; the host holds the keys
 * and the default models. Loaded only for fragments that import it.
 */
declare module "fragment:ai" {
  /** Media output: already a file in the working copy (syncs to the folder). */
  interface MediaFile {
    mediaType: string;
    path: string;
    /** Site-relative serve URL (`__file?path=…`). */
    url: string;
    sha256: string;
    size: number;
        /** Lazy byte access — fetched from the file plane on demand. */
    bytes(): Promise<Uint8Array>;
    base64(): Promise<string>;
  }

  interface ImageGenOpts {
    prompt: string;
    model?: string;
    n?: number;
    size?: string;
    seed?: number;
    dir?: string;
    providerOptions?: { fal?: Record<string, unknown> };
  }
  interface VideoGenOpts {
    prompt: string;
    model?: string;
    duration?: number;
    aspectRatio?: "21:9" | "16:9" | "4:3" | "1:1" | "3:4" | "9:16";
    resolution?: "480P" | "768P";
    seed?: number;
    dir?: string;
    providerOptions?: { fal?: Record<string, unknown> };
  }

  export function generateText(opts: Record<string, any>): Promise<{ text: string; [k: string]: any }>;
  export function streamText(opts: Record<string, any>): Promise<AsyncIterable<string> & Record<string, any>>;
  export function generateObject(opts: Record<string, any>): Promise<Record<string, any>>;
  export function tool(def: Record<string, any>): Record<string, any>;
  export function generateImage(opts: ImageGenOpts): Promise<{ image: MediaFile; images: MediaFile[] }>;
  export function generateVideo(opts: VideoGenOpts): Promise<{ video: MediaFile }>;
  export class NoImageGeneratedError extends Error {}
  export class NoVideoGeneratedError extends Error {}
  export class XSAIError extends Error {}
}
