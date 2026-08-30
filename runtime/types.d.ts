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
