// Workers-runtime globals the DOM lib doesn't know about.
// Zero-copy pass-through stream (workerd): minting a body this context OWNs
// out of one it doesn't (see tierPlaceFromUrl).
declare class IdentityTransformStream {
  constructor();
  readonly readable: ReadableStream;
  readonly writable: WritableStream;
}

declare class WebSocketPair {
  constructor();
  0: WebSocket;
  1: WebSocket;
}

interface ResponseInit {
  webSocket?: WebSocket | null;
}

interface WebSocket {
  serializeAttachment(data: unknown): void;
  deserializeAttachment(): unknown;
}

// Native Workflows (celld / Cloudflare-compat). Minimal ambient shape for
// the subset fragment uses: WorkflowEntrypoint + step.do/step.sleep +
// instance create/get/status. See docs/explorations/celld-cloud-run-demo.md
// and celld's cloudflare-compat notes (1 MiB step-result/event/param cap;
// replays rerun non-step code; a crashed step re-runs its callback).
declare module "cloudflare:workers" {
  abstract class WorkflowEntrypoint<C = unknown, P = unknown> {
    readonly env: C;
    readonly ctx: unknown;
    constructor(ctx: unknown, env: C);
    run(event: P, step: WorkflowStep): Promise<unknown>;
  }
  interface WorkflowStep {
    do<T>(name: string, fn: () => Promise<T>): Promise<T>;
    do<T>(name: string, opts: { retries?: { limit?: number; delay?: string | number; backoff?: string } }, fn: () => Promise<T>): Promise<T>;
    sleep(name: string, duration: string | number): Promise<void>;
    sleepUntil(name: string, timestamp: number | Date): Promise<void>;
    waitForEvent(name: string, opts: unknown): Promise<unknown>;
  }
  interface WorkflowInstance {
    id: string;
    status(): Promise<{ status: string; output?: unknown; error?: unknown }>;
    pause(): Promise<void>;
    resume(): Promise<void>;
    restart(options?: { from?: { stepName?: string; count?: number; type?: string } }): Promise<void>;
  }
  interface WorkflowCreateOptions {
    id?: string;
    params?: unknown;
  }
  interface WorkflowBinding {
    create(opts: WorkflowCreateOptions): Promise<WorkflowInstance>;
    get(id: string): Promise<WorkflowInstance>;
  }
}

// celld Queues (Cloudflare-compat shape, subset): producer side only in
// the runtime — the consumer is the notify-relay script.
interface MessageBatch<T = unknown> {
  messages: Array<{ id: string; body: T; ack(): void; retry(opts?: { delaySeconds?: number }): void }>;
  retryAll(opts?: { delaySeconds?: number }): void;
  ackAll(): void;
  queue: string;
}
interface QueueBinding<T = unknown> {
  send(message: T, opts?: { delaySeconds?: number }): Promise<void>;
  sendBatch(messages: Array<{ body: T; delaySeconds?: number }>): Promise<void>;
}

