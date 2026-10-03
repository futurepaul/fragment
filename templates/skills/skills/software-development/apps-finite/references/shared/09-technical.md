# Technical Rules And Workflow

This file is authoritative for fragment. It overrides foreign
`deploy_website`, S3, iframe, opaque-URL, proxy, or port-exposure
assumptions from the source material.

## Project Structure

Each app is one fragment, and its folder is the fragment's working copy:

```text
garden/
├── fragment.json      # operations, channels, triggers, meta
├── app.mjs            # the App class (only when it has operations)
├── applib/            # modules app.mjs imports
├── site/              # what the fragment serves: index.html, assets
├── src/               # a toolchain's source, when there is one
└── package.json       # only when using a toolchain
```

For React, Vite or another framework, keep the source in the folder and
build into `site/`. The fragment serves `site/` from live and runs no
build: build and test before deploying.

## Platform Rules

- Publish with `fragment deploy <name> --dir .`: it syncs the folder to
  `main` and moves `live` to it. `fragment rollback <name>` moves it back.
- Visibility is `link` by default (anyone with the share link). Make it
  `members` for a private app, and `public` only after the human agrees.
- Do not edit proxies, DNS, host networking, or platform configuration.
- Never commit secrets: `.env*`, private keys, tokens, credentials. An
  app's outbound calls take secrets from `fragment secret set`.
- What syncs: everything but dot files and folders, the top-level
  `node_modules/`, editor droppings, and sync's own `.conflict-` copies.
  A file of 1 MiB or more is stored as a blob and served at its path.
- A sync that would delete more than max(3, 30%) of the files it knows,
  or all of them, is refused (exit 4) until `--apply-mass-delete`: check
  before you pass it.
- `app.mjs` and `applib/` together are at most 4 MiB and 64 modules.
- Use relative asset paths inside `site/`.
- External links should use `target="_blank" rel="noopener noreferrer"`.
- Install missing development tools in the project or your home rather
  than asking for host changes.

## Workflow

1. Pick the shape (site, document, stateful app) and the art direction.
2. Research real references.
3. `fragment init <label> --template <closest>` in `~/apps`.
4. Build the experience with real content and intentional assets.
5. Run unit, integration, accessibility, and browser checks appropriate to
   the project.
6. Deploy and open its link at desktop and mobile sizes:

   ```sh
   fragment deploy <name> --dir .
   fragment open <name>
   ```

7. Read `fragment status <name>`: `code.error` says why code was refused,
   and `fragment events <name> --tail 30` what happened.
8. Change sharing only when the human asks.

## Recommended Local Preview

A static `site/` can be previewed before a deploy:

- Plain static site:
  `setsid sh -lc 'npx serve site -l 3000 --no-clipboard >/tmp/project-qa.log 2>&1 < /dev/null' >/dev/null 2>&1 & echo $! >/tmp/project-qa.pid`
- Vite / React (its dev server):
  `setsid sh -lc 'npm run dev -- --host 127.0.0.1 --port 3000 >/tmp/project-qa.log 2>&1 < /dev/null' >/dev/null 2>&1 & echo $! >/tmp/project-qa.pid`

Avoid plain `nohup ... &` preview one-liners in Hermes terminal tools
because they can leave the terminal wrapper hanging. Verify the local
server with `curl http://127.0.0.1:PORT` before opening the browser, and
stop it after QA. Operations, channels and `__fragment.js` answer only on
the deployed fragment: preview those at its link.

## Backend Note

If the app needs state, webhooks, schedules or AI, read `19-backend.md`.

## Quality Checklist

- Research informed the design.
- Typography, color, spacing, and assets feel intentional.
- Interactive or data-heavy views were checked in a real browser.
- Mobile and desktop both look deliberate.
- The project's own tests and build passed before the deploy.
- The live link was tested after the deploy, not only a local preview.
- Visibility matches the human's explicit request.
