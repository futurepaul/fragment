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

All of `finite-mono/finite-skills/skills` (47 skills) but
`shared-skills-finite`, which git replaces (decision 17), under the same
categories and names: 43 skills. Names keep finite-skills' `-finite`
suffix, so none collides with a skill Hermes bundles or one an agent makes.

### Rewritten for fragment

- **apps-finite**: make, publish and share a fragment app with the
  `fragment` CLI. Replaces `finite-sites-publishing-finite`,
  `publish-web-apps-finite` and `website-building-finite` (Finite Sites and
  `fsite`); website building's design references are kept, and its four
  Finite-specific ones (`09-technical`, `12-playwright-interactive`,
  `19-backend`, `20-llm-api`) are rewritten for fragments.
- **git-finite**: fragment git: a fragment as a repository, `fragment
  sync`, history, deploys and rollback, sharing. Replaces Finite Sites
  Project Repositories.
- **brain-finite**: keep and search a brain fragment (the blessed `brain`
  template) with the CLI and the brain's `search {q, limit?, wiki?}`
  operation, whose answer is `{results: [{rank, path, wiki, title, heading,
  ancestry, snippet}], pending}`, and its `guide`. Replaces `finitebrain`
  (`fbrain`) and `llm-wiki-finite`, whose wiki method (nvk's llm-wiki, MIT)
  it adapts; that license is beside it.
- **google-workspace-finite**: Gmail, Calendar, Drive, Contacts, Sheets and
  Docs through the person's connected Google account: its helper is
  rewritten on Google's REST APIs with Python's standard library, sending
  `Authorization: Bearer fragment-connection:google` and `x-fragment-agent`
  (docs/computers.md, Connections). The OAuth setup scripts are gone.
- **image-generation-finite** (was `fal-image-editing-finite`): text to
  image with Cloudflare's own FLUX.1 [schnell] through a fragment AI step
  (`job.ai.image`), from an images fragment it carries the code for
  (`images-app/`). **Missing:** editing an existing image, reference
  images, masks, model choice and aspect ratio; the skill says so.
- **monday-com-finite**: Finite's Monday MCP integration is gone; the skill
  now calls Monday's GraphQL API through the `monday` connection.
- **model-council-finite**: OpenRouter is gone (decision 23: models
  through AI Gateway); the council is the platform's model tiers (`medium`,
  `cheap`, and `high` while on) through the computer's model intercept.
  **Missing:** other vendors' frontier models.

### Ported as they were, with Finite-only instructions stripped

- Skill paths: `/profile-assets/hermes-local/managed-skills/…`,
  `~/.finite/managed-skills/current/…` and
  `${FINITECHAT_HOME:-/data/agent}/managed-skills/finite/current/…` are
  `${HERMES_SKILL_DIR}`; a helper of another skill is named by that skill.
- Runtime claims: `/home/node/`, `~/.hermes/venv` and "the Finite runtime
  includes …" are a virtualenv of the agent's own in its home; Telegram
  location pins and `MEDIA:` notes name chats generally.
- Keys read from `~/.hermes/.env` are the computer's credential swap: the
  helpers send the placeholder in the header the provider reads, and
  `x-fragment-agent: $FRAGMENT_AS_AGENT` (perplexity-research, x-api,
  x-search, goplaces, linear, notion, music-generation; x-search now calls
  xAI's REST Responses API, since the xai-sdk's gRPC cannot pass the
  intercept). `trading-agent-finite` reads FRED's keyless CSV, since FRED
  takes its key in the URL, which the swap cannot fill.
- Left out as no skill reads them: tufte-viz's demo pages (2.5 MB), the
  OOXML schemas under powerpoint's `scripts/office/schemas/` (no script of
  it reads them), the paper-writing templates' example PDFs, and compiled
  Python (`__pycache__`). `inference-sh/cli-finite` is now
  `inference-sh/inference-sh-cli-finite`, its directory its name.

### What the deployment must offer

A skill whose provider the deployment does not offer answers 401 or 403,
and says so. The connections (`FRAGMENT_CONNECTIONS`, WorkOS Pipes) and the
operator's keys (`FRAGMENT_OPERATOR_KEYS`) these skills use:

| Skill | Credential | Hosts |
|---|---|---|
| google-workspace-finite | connection `google` | `gmail.googleapis.com`, `www.googleapis.com`, `people.googleapis.com`, `sheets.googleapis.com`, `docs.googleapis.com` |
| linear-finite | connection `linear` | `api.linear.app` |
| notion-finite | connection `notion` | `api.notion.com` |
| monday-com-finite | connection `monday` | `api.monday.com` |
| perplexity-research-finite | keys `perplexity`, `firecrawl` | `api.perplexity.ai`, `api.firecrawl.dev` |
| x-search-finite | key `xai` | `api.x.ai` |
| x-api-finite | key `x` | `api.x.com` |
| goplaces-finite | key `google-places` | `places.googleapis.com` |
| music-generation-finite | keys `fal`, `elevenlabs` | `fal.run`, `queue.fal.run`, `api.elevenlabs.io` |

Third-party CLIs cannot name an agent (`x-fragment-agent`), so the swap
cannot fill their keys: `parallel-cli-finite` and
`inference-sh-cli-finite` work only with an account the person logs into
from the CLI itself, and `pdf-workbench-finite`'s `nano-pdf` only with a
Gemini key the person gives for the task.

## Its page

`site/` lists the skills by category and shows each one's `SKILL.md`,
reading the fragment's own `__files` and `__file`, so it shows what its
owner's agents have.
