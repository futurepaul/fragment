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
  `GOOGLE_OAUTH_ACCESS_TOKEN` (the connection's placeholder) as a bearer
  token (docs/computers.md, Connections and operator keys). The OAuth setup
  scripts are gone.
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
- Keys read from `~/.hermes/.env` are the computer's credentials (Paul,
  2026-10-04): each is the provider's standard environment variable, which
  holds a placeholder naming the agent, and the helpers send it where the
  provider reads it, as any SDK or CLI would, with no header of ours; the
  computer's swap fills it on the way to the provider's own hosts
  (perplexity-research, x-api, x-search, goplaces, linear, notion, monday,
  music-generation). x-search calls xAI's REST Responses API, since the
  xai-sdk's gRPC cannot pass the intercept. `trading-agent-finite` reads
  FRED's keyless CSV, since the platform offers no FRED key.
- Left out as no skill reads them: tufte-viz's demo pages (2.5 MB), the
  OOXML schemas under powerpoint's `scripts/office/schemas/` (no script of
  it reads them), and compiled Python (`__pycache__`).
- The paper-writing skills' conference LaTeX kits (38k lines of `.sty`,
  `.bst`, `.tex`) are gone, and so is `ml-paper-writing-finite`, a
  duplicate of `research-paper-writing-finite` (Paul, 2026-10-04: skills
  stay minimal): an agent fetches the venue's own kit, which changes yearly. `inference-sh/cli-finite` is now
  `inference-sh/inference-sh-cli-finite`, its directory its name.

### What the deployment must offer

The providers these skills use, as rows of the deployment's catalog
(`providers` in its config: `deploy/example.jsonc`; docs/computers.md,
Connections and operator keys). A guest is given each one its agent may use
as a placeholder in the environment variable named; a skill whose variable
is unset says the deployment does not offer it. The platform's catalog
(Paul, 2026-10-04) offers the first five; each other is a row away (and,
for a connection, its provider enabled in WorkOS Pipes).

| Skill | Provider | Kind | Environment variable | Hosts | Offered |
|---|---|---|---|---|---|
| google-workspace-finite | `google` | connection | `GOOGLE_OAUTH_ACCESS_TOKEN` | `gmail.googleapis.com`, `www.googleapis.com`, `people.googleapis.com`, `sheets.googleapis.com`, `docs.googleapis.com` | yes |
| perplexity-research-finite | `perplexity` | operator | `PERPLEXITY_API_KEY` | `api.perplexity.ai` | yes |
| goplaces-finite | `google-places` | operator | `GOOGLE_PLACES_API_KEY` | `places.googleapis.com` | yes |
| x-search-finite | `xai` | operator | `XAI_API_KEY` | `api.x.ai` | yes |
| music-generation-finite | `elevenlabs` | operator | `ELEVENLABS_API_KEY` | `api.elevenlabs.io` | yes |
| perplexity-research-finite | `firecrawl` | operator | `FIRECRAWL_API_KEY` | `api.firecrawl.dev` | no |
| music-generation-finite | `fal` | operator | `FAL_KEY` | `fal.run`, `queue.fal.run` | no |
| x-api-finite | `x` | operator | `X_API_BEARER_TOKEN` | `api.x.com` | no |
| linear-finite | `linear` | connection | `LINEAR_API_KEY` | `api.linear.app` | no |
| notion-finite | `notion` | connection | `NOTION_TOKEN` | `api.notion.com` | no |
| monday-com-finite | `monday` | connection | `MONDAY_API_TOKEN` | `api.monday.com` | no |

A third-party CLI or SDK that reads its provider's standard variable uses
the swap unmodified once the catalog offers that provider:
`pdf-workbench-finite`'s `nano-pdf` with `GEMINI_API_KEY`, `parallel-cli`
with `PARALLEL_API_KEY`. Until then they work only with a key the person
gives for the task, or an account they log into from the CLI itself
(`parallel-cli-finite`, `inference-sh-cli-finite`).

## Its page

`site/` lists the skills by category and shows each one's `SKILL.md`,
reading the fragment's own `__files` and `__file`, so it shows what its
owner's agents have.
