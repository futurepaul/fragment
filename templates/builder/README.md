# builder

A fragment that builds fragments. You say what to build, and goose, on
this fragment's own computer, makes it as a new fragment and deploys it.

- `fragment.json` declares a computer (`"computer": {}`): the platform
  gives the fragment a Sprite, paired as your computer and an editor
  here, with the `fragment` CLI signed in as itself. What it makes is
  yours, on your budget.
- `build({task, chat?})` is a job (editors only). Each step is a durable
  `job.computer.exec` on that computer:
  1. The job lists your fragments, then hands the task to the computer's
     hands: goose (v1.52.0, which every computer runs as `goose serve`:
     docs/agent-computer.md), in the session of the chat that asked (none:
     this fragment's own), so a later task there remembers this one. goose
     has the developer tools, no approvals, and the CLI's guide as its
     hints. Its model is the platform's: `fragment model --serve` runs
     beside it, and every call is signed as the computer and paid from
     your budget. No model key is ever on the computer. Each of its tool
     calls is a step on the chat's `work` (this fragment's own, when no
     chat asked).
  2. goose makes a fragment (`fragment create`), writes it, and deploys
     it. The job lists your fragments again: the new ones are what it
     built.
  3. The run answers `{url, built, message, code}`: the new fragment's
     URL, each fragment it made (name, URL, whether it is live), the end
     of what goose said, and the task's exit code.
- The run's time is capped: the task ends after 9 minutes (its step
  after 10), and each other step after 3.
- `record` keeps each build's progress (building, built,
  failed) and `builds` lists them. The page shows them live, with links
  to what each built.
- The goose it runs is the platform's pin (`fragment_core::computer::GOOSE_VERSION`), the same on every computer.
