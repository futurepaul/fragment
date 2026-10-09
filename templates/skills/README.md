# skills

The blessed skills template (docs/cloudflare-v1.md, decisions 17 and 40):
the managed skills every agent of a person has. Its repo here is the
managed set, versioned with the platform; each person has one skills
fragment on it (kind `skills`), which the shell makes at setup beside their
default agent. A fragment on it names it and holds nothing else:
`{"template": "skills"}`.

## The contract

- **Its files are the release's managed set.** Every file of this template
  but its manifest, this README and `site/` is its *data*
  (crates/templates `blessed::is_data`). A fragment on the template lists
  and reads that data as its own files, from the platform's current
  release: `GET /api/f/{name}/files` (each such row `release: true`, its
  `lastCommitSha` the file's version, `release:<hash of its bytes>`), `GET
  …/file?path=`, and its site's `__files` and `__file`. One deploy of the
  platform updates every person's managed set; there is no copy to drift.
- **A file of its own wins.** A file the fragment holds at a managed path
  (`POST /api/f/{name}/files`) is listed and read in the release's place;
  removed, the release's is back. Forking (a `fragment.json` that names no
  template) makes the whole set its own.
- **A skill** is `skills/<category>/<name>/SKILL.md`, or
  `skills/<name>/SKILL.md` with no category, with the files its skill uses
  beside it. Its frontmatter's `name` is its directory's, and says what it
  is for (`description`). Its helpers are named from `${HERMES_SKILL_DIR}`,
  which Hermes fills in with the skill's directory.
- **Who reads it.** Its owner (the shell's Skills section in settings), and
  their agents' computers: our Hermes image installs `skills/` into every
  profile, as the computer's first agent acting for its owner
  (docs/computers.md, "Skills and the CLI"). The platform knows nothing of
  Hermes: this template is data, served as any fragment's files are.
- Bounds: at most 1,000 files and 4 MiB of data, each file at most 256 KiB
  (`blessed::DATA_*`; a test holds the template to them).

## The managed set

Six skills, each teaching a Fragment platform contract:

| Skill | Why it is ours |
|---|---|
| apps-finite | Fragment publishing, operations, channels and sharing |
| git-finite | Fragment repositories, sync, deploys and rollback |
| brain-finite | The brain template's files, search and wiki contract |
| google-workspace-finite | Connected Google access through per-agent placeholders; gws, no local OAuth |
| image-generation-finite | Metered Workers AI images through a fragment job |
| model-council-finite | Fragment model tiers through the computer's model intercept |

The 2026-10-08 audit removed 35 generic copies from the Finite skills
snapshot imported on 2026-10-03. It also removed apps' generic design and
game references, retaining the four platform references. The inventory,
sources, PDF experiment and recommendations are in
[docs/skills-audit.md](../../docs/skills-audit.md).

Our Hermes image exposes its own pinned bundled skills to every profile,
read-only, with the agent's own skills and managed overrides winning by
name (docs/computers.md, Skills and the CLI). The image also installs the
PDF and Office helpers' pinned Python dependencies. Generic PDF, OCR,
Office, research, code and design workflows belong to Hermes and its hub;
they are not maintained as `-finite` forks here. Optional skills are
available for explicit installation through Hermes' hub, not enabled in
all profiles merely because the image contains them.

Google is the exception: Hermes' `google-workspace` teaches local OAuth
and token files, so the image omits it in favor of the managed connected
account skill. `GOOGLE_OAUTH_ACCESS_TOKEN` is the connection's placeholder;
the image's `gws` wrapper maps it to `GOOGLE_WORKSPACE_CLI_TOKEN`.

The retained model-council helper uses `FRAGMENT_MODEL` and
`FRAGMENT_AS_AGENT`, never a provider key. The image skill uses the
platform's `job.ai.image`, not Hermes' FAL tool (FAL is not offered by the
default catalog). Perplexity research uses Hermes' native web tools with
`PERPLEXITY_API_KEY`; specialized vendor or wallet skills can be installed
when needed, rather than competing in every agent's catalog.

## Its page

`site/` lists the skills by category and shows each one's `SKILL.md`,
reading the fragment's own `__files` and `__file`, so it shows what its
owner's agents have.
