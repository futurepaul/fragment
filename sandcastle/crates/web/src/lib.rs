//! A browser's way to a sandcastle computer by its key (fragment-next
//! docs/runtime-seam.md): a throwaway iroh key, a connection through the
//! computer's relay (browsers have no UDP, so always relayed), an admission
//! on its first stream, then HTTP/1.1 and WebSockets each on a stream of
//! their own, as `sandcastled`'s iroh endpoint serves them. Nothing here is
//! a platform's: a page gets its admission from whoever may give one.
//!
//! From JavaScript:
//!
//! ```js
//! const peer = new Peer(); // its key, for asking an admission: peer.id()
//! const computer = await peer.connect(endpoint, relay, admission, host);
//! const { status, headers, body } = await computer.fetch("GET", "/api/status", "{}", new Uint8Array());
//! const socket = await computer.websocket("/api/ws", []);
//! await socket.send(text); // sendBytes(bytes) for a binary message
//! const message = await socket.next(); // a string, a Uint8Array, or undefined at its end
//! const screen = await computer.screen("/api/display/ws?display_ticket=…", (e) => draw(e));
//! await screen.pointer(x, y, 1); await screen.key("a", true);
//! ```
//!
//! A computer's screen (its RFB, as Hermes' serves it) runs over one of
//! those WebSockets: `rfb` is its protocol, pure, and `Screen` its page's
//! side.

pub mod rfb;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
