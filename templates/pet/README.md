# pet

A computer everyone with the fragment shares: its screen, live on the
page, and driven together (click it, type into it, open an address), and
by its agent (ask it to do something).

- `fragment.json` declares the computer (`"computer": {"start": "node
  computer/pet.mjs"}`): a Sprite of its own, paired as an editor here. It
  is awake while someone has the page open and 5 minutes after, on the
  owner's budget; while awake, the platform runs `start` from the
  fragment's live files (`~/fragment`), its output in `~/fragment.log`.
  So a new pet is its owner's alone (`members`) until they share it.
- `computer/pet.mjs` (Node, which a Sprite has) installs what it needs
  on its first start (apt: `xvfb openbox xdotool imagemagick
  fonts-liberation fonts-noto-color-emoji`, and `libxi6 at-spi2-core
  dbus` for Cua Driver; Google's Chrome 154.0.8037.57-1, its .deb
  checked against its SHA-256, since Ubuntu's own browser is a snap),
  starts a 1024×640 display, a session bus, and Chrome on
  `computer/start.html` with its CDP on loopback for its agent's
  Stagehand, and then:
  - follows `control` (`fragment channel <name> control --follow`) and
    applies each record once, by seq, as xdotool input: `{kind: "click",
    x, y}` (screen pixels), `{kind: "type", text}`, `{kind: "key", key}`
    (`Enter`, `Backspace`, `Escape`, `Tab`, arrows), `{kind: "open",
    url}`, skipping one older than 30 seconds. `control` says
    `"signedIn": true`, so only people signed in post there: the
    platform refuses an anonymous link holder's post (401);
  - sends the screen when it changed: a JPEG of at most 85 KB, at most
    one a second for a minute after someone drives it and one each 5
    seconds otherwise, uploaded as a blob of the fragment (`fragment blob
    put --frame`: the platform deletes a frame a minute after a later one
    arrives), then named through `frame` (`fragment call`). Its agent is
    the driver it names (`agent:<who asked>`) while `~/.pet/agent` is
    newer than anyone's last `control` record.
- `app.mjs` keeps only the latest frame (one row: its blob's hash and
  size, what is on screen, who drove it last; never the image);
  `screen` is the live query the page shows.
- `site/index.html` shows the frame big, from `./__blob/<shot>`; a click
  on it posts a click at the same point of the screen. Anyone who can
  open the fragment watches (viewers and up: the link counts); signing in
  lets you drive.
- `run({command})` is a job for editors (the owner, their agent capped at
  editor, the computer): `bash -lc` on the computer in `~/fragment`
  (`job.computer.exec`), answered with `{code, stdout, stderr,
  truncated}`. The owner's agent calls it from any chat (the desktop's
  Computers: docs/computers.md). A viewer is refused.
- `do({task, chat?})` is its agent, a job for editors (it spends the
  owner's budget): the hands every computer has (goose v1.52.0 in `goose
  serve`, one session per chat: docs/agent-computer.md) do `task` in the
  session of the chat that asked (none: this page's own), with two ways
  to the screen, each named in goose's config as an extension: the
  browser (`computer/browser/browser-mcp.mjs`, a small MCP server on
  Stagehand 4.1.0, installed with `npm ci` from its lockfile), for
  anything on the web, through each page's structure, its own model
  calls on Jev with flashx behind it; and Cua Driver v0.28.3, for other
  desktop apps (installed once from its release and checked against its
  SHA-256; `cua-driver mcp`, 8 of its tools, its screenshots capped at
  768 pixels). The task runs for at most 9 minutes. goose's model is the platform's
  (`fragment model --serve`, `z-ai/glm-5.3-flashx`, which reads images),
  and every request goes as goose made it. Each tool call is a step on
  the chat's `work` (`{kind: "turn.step", turn, run, step, tool, args,
  ok, excerpt, text, shot}`, `shot` a screenshot the tool showed, as a
  blob of that chat's), this page's own when no chat asked, where the
  job also posts the run's `start` and `end`, which the page shows live;
  the run answers `{message, code}`. Anyone may click while it works: its
  next look shows it.

`frame` says `"ephemeral": true`: its calls leave no ledger row in the
app's database (a mutation's id is otherwise kept a week there, which a
frame a second would fill within a day), so the database holds only the
one row. It gives up the replay (a call sent again runs again, which is
harmless for the latest frame) and effects (it publishes nothing).

Its state is in `~/.pet` on the computer: the last record applied, the
browser's profile, the display's and browser's logs, the session bus,
and who the agent last worked for (`agent`). The hands keep theirs in
`~/.fragment/agent` (goose's sessions in its own store; `browser.log`,
each of the browser's model calls), and the browser's server and
Stagehand are in `~/.local/share/pet-browser`. `PET_FAKE_SCREEN=
<a JPEG>` shows that image instead (nothing installed, nothing driven).
