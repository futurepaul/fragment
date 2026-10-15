# Fragment for iOS

A native SwiftUI preview of Fragment's focused mobile experience. iOS 26+,
Swift 6, Apple frameworks only. No packages, Rust bindings, shared runtime,
or web chat UI. Only ordinary app fragments open in a WKWebView sheet.

## Run locally

On the Mac, from the repository root:

```sh
cargo run --locked --manifest-path ios/dev-server/Cargo.toml --target-dir target/ios-fixture
open ios/Fragment.xcodeproj
```

Choose the **Fragment** scheme and an iPhone Simulator, then Run. The
checked-in Xcode project needs no generator or dependency installation.
Debug builds default to `http://localhost:8910`. Settings → Local development
can change it; `--server http://localhost:8910` overrides it for UI tests.
Release builds cannot connect to the fixture and show the pairing preview
until production device authentication is built. No fake production login.

The Rust server is a separate, memory-only development fixture. It never
calls a model, deployment, account, or Docker. Stop and restart it to reset.
It speaks the existing Fragment API shapes for lists, status, chat creation,
agent membership, channel posts, backwards history, and `__live` WebSockets.
The server is not part of the app binary or the root Cargo workspace.

For a physical phone, explicitly bind the fixture to the LAN and set the
canonical URL to the Mac's Bonjour name (do this only on a trusted network;
the fixture has no authentication):

```sh
FRAGMENT_IOS_LISTEN=0.0.0.0:8910 FRAGMENT_IOS_BASE_URL=http://YOUR-MAC.local:8910 cargo run --locked --manifest-path ios/dev-server/Cargo.toml --target-dir target/ios-fixture
```

Set that same URL in the app's development settings. Loopback is the default.

## Scope

- Signed-in home is a native fullscreen Command-K launcher: search chats,
  apps and message text through `GET /api/search`, with a new chat as the
  first action. Return sends the complete input to your agent in a new chat.
  Search is debounced; its eight-word / 256-byte hint never truncates the prompt.
- Native message composer, interactive keyboard dismissal, selectable text,
  Markdown paragraphs, headings, lists, quotes, links and fenced code.
- Real HTTP sends and live whole-text drafts, replaced by durable records.
- Stop, same-id retry, independent chat/work cursors, paged catch-up,
  reconnect with capped backoff, and foreground resumption.
- Earlier history loads separately from live cursors; reading older messages
  does not force scrolling down when a reply arrives.
- Native sheets for settings and WKWebView apps; separate ephemeral web stores.
- System light/dark, Dynamic Type, SF Symbols, VoiceOver labels and native
  navigation. Colours and icon come from Fragment's mobile shell.

This is the local UI milestone. Production authentication, QR scanning,
push, attachments, interactive agent approval cards, full Markdown tables,
and durable offline storage are outside this milestone. Unknown records do
not break decoding; an approval card directs you to the web. Messages and
unsent posts live in the chat view's memory; keep that chat open to retry a
failed send. The production gates are in [the pairing plan](../docs/ios.md).

## Verification

With the fixture running:

```sh
xcodebuild -project ios/Fragment.xcodeproj -scheme Fragment -destination 'platform=iOS Simulator,name=iPhone 17,OS=26.5' -derivedDataPath target/ios -parallel-testing-enabled NO test
cargo clippy --locked --manifest-path ios/dev-server/Cargo.toml --target-dir target/ios-fixture -- -D warnings
```

`TimelineTests` cover duplicate delivery, numeric ordering, independent
cursors, historical insertion, whole-text drafts, replacing only an author's
draft, multipart replay and turn completion. `TransportTests` use real HTTP
and WebSockets: stream, Stop, forced disconnection/catch-up, same-id replay
after a lost acknowledgement, conflicting body rejection, and exclusive
history cursors. `FragmentUITests` drive home → send → streamed text → Stop
→ app sheet → earlier history in Simulator. `MarkdownTests` exercise a code
fence arriving incrementally. These tests require no paid calls.

Fixture prompts: `/disconnect` cuts the live socket once mid-reply;
`/fail-once` returns HTTP 503 **after committing** the message, so replay
must not append or run the agent twice; ask for `code` to exercise Markdown.
A matching live record acknowledges a post even when its HTTP response is
lost. To try the visible retry UI, disconnect the server while a send is in
flight, restart it and tap the failed post (the fixture resets on restart).

The source of the generated project is `project.yml`. If changing project
structure, use XcodeGen 2.42+ and commit its result:

```sh
xcodegen generate --spec ios/project.yml
```

## Design references

The second Finite iOS design is the retired `finitechat/ios/Sources/FocusedAppShell.swift`
at `finite-mono` commit `a1d5089c^`: focused home, three recents, one agent,
and chat selection. Its original brief is the Mac Codex archive for
`/Users/futurepaul/dev/finite/finite-mono`, session
`019f9006-c9e6-7f90-831e-7c6ae6f3ace7`, source JSONL line **9**, July 23,
2026 (shared Linux history archive; existing index, not refreshed).
The signed-in home now follows Fragment's own fullscreen Command-K launcher
(`cell/shell/shell.js`): search everything and default to starting a new chat,
per Paul's direction on 2026-10-14, replacing the older three-recents home.

[Enchanted](https://github.com/gluonfield/enchanted) supplies a native iOS
chat reference for conversation history, Markdown and a configurable server.
[Claude's iOS app](https://claude.com/resources/articles/team-plan-and-ios)
is a reference for a focused conversation-first mobile experience. No source
code was copied. SwiftUI's native scrolling follows
[Apple's scroll geometry API](https://developer.apple.com/documentation/swiftui/view/onscrollgeometrychange(for:of:action:)).
