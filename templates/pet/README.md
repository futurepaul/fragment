# pet

A computer everyone with the fragment shares: its screen, live on the
page, and driven together (click it, type into it, open an address).

- `fragment.json` declares the computer (`"computer": {"start": "node
  computer/pet.mjs"}`): a Sprite of its own, paired as an editor here. It
  is awake while someone has the page open and 5 minutes after, on the
  owner's budget; while awake, the platform runs `start` from the
  fragment's live files (`~/fragment`), its output in `~/fragment.log`.
  So a new pet is its owner's alone (`members`) until they share it.
- `computer/pet.mjs` (Node, which a Sprite has) installs what it needs
  on its first start (apt: `xvfb openbox xdotool imagemagick
  fonts-liberation fonts-noto-color-emoji`; Chromium from Playwright
  1.63.0, since Ubuntu's own is a snap), starts a 1024×640 display with a
  browser on `computer/start.html`, and then:
  - follows `control` (`fragment channel <name> control --follow`) and
    applies each record once, by seq, as xdotool input: `{kind: "click",
    x, y}` (screen pixels), `{kind: "type", text}`, `{kind: "key", key}`
    (`Enter`, `Backspace`, `Escape`, `Tab`, arrows), `{kind: "open",
    url}`, skipping one older than 30 seconds. `control` says
    `"signedIn": true`, so only people signed in post there: the
    platform refuses an anonymous link holder's post (401);
  - sends the screen through `frame` (`fragment call --input @file`, as a
    frame is more than one argument holds) when it changed: a JPEG of at
    most 85 KB, at most one a second for a minute after someone drives it
    and one each 5 seconds otherwise.
- `app.mjs` keeps only the latest frame (one row: the JPEG, what is on
  screen, who drove it last); `screen` is the live query the page shows.
- `site/index.html` shows the frame big; a click on it posts a click at
  the same point of the screen. Anyone who can open the fragment
  watches; signing in lets you drive.
- `run({command})` is a job for editors (the owner, their agent capped at
  editor, the computer): `bash -lc` on the computer in `~/fragment`
  (`job.computer.exec`), answered with `{code, stdout, stderr,
  truncated}`. The owner's agent calls it from any chat (the desktop's
  Computers: docs/computers.md). A viewer is refused.

`frame` says `"ephemeral": true`: its calls leave no ledger row in the
app's database (a mutation's id is otherwise kept a week there, which a
frame a second would fill within a day), so the database holds only the
one row. It gives up the replay (a call sent again runs again, which is
harmless for the latest frame) and effects (it publishes nothing).

Its state is in `~/.pet` on the computer: the last record applied, the
browser's profile, and the display's and browser's logs. `PET_FAKE_SCREEN=
<a JPEG>` shows that image instead (nothing installed, nothing driven).
