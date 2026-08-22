// Workers-runtime globals the DOM lib doesn't know about.
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
