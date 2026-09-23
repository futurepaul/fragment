# todo

A shared todo list on fragment's operation model: the reference app for
the author-facing API.

- `fragment.json` declares four operations with input schemas (`add`,
  `toggle`, `remove` are mutations; `list` is a query) and one channel,
  `activity`. Their role is `public`: anyone who can open the fragment
  can change the list, so its visibility decides who that is (the share
  link by default). For a list only members may change, set the
  mutations' role to `editor` and the visibility to `members`; browsers
  act as members once people sign in (phase 4).
- `app.mjs` is the `App` class: one method per operation over its own
  SQLite. Each mutation publishes a line to `activity`.
- `site/index.html` uses the browser library (`./__fragment.js`): `live`
  re-runs `list` whenever anything changes, `subscribe` follows
  `activity`, and `presence` shows who else has the page open.

From the CLI: `fragment call <name> add --input '{"text":"milk"}'`,
`fragment call <name> list`, `fragment channel <name> activity --follow`.
