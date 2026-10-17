# The owner's browser (spike)

Exploration, 2026-10-16, on branch `claude/owner-browser` from master at
`ddd88f6f`. Austin asked for it on 2026-10-16: "I want to explore the
possibility of allowing an agent to access your browser on your machine
… I think Chrome extension is the right call." Nothing here is decided:
the branch's draft PR is a spike, not to be merged. Where this file
disagrees with docs/cloudflare-v1.md, that file wins until Paul decides.
An agent driving its owner's signed-in browser is privacy posture, which
is Paul's (docs/cloudflare-v1.md, "Escalations"); "Questions for Paul",
below, lists what is his to say.

Every claim is marked with how it was checked:

- **run**: done in this spike. The command that reproduces it is given.
- **read**: read in a vendor's documentation or source, at the place named.
- **inferred**: follows from what was run or read, but was not seen happening.
- **unknown**: needs a vendor's answer or a hosted run.

All runs were local, on an Apple Silicon Mac: workerd under `wrangler
dev`, the stub image and our Hermes image (v0.21.7, amd64 under
emulation) in Docker, and Chromium 148 with the extension loaded
unpacked. No hosted deployment was used.

## The short version

- **What it is.** A person's agents use the person's own browser, with
  its sign-ins. What they may use of it is the person's choice in the
  extension: only the tabs they open (the default), those and tabs the
  person chooses, or all the person's tabs and windows. The browser
  connects through the extension, and the person's computer carries the
  Chrome DevTools Protocol (CDP) between that browser and its guest. The agent's
  runtime needs nothing new: any CDP client works, Hermes' browser tools
  and the agent-browser CLI in our image among them. While the browser
  is connected, the bridge tells the agent so, in one line beside each of
  its owner's messages: no skill.
- **It fits the design as it is.** The platform gains no notion of an
  agent runtime, a vendor, or CDP's meaning: it carries frames.
  - The guest reaches it the way it reaches the API and models: one more
    egress intercept, `browser.fragment.internal`.
  - The owner's browser reaches it the way a browser reaches the
    computer's ports: a one-time ticket the owner mints, spent on the
    computer's own origin.
  - The Computer DO pairs the two sockets, as it already holds the
    keepalive.
- **It works end to end locally (run).**
  - In a Chrome with the extension, Settings connects the browser once,
    and lists it by name. The agent (the stub image's `browse`) opens a
    tab in it and reads its title. A dropped connection comes back on
    its own. Forget, in Settings, ends it.
  - Our Hermes image, unchanged, does the same: its terminal tool runs
    the agent-browser on its PATH against the socket, and the tab opens
    in the extension's Chrome.
  - agent-browser 0.26.0 (Hermes' pin) drives the extension through
    open, title, snapshot, screenshot, a second tab and a click. At "All
    my tabs and windows" it lists the person's tabs, switches to one,
    reads it, closes a duplicate, and opens a new window.
  - On the dev stack, a person tries it as written under "Trying it
    locally", below.
  - **A real agent, in plain English.** Hermes on a real model, told by
    the bridge's line, used the owner's browser the first time it was
    asked: it listed the tabs it may use, and duplicated the person's
    tabs when asked to. An earlier trial, with a skill instead, searched
    Google Flights in Austin's browser. A fresh agent told nothing used
    the Chromium on its own desktop (below).
- **Not run:** anything hosted.

## The shape

```
the guest (any CDP client)            the owner's browser (Chrome + extension)
  ws://browser.fragment.internal         ws(s)://<computer origin>/__browser?t=<ticket>
      /devtools/browser                              │
          │                                          │
          └──────────── the Computer DO ─────────────┘
                 one socket for the browser, one per guest socket;
                 each frame `<socket> <CDP message>`, or `<socket> close`
```

### The owner's side (run)

- `POST /api/computers/{id}/browser/ticket`, its owner alone (anyone else
  finds no computer: 404) → `{url, expiresAt}`. The URL is a socket on
  the computer's own origin, `/__browser?t=<ticket>`: 24 random bytes, kept
  hashed, spent once, within two minutes, as a port's ticket is.
- The socket opens only from an extension: its `Origin` must be
  `chrome-extension://` and an id (32 letters `a` to `p`). A page holding
  a ticket is refused (403); a socket that names no ticket too (400); a
  ticket spent before (401).
- **Connect once.** That first connection makes the browser a *link*,
  sent once as the new socket's first frame:
  `{"type":"link","id":…,"link":…}`.
  - The link is 32 random bytes, kept hashed beside its id, the name the
    extension gives it (`{"type":"hello","name":"Chromium on macOS"}`),
    and when it was made and last heard.
  - With it the extension comes back on its own. It asks the computer's
    origin for a ticket of its own (`POST /__browser/ticket`, from an
    extension's origin, `{"link": …}` as the body) and connects with
    that, at once, then after 1, 5 and 15 seconds, then each minute (an
    alarm, which wakes its worker).
    - A dropped socket (run: a test lever drops it as a network would).
    - Chrome or its worker starting again (inferred: the worker resumes
      on `chrome.runtime.onStartup` and whenever it starts).
  - The link goes in the body because an `Authorization` header on a
    computer's origin is a signer's: the router verifies it as NIP-98
    before the computer sees it (run).
  - At most 8 browsers stay linked: making a ninth forgets the one heard
    from longest ago.
- **One browser at a time.** The newest replaces the one before it,
  closed `4001` ("another browser connected"), and every guest socket the
  older carried ends. The DO keeps which one is current
  (`MetaKey::BrowserOwner`), so the older one's late close ends nothing
  of the newer's.
- **Forgetting.** Its owner forgets a browser from any device
  (`DELETE /api/computers/{id}/browser/links/{link}`: Settings' Forget),
  or the extension forgets itself (`POST /__browser/forget`: its own
  Disconnect). Its link is gone, and its socket is closed `4002`.
- **Coming back, or not.** `4001`, `4002` and `4003` (its owner wiped)
  tell the extension not to come back; any other end, it does
  (`fragment_core::owner_browser::comes_back`, host-tested). The
  extension keeps the same list.
- **Told.** The computer's view lists its browsers (`browsers`: id, name,
  connected, last heard). Its owner's open pages are told when that
  changes, and Settings re-reads it when this browser's extension
  reports a change.
- The extension pings every 20 seconds (`{"type":"ping"}`, answered
  `{"type":"pong"}`). Chrome keeps an extension's worker alive while its
  socket carries a message every 30 seconds (read: Chrome's "Use
  WebSockets in service workers").
- A wipe of the owner closes both sides.

### The guest's side (run)

- `ws://browser.fragment.internal/devtools/browser` (or any visible path
  below it, as a browser's DevTools names one), told to the guest as
  `FRAGMENT_BROWSER=http://browser.fragment.internal`. A client given the
  full URL connects as given, with no `/json/version` lookup first.
- `GET /json/version` there answers as a browser's DevTools does, from
  the Computer DO: the connected browser's name (`Browser`) and its
  socket (`webSocketDebuggerUrl`), or `not_connected`.
- **Told.** Before each turn its owner asks, the bridge asks
  `/json/version` (at most 2 s, beside the note's reads), and while a
  browser is connected the agent is told so in one line beside the
  message: which browser "my browser" means, how its terminal reaches
  it, and what it may use (the appendix; docs/bridge.md, "Its owner's
  browser is told"). Another asker's turn is never told, the browser
  being the owner's; nothing is told while none is connected.
- Accepted only while the owner's browser is connected. Otherwise 403
  `not_connected`: "your owner's browser is not connected: they connect it
  from Settings, with the Fragment extension". The stub's reply carries
  those words (run).
- At most 32 guest sockets at once (`BROWSER_GUESTS_MAX`: a browser
  tool's own and a supervisor's beside it, for each of 16 agents).
- Any agent on the computer may use it. One person's agents share their
  computer and are not fenced from each other (decision 44), so a
  per-agent list would be no boundary (inferred).

### What the DO carries (run)

- A frame from a guest socket goes to the browser as `<socket>
  <message>`: `<socket>` is 8 hex digits the DO picked for that guest
  socket, and `<message>` is the CDP message as the guest sent it.
- A frame from the browser, `<socket> <message>`, goes to that guest
  socket as `<message>`; `<socket> close` closes it.
- A guest socket that ends is told to the browser as `<socket> close`.
- The rules are `fragment_core::owner_browser`, host-tested: what a frame
  may say, what path a guest may name, which origin may connect. The DO
  reads no CDP.

### The extension (run)

`extension/`, Manifest V3:
- a worker: `background.js` wires `link.js` (the computer's socket and
  this browser's link, coming back on its own), `cdp.js` (the guest's
  CDP, answered) and `access.js` (what the agent may use);
- a content script on the platform's page (`platform.js`);
- a popup, its settings.

- **Connecting.** Settings' Computer section has a "This browser" line.
  - Settings' Computer section lists "Browsers": each one linked, by its
    name, connected or when it was last heard, with Forget. This browser
    is marked "(this browser)".
  - With the extension, it offers "Connect this browser". The page mints
    a ticket and hands it to the content script by a window message, and
    the worker opens the socket.
  - The extension says which computer and which link it holds. Settings
    calls this browser connected only on the person's own computer; for
    any other it offers Connect, which replaces it.
  - The popup says "Reconnecting to your computer…" while the extension
    comes back with its link. Its Disconnect forgets the browser.
  - Without the extension, the line says to add it.
- **What an agent may use: the person's choice.** The popup is the
  extension's settings ("What your agents may use"). It offers three
  levels:
  - "Only the tabs they open", the default: the agent's own tabs,
    grouped as "Agent" (inferred: every run was headless, which has no
    tab strip).
  - "Those, and tabs I choose": the popup lists the person's open tabs,
    each with a switch.
  - "All my tabs and windows": every tab in every window, to list, read,
    drive, switch to and close. A new window is the person's too: an
    extension can make no browser profile apart from theirs, so
    `Target.createBrowserContext` answers with a new window.

  A tab outside the choice is never listed, attached or touched.
  Narrowing the choice detaches the agent at once from what it may no
  longer use. Only the extension's own popup sets the level, never a
  page. The level is kept across restarts; the agent's tabs and the
  chosen ones for the browser's session (`chrome.storage.session`), so a
  worker Chrome stopped while the computer was away (a laptop's sleep, a
  long outage) still knows them.
- **The browser's own methods** are answered by the worker:
  - `Target.*`: `setDiscoverTargets`, `getTargets`, `getTargetInfo`,
    `createTarget`, `attachToTarget` with flattened sessions,
    `detachFromTarget`, `activateTarget`, `closeTarget`, `setAutoAttach`,
    `createBrowserContext` and `disposeBrowserContext`;
  - `Browser.getVersion` and `Browser.getWindowForTarget`.

  Anything else at the browser's level is refused by name.
- **A page's methods** go to its tab through `chrome.debugger`, and its
  events come back on each session attached to it.
- **Seen and stopped (inferred: headless Chrome has no bar).** Chrome
  shows its own bar, "Fragment started debugging this browser", while a
  debugger is attached. The extension detaches once no guest socket is
  left, and the person's closing that bar disconnects everything.

## Evidence

### What agent-browser asks of a browser (run)

agent-browser 0.26.0 is the version Hermes v0.21.7 pins in its
`pm/lock.json`, which our image installs (read). Its tarball's SHA-256
matched the lock's. Its CDP was logged through a proxy in front of a
headless Chromium of the spike's own, on a port checked to be that
Chromium's. The commands were `open`, `get title`, `snapshot`, `click`,
`screenshot`, `tab new` and `tab list`, then `close`.

- At the browser's level it asked only `Target.setDiscoverTargets`,
  `Target.getTargets`, `Target.attachToTarget` (flattened),
  `Target.createTarget` and `Browser.getVersion`.
- Everything else was on a page's session: `Page.*`, `Runtime.*`,
  `Network.enable`, `DOM.enable`, `Accessibility.*`, `Input.*`, and
  `Target.setAutoAttach`.
- `close` sent no `Browser.close`: connected over CDP, it only lets go.

### agent-browser through the extension (run)

A stand-in relay framed sockets as the DO does. It was a throwaway
script, not in the repo. Chromium 148 was headless with the extension
loaded, and a page on `127.0.0.1` handed the extension its ticket
through the content script. That also showed the manifest's
`http://127.0.0.1/*` matches a page on any port. agent-browser 0.26.0
with `--cdp ws://…/devtools/browser`:

- `open`, `get title`, `snapshot`, `screenshot`, `tab new`, `tab list`
  and a `click` that navigated all worked;
- `tab list` showed the agent's two tabs and not the page the test had
  opened outside it.

Then the person's own tabs: two duplicates of one page, a stand-in, and
a New Tab, with the extension set to "All my tabs and windows" through
its popup.

- `tab` listed them as `t1` to `t4`, but not Chrome's own New Tab page
  (inferred: `chrome.debugger` cannot attach to a `chrome://` page, so
  Chrome does not offer it as a target).
- `tab t4` switched to the second duplicate, and `get title` read it.
- `tab close t4` closed it.
- `window new` opened a new window of the person's, which joined the
  list as `t5`.

### The platform, end to end (run)

`CHROME_BIN=<Chromium 148> cargo xtask e2e --only owner-browser`:
36
passed, 0 failed, 0 skipped. The section takes about 11 s after the
build (the cell, the CLI and the stub image), and no wait runs out its
limit. Its checks (`crates/e2e/src/lanes/owner_browser.rs`):

- **Valid.**
  - The owner's ticket is a socket on the computer's own origin.
  - Its first connection makes the browser a link, given once on its
    socket. The name the extension says is what its owner's view shows,
    connected.
  - The stub's `browse` reaches a fake extension's socket as
    `Target.createTarget`, `Target.attachToTarget` and
    `Runtime.evaluate`, all named by one guest socket.
  - The answers reach the guest (the agent's reply reads the page's
    title), and the guest socket's end is told to the browser
    (`<socket> close`).
  - While it is connected, the owner's turn is told so first: the
    browser's name and its socket, as the computer answered
    `/json/version` (the stub echoes what it was told).
- **Invalid.**
  - No one else mints a ticket (404) or forgets the browser (404).
  - With no browser connected, the agent's reply carries the platform's
    words, and the owner's turns are told nothing of a browser; nor after
    Forget.
  - A socket from a page's origin (403) and one naming no ticket (400)
    are refused.
  - A link's ticket is refused to a page (403), and to a link the
    computer never made (401).
- **Replay.** A ticket spent once is refused again (401). A forgotten
  browser's link is refused (401).
- **Restart.**
  - With its link a browser asks for a ticket of its own and comes back,
    replacing the socket before it, which is closed `4001`.
  - Its owner forgets it: its socket is closed `4002`, and its view lists
    it no more.
- **Replaced.** A browser dropped mid-call ends the guest's socket: the
  agent's reply says "your owner's browser disconnected".
- **The real extension, in Chromium 148, headless.**
  - Settings offers "Connect this browser". Connecting it holds the
    computer's socket.
  - Another person signed in to the same browser is offered Connect, not
    told it is connected: the extension holds the first person's computer.
  - Settings lists this browser by the name its extension gave
    ("Chromium on macOS"), connected.
  - The agent's `browse` of a `data:` page opens a tab in that Chrome
    (listed by its own DevTools) and replies with the page's title.
  - A dropped connection (a test lever, `drop-browser`, drops it as a
    network would) comes back on its own, with the extension's link, and
    the agent uses the browser again.
  - Its worker stopped, as Chrome stops one idle past its socket's end,
    then woken by the platform's page: it comes back with its link, and
    the tab the agent opened is still its own. Against the extension
    before it kept the agent's tabs (only the chosen ones), this check
    fails: the agent lists only a new blank tab (run).
  - What the agent may use follows the popup's level. The stub's `tabs`
    (the titles of the tabs it may use) and `close-tab <title>` stand in
    for a browser tool's `tab` and `tab close`:
    - at "Only the tabs they open" (the default), the agent lists its
      own tab and none of the person's;
    - at "Those, and tabs I choose", the popup lists the person's tabs,
      and the one switched on joins the agent's;
    - at "All my tabs and windows", the agent lists every tab and closes
      one of the person's;
    - back at "Only the tabs they open", the person's tabs leave the
      list.
  - Reloaded (its owner's Reload in `chrome://extensions`), the extension
    comes back on its own, with its link. The Settings page open from
    before keeps the old script, which Chrome cut off from it: asked, it
    says "reload this page" instead of throwing (the error a person saw
    in the extension's Errors, trying it), and that page loaded again
    shows this browser connected.
  - Forget, in Settings, ends it. The extension hears it must not come
    back (its status says "your owner forgot this browser"), and the
    next `browse` is told the browser is not connected.

The harness launches a lane's own Chrome without `--disable-extensions`
when the lane loads one (`crates/e2e/src/browser.rs`), and finds the
extension by the id Chrome derives from its path. A Chrome that loads no
unpacked extension (Google Chrome from 137, CI's) makes those fourteen
checks skips that say so.

### Hermes, end to end (run)

`CHROME_BIN=<Chromium 148> cargo xtask e2e --only owner-browser-hermes`:
8 passed, 0 failed, 0 skipped. The section runs only by name, as
`hermes` does, because it builds our Hermes image (v0.21.7).

- A computer pinned to the Hermes image wakes and follows its chat.
- Settings connects the extension's Chrome.
- The scripted model's `run:` has Hermes' terminal tool run `agent-browser
  --session owner --cdp ws://browser.fragment.internal/devtools/browser
  open <the platform's page>`.
- The tab opens in the extension's Chrome, and the tool's answer reads the
  page's title.
- The model's request carries the bridge's line: Hermes renders the
  inbound's `context` before its owner's message.
- Nothing in the image changed: the agent-browser it already carries
  (0.26.0) is the client.

Its first run failed on its page, not its path (run). The tabs opened,
but agent-browser's `open` navigates with `Page.navigate`, and Chrome
refuses a `data:` URL navigated through `chrome.debugger`
(`net::ERR_ABORTED`). The stub's check passes with a `data:` page only
because the extension opens it with `chrome.tabs.create`. The section
now opens the platform's own page.

Hermes v0.21.7 also reads a CDP endpoint from `BROWSER_CDP_URL` or the
profile's `browser.cdp_url`. A full `ws://…/devtools/browser…` URL
passes through as given (`tools/browser_tool_cdp.py` at v0.21.7; read),
so its own browser tools could be pointed here too (question 5).

### On the dev stack, as a person tries it (run)

`cargo xtask dev`, and a headless Chromium 148 with the extension,
driven over its DevTools by a throwaway script (not in the repo):

- sign in through the WorkOS fake, and the shell's first run (the
  computer, the agent, its chat);
- Settings → Connect this browser;
- `browse https://example.com` in the agent's chat: "browsed
  https://example.com: Example Domain", the tab open in that browser.

Then a second person in the same browser, who connects and browses
`https://example.org`. This found two bugs, both fixed on the branch:

- **Settings missed the extension's answer.** Settings asked the
  extension how it was before its page was shown, and the answer could
  come before the line it redraws. It now asks once the page is shown.
- **Settings said "Connected" for another person's computer.** The
  extension reported only connected or not. It now says which computer
  it holds, and Settings says "Connected" only for the person's own;
  otherwise it offers Connect, which replaces the other. The e2e checks
  it.

### A real agent, in plain English (run)

The dev stack calls no real model by design. For these trials only, a
local change to `cargo xtask dev`, not on this branch, gave it Finite's
own model config (deploy/example.jsonc: GLM-5.3 Flash through
OpenRouter, on a budgeted key of Austin's read from a file) and put new
computers on our Hermes image.

- **With a skill.** The first trial's agent carried a skill in its own
  fragment (2.3 KB: `skills/owner-browser/SKILL.md`, this branch's
  history before the line). Austin asked: "Use my browser to find a
  one-way flight from Austin to San Francisco on November 3rd, and tell
  me the cheapest options." The agent drove his browser through
  agent-browser in 3 steps, and answered from Google Flights (AUS → SFO:
  $164 cheapest, $224 nonstop).
- **Told nothing.** A fresh agent of his, without the skill, was asked
  to "create a random assortment of 20 or so duplicate tabs on my
  chromium window here with existing tabs". It duplicated tabs in the
  Chromium on its own desktop, said no other browser existed, and
  Hermes' self-improvement then wrote it a skill
  (`desktop-browser-tab-management`) that would send it there again.
  Nothing it is given names the owner's browser: the managed skills, the
  image and the CLI's skill were searched (read).
- **Told by the line.** A test person's agent (Hermes on that model),
  their browser a throwaway Chromium connected in Settings, two of
  "their" tabs open in it. Each was its first try, in plain English:
  - At "Only the tabs they open": "What tabs do I have open in my
    Chromium window right now?" Its one step: `agent-browser --session
    owner --cdp ws://browser.fragment.internal/devtools/browser tab`.
    The line's first wording ("`tab` lists the tabs they let you use")
    let it answer that the person had one tab open; worded "only the
    tabs they let you use, at first just the ones you open", it said it
    could see only one, and that the extension's icon gives it more.
  - At "All my tabs and windows": "Please create a random assortment of
    5 or so duplicate tabs on my chromium window here with existing
    tabs". It read `--help`, listed the tabs, and opened 5 duplicates of
    the person's tabs in that Chromium, in 9 s.
  - Stopping the dev stack and starting it again lost the agent the tabs
    it had opened: Chrome had stopped the extension's worker while the
    computer was away, and the worker kept them only in memory. They are
    kept for the browser's session now (the extension, above).

### Tests (run)

- `cargo test -p fragment-core --lib owner_browser` (9): a frame's
  rules, the guest's path, the extension's origin, a browser's name and
  link, which ends of its socket it comes back from, and
  `/json/version`'s answer.
- `cargo test -p fragment-bridge` (in `images/`):
  `its_owner_is_told_their_browser_while_it_is_connected` (its owner's
  turn told while a faked `/json/version` answers connected, another
  asker's never, and none once it answers not), the line's own test, and
  Relay's `context` as a line each.
- `cargo test -p fragment-bridge --lib browse` (in `images/`): `browse`
  against an in-process browser (an event before each answer, the page
  loaded on the second read), and a refusal that says why in the
  platform's words.
- `cargo test --workspace --all-features` and both clippy passes, as
  `cargo xtask check` runs them, and `images/`' `cargo test --workspace`.
  Three things on this Mac are not the branch's:
  - `cli/tests/mcp.rs` writes the CLI's config under `.config`, where
    macOS's CLI does not read it.
  - The hosted plan's test wants a Chrome at a default path (it passes
    with `CHROME_BIN`).
  - Clippy 1.94's `nonminimal_bool` flags `crates/core/src/schema.rs`
    and the bridge's relay.

  With that lint allowed, clippy has nothing to say about this branch.

## What the spike leaves out

- **The platforms it trusts.** The content script runs only on
  `127.0.0.1` and `localhost` (the dev stack and the e2e), and the
  extension reaches computers' origins only under `*.fragment.localhost`. A hosted
  platform's origin would be listed per deployment. Fragments share a
  branch preview's zone, so the extension must trust the platform's
  exact origin, never its zone.
- **A page's own targets.** Cross-origin frames and workers (CDP's child
  sessions) are not offered: `Target.setAutoAttach` on a page answers
  `{}`.
- **The agent's tabs open in the background** (`active: false`), so the
  person's tab keeps the focus.
- **The line's cost.** It rides each turn its owner asks while the
  browser is connected, about 100 tokens, and Hermes keeps each in its
  session. Told once a chat, or when the browser comes or goes, would
  cost less and keep more state (question 5).
- **Hosted costs and bounds (unknown).** Each ping, 20 s apart, wakes the
  DO, and so does the bridge's `/json/version` before each turn its
  owner asks. A WebSocket auto-response would answer it without waking it. The
  largest frame the DO carries, against a full-page screenshot's
  base64, was not measured on Cloudflare.

## Questions for Paul

1. **May an agent use its owner's own browser at all?** It carries the
   person's whole web identity (mail, bank, accounts), far beyond the
   agent's role on the platform (R17, decision 36).
2. **Whose agents.** Every agent on the computer, as built (decision 44
   makes a per-agent list no boundary), or only agents the owner names?
3. **The consent.** As built: the person connects the browser in
   Settings, and Chrome's own bar and the "Agent" tab group show it in
   use. Should the agent also ask, as a card, before its first use in a
   chat? That is a question about an outcome, which Paul's rule of
   2026-10-14 allows.
4. **How much of the browser.** As built, the person chooses in the
   extension: only the tabs the agent opens (the default), those and
   tabs they switch on, or all their tabs and windows. Should "All" be
   offered? Should it ask again before taking effect, or carry a
   warning in Settings too?
5. **Where the agent learns of it.** As built, the bridge tells it: one
   line beside each of its owner's messages while the browser is
   connected (the appendix), and nothing otherwise, so an agent whose
   owner connects no browser carries nothing. Told nothing, an agent used
   its own desktop's browser. Is the bridge the place? The alternatives:
   - a skill (the first trial's, 2.3 KB): a thirteenth managed one (that
     set is Paul's), the platform skill (143 bytes left of its 16 KiB),
     or each agent's own;
   - Hermes' own `browser.cdp_url`, which would put its browser tools on
     the owner's browser for every call, connected or not.
6. **The link.** As built, a browser's link can do two things: ask for a
   ticket to this computer's browser socket, and forget itself. It lasts
   until its owner forgets it, or eight newer browsers push it out. Should
   a link also lapse after a time unheard, or need Connect again after a
   new sign-in?
7. **Shipping it.** The Chrome Web Store (a vendor account), and which
   platform origins a build trusts.
8. **The inventory.** It would be a surface that is not a fragment
   (`SPECIAL-CASE-INVENTORY.md`). A row is proposed below.

## What it would change, if kept

- **SPECIAL-CASE-INVENTORY.md**, a row: "The owner's browser | Security:
  the person's own browser, reached only through the computer its
  owner connected it to, by a one-time ticket | the Computer DO's
  sockets; the egress intercept `browser.fragment.internal`".
- **docs/computers.md**: the guest's contract gains
  `FRAGMENT_BROWSER` and its socket; "What its owner is told" gains the
  browser's state, if the shell shows it.
- **docs/api.md**: `POST /api/computers/{id}/browser/ticket`, `DELETE
  …/browser/links/{link}`, the view's `browsers`, and the computer
  origin's `/__browser` (a socket, and `POST` `ticket` and `forget`).
- **docs/finite-integration.md**: the "Reaching a computer or agent" row
  gains the opposite direction: the computer reaching its owner's
  machine. FIN-39's bearer-or-Iroh question is the same one asked the
  other way.
- **docs/cloudflare-v1.md**: a decision of Paul's, if he takes it.

## Trying it locally

1. `cargo xtask dev`, then sign in at `http://127.0.0.1:8790/` (any
   email: the WorkOS fake). The first run makes the computer (the stub
   image) and an agent.
2. In Chromium (or Chrome for Testing; Google Chrome loads no unpacked
   extension from 137), open `chrome://extensions`, turn on Developer
   mode, and Load unpacked: `extension/`.
3. Settings → Computer → This browser → Connect this browser.
4. In the agent's chat: `browse https://example.com`. A tab opens in the
   "Agent" group, and the agent replies with its title, then what it was
   told: `(told: its owner's browser, …)`.
5. The extension's icon sets what your agents may use: only the tabs
   they open (the default), those and tabs you switch on, or all your
   tabs and windows. At "All", `tabs` in the chat lists every tab you
   have open.
6. Let your laptop sleep, or stop the dev stack and start it again: the
   extension reconnects by itself, and Settings lists the browser by
   name.
7. Forget it in Settings (or Disconnect in the popup, or close Chrome's
   bar), then `browse` again: the agent says the browser is not
   connected.

Loaded into a browser you use, the agent's tabs carry your own sign-ins:
that is the point, and question 1. To try it without them, start
Chromium with a profile of its own (`--user-data-dir=<a new
directory>`) and load the extension there.

The dev stack runs the stub image, whose `browse` is the guest. A real
browser tool on a computer drives the same socket with
`agent-browser --cdp ws://browser.fragment.internal/devtools/browser
<command>`.

## Appendix: what the agent is told

Before each turn its owner asks, while their browser is connected, our
Hermes image's agent is handed this line beside the message
(`relay::browser_told`, images/bridge), the name and socket as the
computer answered `/json/version`:

> Your owner's browser is connected: Chromium on macOS, on their own
> computer, signed in as them. When they say "my browser", or its tabs
> and windows, they mean this one. Drive it from your terminal with
> `agent-browser --session owner --cdp
> ws://browser.fragment.internal/devtools/browser <command>` (its
> `--help` lists the commands). `tab` lists only the tabs they let you
> use, at first just the ones you open; they give you more from the
> Fragment extension's icon. Ask them before you pay, buy, book or type a
> password there.

It carries only what the agent cannot find by looking: which browser
"my browser" means, the address, and that what `tab` lists is the
owner's choice. The commands are agent-browser's own `--help`, and the
access levels the extension's. Its last sentence is a guardrail, added
after the trials: the browser is signed in as its owner.
