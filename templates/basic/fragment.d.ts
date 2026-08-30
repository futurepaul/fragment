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
  /** true when the content was byte-identical — rev/updatedAt did NOT move. */
  deduped: boolean;
  rev: number;
}

/** Live row metadata, from `ctx.files.stat`. */
export interface FileStat {
  path: string;
  rev: number;
  sha256: string;
  size: number;
  /** true when the row is a tombstone — the path is deleted, and `rev` is
   *  the revision OF the deletion. */
  deleted: boolean;
}

/** Thrown by `ctx.files.write` when `{ ifRev }` loses the race. */
export class RevConflict extends Error {
  conflict: true;
  currentRev: number;
}

export interface FilesApi {
  /** Read a file's bytes as text. Throws over the 8MiB decode ceiling —
   *  consume giants by hash with ranged access instead. */
  read(path: string): Promise<string>;
  readBytes(path: string): Promise<ArrayBuffer>;
  /**
   * Write a file. Identical content is a recorded no-op (deduped) that does
   * not churn the revision counter.
   *
   * Pass `{ ifRev }` (from `ctx.files.stat`) for read-modify-write: the
   * write only lands if the row is still at that revision, else it rejects
   * with `e.conflict === true` and `e.currentRev`.
   *
   * LESSON — always pin multi-step updates: a workflow that holds a
   * snapshot across a slow await (an LLM call, an outbound fetch) WILL race
   * other writers. `stat` → merge → `write({ ifRev })` makes the stale
   * write fail loudly instead of clobbering a concurrent edit — or
   * resurrecting a file someone deleted mid-flight.
   */
  write(path: string, data: string | ArrayBuffer, opts?: { ifRev?: number }): Promise<WriteResult>;
  /** Stream a remote URL into the tier and commit it at path. */
  ingest(url: string, path: string): Promise<{ path: string; sha256: string; size: number; mime: string; url: string }>;
  list(prefix?: string): Promise<string[]>;
  /** Like list(), but with metadata: [{path, size, updatedAt, rev}]. */
  index(prefix?: string): Promise<Array<{ path: string; size: number; updatedAt: number | null; rev: number }>>;
  /**
   * Live row metadata including tombstones; null when the path has no
   * history at all. The read half of the ifRev pattern.
   */
  stat(path: string): Promise<FileStat | null>;
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
    /** Lazy byte access — fetched from the tier on demand. */
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
