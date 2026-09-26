# builder

A fragment that builds fragments. You say what to build, and goose, on
this fragment's own computer, makes it as a new fragment and deploys it.

- `fragment.json` declares a computer (`"computer": {}`): the platform
  gives the fragment a Sprite, paired as your computer and an editor
  here, with the `fragment` CLI signed in as itself. What it makes is
  yours, on your budget.
- `build({task})` is a job (editors only). Each step is a durable
  `job.computer.exec` on that computer:
  1. goose's CLI is installed once: release v1.50.0
     (`goose-x86_64-unknown-linux-musl.tar.gz`, static), fetched from
     GitHub and checked against its SHA-256 before it is unpacked. The
     CLI's guide becomes goose's hints (`fragment guide >
     ~/.config/goose/.goosehints`), so goose knows the CLI.
  2. The job lists your fragments, then runs goose headless in
     `~/builds/run-<run>` (`goose run --text <prompt>`, the developer
     tools, no approvals, at most 60 turns). goose's model is the
     platform's: `fragment model --serve` runs beside it on a free
     loopback port, and every call is signed as the computer and paid
     from your budget. No model key is ever on the computer.
  3. goose makes a fragment (`fragment create`), writes it, and deploys
     it. The job lists your fragments again: the new ones are what it
     built.
  4. The run answers `{url, built, message, code}`: the new fragment's
     URL, each fragment it made (name, URL, whether it is live), the end
     of what goose said, and goose's exit code.
- The run's time is capped: goose's step ends after 10 minutes, and each
  other step after 3.
- `record` keeps each build's progress (installing, building, built,
  failed) and `builds` lists them. The page shows them live, with links
  to what each built.
- Changing the pinned goose means changing `GOOSE_VERSION`,
  `GOOSE_SHA256` (GitHub lists each release asset's digest), and a
  release at least two weeks old. A computer keeps each version it
  installed as `~/.local/bin/goose-<version>`.
