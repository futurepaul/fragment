# Screen streaming: a computer's screen as live video

Status: **proposed 2026-09-26.** Research and design only; nothing is
built. It answers "stream video directly like Hermes does" for the pet
(`templates/pet`), and for any fragment whose computer has a screen.

Today the pet writes a JPEG through the `frame` mutation, about once a
second while driven and once each 5 seconds otherwise, and input goes
through the `control` channel to xdotool. A click takes a second or two
to show, and the mutation ledger fills the app's 16 MiB database within a
day of nonstop driving (docs/computers.md).

## Recommendation

**VNC, relayed by the fragment, with the Sprite dialing out.** noVNC in
the page speaks RFB over a new `__screen` socket on the fragment's own
origin. The fragment's cell checks the viewer's role and relays the
bytes. On the Sprite, TigerVNC's `Xvnc` is the display, and a `fragment
screen` bridge (the CLI, signed with the computer's own key) dials the
same fragment and carries each viewer's session to Xvnc's Unix socket.
No Sprites token, no Sprite URL, and no celld fork change. This is
Hermes's design, with the platform as its gateway.

## Facts checked (2026-09-26)

- **Sprite URL:** "private by default. It's reachable only by members of
  your org, through the browser or with an org token"; public means "no
  authentication". HTTP(S) only, to one service's port (docs.fly.io/
  sprites: concepts/networking, concepts/services).
- **Tokens** are org-wide (`<org>/<id>/<secret>`, a Bearer header); the
  OpenAPI spec has no scoped or per-Sprite token (api/openapi.json).
- **Sprites' own tunnel:** `wss://api.sprites.dev/v1/sprites/{name}/proxy`
  takes the org token, then `{host, port}`, then raw bytes
  (api/websockets/tcp-proxy). The docs index (docs.fly.io/llms.txt) lists
  no desktop, display, or VNC feature.
- **Awake:** activity includes "Open TCP connections (like your app's
  URL)" (working-with-sprites). Outbound ones do not count: the Tasks API
  is for "Anything holding outbound connections: websockets", and "Open
  TCP connections drop on the pause, even on warm" (keeping-sprites-
  running). Idle is about 30 s; a warm wake 100–500 ms, cold 1–2 s
  (concepts/lifecycle). Egress is unrestricted without a network policy.
- **Packages:** Sprites run Ubuntu 25.10, whose archive has TigerVNC
  1.15.0 (May 2025) and ffmpeg 7.1.1 (Launchpad).
- **A cell can dial a socket with a header:** `fetch(url, {headers:
  {Upgrade: "websocket", …}})` (celld fork, `crates/celld/js/
  websocket.rs`, `op_ws_upgrade`). Such a socket "keeps its cell
  resident" and closes when the cell moves (celld `docs/limitations.md`).
- **KEYS cannot hold one:** the fork's seam passes whole bodies ("a
  native service takes a whole request body, not a stream"; its answer
  sets `websocket: None`: `crates/celld/main/native_seam.rs` at the pinned
  `f734f8f`). So a cell proxies a socket with the Sprites token only by
  holding the token, which H1 forbids.
- **The Fragment cell** tags a computer's socket `computer`: no page, so
  it neither wakes nor holds the computer (`cell/src/live.rs`). It
  ignores binary messages today (`fragment.rs`, `websocket_message`).
- **noVNC 1.7.0** (2026-04-28, npm and GitHub): MPL-2.0, ES modules
  (`core/rfb.js`), no dependencies; `core/` and `vendor/pako` are 54
  files, about 700 KB; `viewOnly`, `scaleViewport`, continuous updates.
- **WebCodecs** video: Chrome 94, Firefox 130, Safari 16.4; not Firefox
  for Android (caniuse). **Fly egress:** $0.02/GB in North America and
  Europe (docs.fly.io/about/pricing).
- **Hermes** (Nous Research's Bot Screen, September 2026): a TigerVNC
  `Xvnc` per profile on a 0600 Unix socket, noVNC in Hermes Desktop, and
  a socket `/api/display/ws` opened with a single-use ticket from the
  gateway, which "drops keyboard, pointer and clipboard messages from any
  viewer that does not hold the lease, at the RFB byte level"
  (hermes-agent docs, features/bot-screen; PR #124308).

## The options

Latency and bandwidth are estimates, not measurements. The Sprites API
has no region field, so the distance from a Sprite to our Machines in
`ord` is the first smoke's to measure.

**1. VNC relayed by the fragment, the Sprite dialing out (recommended).**
- *Reach:* the page opens `wss://<fragment>/__screen`. The router's socket
  rule applies (`Origin` must be the fragment's own; no `Origin` counts
  only a signature). The cell requires a signed-in viewer. The bridge
  opens `/f/<name>/__screen` signed with the computer's key, taken only
  from this fragment's own computer. The Sprite's URL stays private and
  unused.
- *Framing:* page ↔ cell is plain RFB, so stock noVNC works. Cell ↔
  computer is one socket: binary messages carry a 4-byte session id;
  text frames say `open {session, principal}` and `close`.
- *Input:* RFB carries pointer, wheel, and keys; noVNC maps the keyboard.
  Every signed-in viewer drives, as today. Xvnc runs with
  `-AcceptCutText=0 -SendCutText=0 -AcceptSetDesktopSize=0`: every viewer
  would see a driver's clipboard, and none should resize the screen.
- *Isolation:* the fragment's origin and `__live`'s socket rules; inside
  the desktop's `__frame`, the frame session counts on an own-origin
  socket. noVNC comes from the fragment's `site/`. No new origin, frame,
  cookie, or CDN; the browser never learns the Sprite's name or address.
- *Awake:* a viewer's screen socket is a page: its open wakes the
  computer, and the last page's close starts the 5-minute wait. The
  cell's Tasks hold keeps the Sprite up; when it pauses, the bridge's
  socket drops, and it reconnects on wake. RFB's server speaks first, so
  a viewer's socket just waits for the computer (60 s at most, then 1013).
- *Latency:* two hops through `ord`, about 100–250 ms click to pixel in
  the US. *Bandwidth* per viewer: near zero idle, tens of KB/s typing,
  0.5–2 Mbit/s scrolling, 3–8 Mbit/s for full-motion video. Each viewer
  requests its own updates, so a slow one slows only itself and the relay
  queues nothing. An hour at 1 Mbit/s is 0.45 GB: $0.009 of egress, beside
  $0.0726 of awake time.
- *Where:* celld routes `__screen` to the Fragment cell, which relays; the
  Sprite runs Xvnc, openbox, Chromium, and `fragment screen`.
- *Size:* cell about 220 lines, proto 40, CLI 180, e2e 250. The pet loses
  `app.mjs`, `frame`, `screen`, `control`, the capture loop, and the
  xdotool follower (about −200).
- *Dependencies:* noVNC 1.7.0 (five months old) and TigerVNC 1.15.0 from
  apt. No new crate (the CLI has tungstenite 0.29).

**2. VNC through the Sprite's URL or Sprites' tunnel (dialing in).**
Browsers cannot reach a private Sprite URL, and a public one is open to
anyone, so KEYS would dial in with the org token and hand the socket to
the cell. That is new websocket plumbing in the fork, far past its seam.
The cell relays; the Sprite runs Xvnc and nothing else. Input, isolation,
and latency match option 1. One gain: an inbound connection counts as
activity, so the stream holds the Sprite by itself. Size: option 1
without the bridge, plus 100–200 lines in the fork and KEYS. The fallback
if the Tasks hold proves unreliable.

**3. H.264 over the same relay, decoded with WebCodecs.** The Sprite
encodes once (ffmpeg 7.1.1 from apt: x11grab, libx264 `zerolatency`, a
keyframe each 2 s); the cell fans the bytes out and keeps the last
keyframe group for joiners. Reach, isolation, and awake match option 1.
Input becomes ours: events as JSON, role-checked by the cell, applied by
a persistent `xdotool -`; key mapping, IME, and clipboard are ours to
write. About 0.3–1.5 Mbit/s at 15–30 fps, better than VNC for motion; the
cell must skip a slow viewer to the next keyframe. About 550 lines before
tests. The upgrade if motion matters; the relay and awake carry over.

**4. JPEG frames over the relay.** Today's capture, sent on the socket
instead of `frame`: no ledger, input still on `control`. At 40–85 KB a
frame, 5 fps is 1.6–3.4 Mbit/s per viewer, and each ImageMagick capture
takes roughly 100 ms. About 150 lines, but not video.

**5. WebRTC.** A streamer on the Sprite (GStreamer's `webrtcbin` from apt,
or a Pion program such as neko), signaling over the fragment, where the
role check happens. Sprites document no inbound UDP, so media needs a
TURN relay both ends reach over TCP/TLS: a new service to run, with
per-viewer credentials the platform mints. Best latency (50–150 ms) and
congestion control, but an encode per viewer without an SFU, and 600+
lines plus the service. Not now.

## The first slice

1. **`__screen` in the Fragment cell:** viewer sockets (own origin,
   signed in, viewer or more) and the source socket (this fragment's
   computer). Relay by session id; viewer sockets count as pages. Limits:
   8 sessions a fragment, 4 a principal, 256 KiB a message.
2. **`fragment screen <name> --vnc <socket>`** in the CLI: the signed
   source socket, reconnecting with backoff; one Unix connection to Xvnc
   per session.
3. **The pet:** `Xvnc :99 -geometry 1280x800 -rfbport -1 -rfbunixpath
   ~/.pet/vnc.sock -SecurityTypes None` and the flags above replace Xvfb;
   openbox and Chromium stay. The page runs noVNC's `RFB` on `./__screen`
   with `scaleViewport`, reconnecting while open. Anonymous visitors see
   "Sign in to watch".
4. **e2e `screen`:** a scripted RFB server behind the fake computer's
   bridge; a viewer gets its version and one update. Refused: another
   origin, an anonymous visitor, another computer, a person as the
   source. A screen socket wakes the computer; its close starts the wait.
5. **The smoke on fragment.boats:** first pixel on warm and cold wakes,
   click to pixel, bytes idle and scrolling, whether the Tasks hold keeps
   the Sprite up while it streams, and `__op` latency meanwhile. If the
   relay slows the Fragment cell, move it to the `Computer` cell.

Slice 2 lets anonymous link holders watch: the cell passes only view
messages (SetPixelFormat, SetEncodings, FramebufferUpdateRequest,
EnableContinuousUpdates, Fence) from a non-driver and closes a session
that sends anything else, as Hermes's gate does (about 120 lines in
`fragment_core`, host-tested).

## Questions for Paul

1. Slice 1 shows the screen only to signed-in viewers; today anonymous
   link holders watch. Wait for slice 2?
2. noVNC as published (54 files, 700 KB, MPL-2.0) in the pet's `site/`,
   or served by the platform (`__novnc/`) for any fragment?
3. Clipboard off for everyone, since viewers share one screen?
