# Playwright QA on your computer

Use Playwright for complex sites: dashboards, multi-step flows, data-heavy apps, and anything where the agent needs to see the result and iterate.

Playwright depth should grow with product certainty. It is a quality tool, not a
reason to hide the first result while the agent repeatedly debates its own taste.

## Rules

- Use Playwright for real browser QA, not just DOM inspection.
- Keep one server running and reload between edits instead of restarting constantly.
- Take screenshots at desktop, narrow pane (about 380 px), and mobile
  widths, in light and dark. Check for horizontal overflow and confirm
  that primary controls work with the keyboard.
- Apps default to no header, title bar, banner, hero, or page-title
  heading unless the human asks: the shell already names the app. Review
  that the page starts with useful content; content headings are fine.
- Check both functionality and visual quality.
- For simple one-page static sites, a careful manual code review may be enough. For anything richer, use Playwright.

## Setup

If Playwright is missing in the project:

```bash
npm install -D playwright
```

Your computer carries a headless Chromium (Hermes' browser): prefer it over
Playwright's downloaded browser cache when it is set, and download one only
when it is not:

```js
import { chromium } from "playwright";

const browser = await chromium.launch({
  executablePath: process.env.AGENT_BROWSER_EXECUTABLE_PATH || undefined,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});
```

Without it, run `npx playwright install chromium` once (it lands in your
home, which your computer keeps).

If the repo already uses Playwright, reuse its setup rather than inventing a second one.

## Start the App

A deployed fragment needs no local server: open its link (the share link
for a `link` fragment). For a static `site/` before its first deploy, run a
local preview on a known port with a detached process that survives
separate tool calls; plain backgrounding is not enough for multi-step QA
loops.

Use a pattern like:

```bash
setsid npm run dev -- --host 0.0.0.0 --port 3000 \
  >/tmp/project-qa.log 2>&1 < /dev/null &
echo $! >/tmp/project-qa.pid
curl http://127.0.0.1:3000
```

or for a static project:

```bash
setsid npx serve . -l 3000 --no-clipboard --single \
  >/tmp/project-qa.log 2>&1 < /dev/null &
echo $! >/tmp/project-qa.pid
curl http://127.0.0.1:3000
```

Use `127.0.0.1`, not `localhost`, when opening the app in Playwright.

On your computer:

- Bind local previews to `127.0.0.1`: nothing outside the computer reaches them.
- Prefer the computer's Chromium via `executablePath: process.env.AGENT_BROWSER_EXECUTABLE_PATH`.
- Pass `--no-sandbox` when launching Chromium inside the container.
- Operations, channels and `__fragment.js` answer only on the deployed fragment.

## Progressive QA

### Stage 1: First reveal

Before the human has seen or approved the direction:

1. Confirm the preview server responds.
2. Open the primary desktop view.
3. Confirm the intended content renders and no fatal console or page errors block it.
4. Capture one representative screenshot or preview.
5. Make at most one automatic correction pass, then reveal the draft and request
   aesthetic guidance.

Do not install a large new QA setup, exhaustively test secondary flows, tune every
breakpoint, or repeat screenshot-fix cycles merely to improve an unapproved visual
direction. State what remains untested so the human understands the draft's maturity.

### Stage 2: Direction approved

After the human confirms the direction, complete the build and expand QA:

1. Write a short QA inventory: key flows, important states, and the claims you expect to make.
2. Open the app in Playwright at desktop width first.
3. Exercise the main flow with real clicks and keyboard input.
4. Inspect visual quality, spacing, contrast, overflow, and hierarchy.
5. Repeat on a mobile viewport.
6. Fix issues and reload instead of restarting the server.
7. Capture the screenshots that support your claims.
8. Stop the detached QA process when you are done if it is no longer needed:

```bash
kill "$(cat /tmp/project-qa.pid)"
```

Keep correction loops bounded. After two unsuccessful attempts at the same visual or
functional issue, report the issue and the available choices instead of continuing
silently. A newly discovered product or aesthetic decision goes back to the human;
a clear implementation defect can be fixed autonomously.

## Minimum Checks

- Desktop screenshot at 1280px or wider
- Mobile screenshot around 390px wide
- One end-to-end happy path
- One off-happy-path or error-state check
- One visual pass specifically looking for clipping, weak contrast, ugly spacing, and broken hierarchy

### Stage 3: Pre-publish completion gate

Do not publish until:

- the local app loads cleanly
- the main interaction path works
- screenshots support the quality claim you want to make
- the app feels intentional on both desktop and mobile

## After Publish

Open the fragment's live link and run one more smoke pass. A site is not validated just because localhost worked.
