# Native iOS client and pairing

Status: **local UI milestone built; production pairing proposed, not implemented**.
The client is in `ios/`, SwiftUI on iOS 26+, with URLSession HTTP/WebSockets.
The app shares wire shapes with Fragment, not its implementation. The local
fixture is a separate Rust development server, not a mobile runtime.

## Problem and acceptance

Make Fragment feel at home on iPhone: open your agent, resume a chat, send a
message, see its reply stream, and open an ordinary app without leaving the
conversation. Follow the later, simpler Finite iOS design and the existing
Fragment mobile colours. The signed-in home is a native fullscreen version
of Fragment's Command-K launcher (Paul, 2026-10-14): one input searches chats,
apps and message text; the first action and Return start a new chat with the
complete input. Search hints alone are capped at eight words and 256 bytes;
older search responses never replace newer results. Empty input stays quiet.
Keep the client minimal: the launcher, chat/app picker, chat and settings. Native controls and typography;
no custom drawer gesture, Rust FFI, embedded chat webpage, model key, or
parallel agent chat protocol.

The first milestone runs locally and spends nothing. Acceptance: real HTTP
and WebSockets, a working composer above the keyboard, whole-text drafts
replaced once by committed replies, Stop, retry with the original id,
reconnect/catch-up without duplicates or regressing either channel's cursor,
earlier history independent of live catch-up, selectable native Markdown,
light/dark, and ordinary fragments in dismissible WKWebView sheets. Build
with Swift 6 strict concurrency and warnings as errors. See `ios/README.md`
for commands and evaluation. The fixture deliberately loses a committed
post's acknowledgement and interrupts a stream to test recovery.

## Existing contract; what is missing

The live service uses these routes already (`docs/api.md` is authoritative):

| Need | Existing API |
| --- | --- |
| Person, owned agent | `GET /api/identities/me`, `GET /api/fragments`, agent's `GET /api/f/{name}/status` for its npub |
| New chat | `POST /api/fragments {label, template:"chat", title}` then `PUT /api/f/{chat}/members/{agent npub} {role:"editor"}`; keep a successful create across a failed join |
| Messages, Stop, prompt answer | `POST /api/f/{chat}/channels/chat {id, body}`, as `docs/chat-records.md` defines |
| Stream/catch-up | `GET /f/{chat}/__live`, subscribe to `chat` and `work` with `last` initially, `after` on reconnect; follow `subscribed.more` until caught up |
| Earlier messages | `GET /api/f/{chat}/channels/chat?before=&limit=50`; never advance live cursors from this page |
| List invalidation | `GET /api/fragments/watch`; refetch the authenticated list on `changed` |
| Launcher search | `GET /api/search?q=` → fragments by title/label and messages by text, archived included; a message result opens its conversation |
| Read marker | `PUT /api/fragments/{chat}/seen`, after the visible chat catches up |
| App URL | `GET /api/f/{name}/status` → `urls.canonical` |

**Authentication is the missing integration.** The existing browser shell
cookie requires same-origin browser Fetch Metadata and an Origin on writes;
a native URLSession cannot honestly manufacture those headers. A native
WebSocket without Origin cannot use browser cookies either. The CLI uses
secp256k1/NIP-98, which would add a signing dependency absent from CryptoKit.
Do not copy a browser's session, bypass Origin checks, or pretend the local
fixture is authenticated production. The platform's model completion route
is for agents; a person's chat app posts chat records and reads its agent's
channel stream, never calls a model directly.

The app's Debug build connects only to the configured development URL.
Release is intentionally gated at the connection screen until this plan is
implemented. The reported deployment is `https://preview.finite.build`,
later `https://finite.chat`; a read-only probe of the preview root on
2026-10-14 returned HTTP 404. Verify the real platform origin and the
canonical fragment origins from deployment configuration before integration.
Never derive fragment origins from a hard-coded suffix in the app.

## Proposed pairing: scan from web Settings

Use the useful device-flow properties of
[RFC 8628](https://www.rfc-editor.org/rfc/rfc8628): short-lived challenges,
explicit user approval, bounded polling and expiring device credentials.
This QR-first variation is a proposal, not a claim of RFC endpoint compliance.
The QR is a rendezvous, not a login credential.

1. **Desktop Settings → Pair iPhone.** An authenticated same-origin POST
   makes a random 256-bit rendezvous id (hash stored in the registry), bound
   to this person and this browser session, expiring after two minutes.
   Show a QR with only a versioned HTTPS pairing link on the platform origin.
   No browser session, device token, private key or durable capability in it.
   Mark the response and page `no-store`; no third-party resources or referrer.
2. **iPhone → Settings → Scan QR.** Use native AVFoundation scanning (with
   an explicit camera permission) and a paste-link alternative. Accept only
   version 1 and an allowlisted platform origin: preview and production are
   distinct accounts. Show the origin and require Connect before contacting
   it. Reject credentials, unexpected paths, fragments, query fields, HTTP,
   lookalike hosts and custom endpoints in release. Debug local connections
   stay separate.
3. The phone creates a random 256-bit verifier in memory, derives its SHA-256
   challenge, and claims the rendezvous with the challenge and a device name.
   The claim atomically binds a fresh pairing attempt to the challenge. The
   response carries a random pending-device id, expiry and a short comparison
   code; it carries no account credentials. Store only hashes of secrets.
   Rate-limit claims and approvals; cap active pairings per person. A claimed
   QR cannot be silently reclaimed by another phone; cancel and regenerate.
4. **Approve on desktop.** Desktop shows the phone name, origin and the same
   comparison code; phone shows the same code while waiting. Only the person
   and browser session that made the QR may approve it, through the existing
   same-origin CSRF boundary. A photographed QR can at most attempt a claim;
   it cannot log in without that explicit approval. Cancellation, expiry,
   another device's claim and denial are distinct outcomes in both UIs.
5. Phone polls a POST status endpoint at the server's interval (at least
   two seconds), sending pending id and verifier in the body, never URL or
   logs. Stop at expiry/background/cancellation; respect 429/backoff. Once
   approved, a verifier-bound one-use exchange atomically mints a device
   session. A lost exchange acknowledgement must be recoverable with the
   same verifier for a short bounded retry window, without creating another
   device or extending expiry (store the encrypted original response for
   that window, then delete it). Parallel exchanges cannot make two sessions.
6. Store the device secret in Keychain with
   [`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`](https://developer.apple.com/documentation/security/ksecattraccessiblewhenunlockedthisdeviceonly).
   No credential in UserDefaults, URL, JS, clipboard, telemetry or backups.
   Store the non-secret deployment origin and device id alongside it. Clear
   only this app's Keychain items on disconnect; clear its caches/web stores.

Proposed platform-only routes (names to settle during implementation):
`POST /api/devices/pairings` (web), `POST /api/devices/pairings/claim`
(phone), `POST /api/devices/pairings/{id}/approve|deny` (web),
`POST /api/devices/pairings/status|exchange` (phone), and authenticated
`GET /api/devices`, `DELETE /api/devices/{id}`. All are bounded, typed,
registry transactions. The phone's verifier and device token are accepted
only by their own endpoints. Browser auth still obeys the current checks.
The settings UI lists each paired device, last use, expiry and Revoke.

## Device API authentication and web sheets

Introduce a narrowly named native-device credential, e.g.
`Authorization: FragmentDevice <opaque 256-bit secret>`. Registry stores its
SHA-256, person, device id, creation/expiry/revocation. A device is a person's
session, not a new identity; membership and billing remain exactly the same.
Set a 30-day absolute expiry initially, matching browser sessions; renew only
through an explicit authenticated exchange, or pair again. Every HTTP
request rechecks revocation. WebSocket upgrades accept this header through
the existing principal resolution, recheck on ping/activity within the
existing live identity window, and close on revoked access. Do not make
browser cookies usable without browser-origin checks. No broad CORS change.

Use an ephemeral URLSession scoped to the platform origin. Add device auth
only to that exact origin, refuse authenticated cross-origin redirects and
never put a credential in the WebSocket query. Reconnect with fresh headers
and the last `subscribed`/record cursor for each channel. On 401, stop retrying,
clear session material and show Pair again. Keep pending post ids and bodies
in a bounded local outbox **before** sending; preserve drafts and ambiguous
sends across navigation, backgrounding and restart. Never auto-replay a user
message with a new id. If a create's response is ambiguous, reconcile the
person's fragment list before making another chat; a known created fragment
is reused across a failed membership call.

For ordinary fragments, add a device-authenticated **top-level** scoped web
redemption endpoint, mirroring `/auth/fragment` (60 seconds, single use,
one fragment and parent device session, live membership checked). The phone
requests a redemption via URLSession, then loads its `__signin` URL in that
sheet's ephemeral WKWebView. Only the fragment session cookie reaches that
origin. No native auth header or long-lived platform cookie enters WKWebView;
never inject credentials into JavaScript or a page. Redemptions must not use
`/auth/frame` because these are top-level webviews, not platform frames.
Revoking the device invalidates all derived fragment sessions. Respect the
existing ask-first boundary for unfamiliar fragments. Restrict the initial
redemption's target to the exact canonical origin supplied by the platform,
allow normal same-origin navigation and send unrelated external links to
the system browser. In-app page errors and retry remain native.

## Phases and release gates

1. **Local UI — this change.** Native screens and real fixture transport;
   simulator tests for ordinary and recovery paths; manual light/dark,
   keyboard, large text and web sheet inspection. No backend auth changes.
2. **Platform pairing.** Registry pairing/device state, hash-only long-lived
   credentials, explicit web approval, revocation, device resolution and
   scoped top-level web redemptions. Update `api.md`, the identity truth map,
   finite integration row and special-case inventory together with that code.
   Add host tests for valid/invalid/replay/restart and run the auth/keys/
   identities/share/isolation/live e2e sections. Platform consent is not a
   template operation and cannot be controlled by an app fragment.
3. **Native integration.** Native scan/paste/approval states, Keychain,
   scoped URLSession, authenticated WebSocket, list watch, read markers,
   durable outbox/drafts, and real fragment-sheet redemption. Preserve this
   UI and reducer; replace the fixture connection boundary. Add native prompt
   responses and attachments before declaring web-chat parity.
4. **Preview canary.** An authorized test account on the actual preview
   deployment: scan, approve, open existing chat, create one, stream a reply,
   Stop, background/foreground, force-close/restart, revoke this device, open
   a private app sheet, and prove another person's app stays inaccessible.
   No deployment or paid call is authorized by this document. Coordinate the
   account and budget with Paul before this phase. Then physical-iPhone QA.
5. **Production.** Configure `finite.chat` only after the deployment exists;
   pair again for that origin. Signing, team/bundle id, camera purpose string,
   associated domains, app privacy disclosures, app icons and accessibility
   QA precede TestFlight. Distribution and production deploys are Paul's.

Pairing tests must include expired/denied/cancelled QR, copied QR without
approval, wrong browser/person/verifier/origin, repeated claim and exchange,
lost successful exchange response, restart during approval, token expiry,
revocation during HTTP and streaming, revoked derived web sessions, membership
revocation, cross-origin redirect refusal and no secret leakage. Repeat
ordinary chat tests with duplicated records, multipart drafts, independent
channel sequences, stale drafts, interrupted page catch-up, ambiguous sends,
changed-body retry (409), and a lost response across app restart. Visual QA
covers a compact iPhone, landscape/iPad, keyboard, large text, light/dark,
VoiceOver, Reduce Motion, long code and long history.
