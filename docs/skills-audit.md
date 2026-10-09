# Bundled skills audit — 2026-10-08

41 managed skills (747,818 bytes including helpers/references) before the audit;
**12 kept and updated, zero upstream skills vendored/replaced, 29 deleted**.
The runtime's own skills and helpers now supply generic workflows. Apps' generic
design/game references also go; its four Fragment references stay.

## Evidence and upstream comparison

The primary source is the actual pinned desktop image, not Hermes master:
`nousresearch/hermes-agent:rc.4-v0.21.6-desktop@sha256:1343accb74013f38a22a11139ea7267bba9e8765d1236750c012f6b44a8ef127`.
Its install stamp is v0.21.6, commit `818c13be1dc4fd28987e1e881a9408224afd4535`,
built 2026-10-08. Read `/opt/hermes/skills`, `/opt/hermes/optional-skills`,
`agent/skill_utils.py`, `tools/skills_tool.py`, `tools/skills_hub_github.py`,
`tools/skills_hub_official.py`, `tools/web_tools.py` and `tools/image_generation_tool.py`
from it. The current p5 image (`cde979a27a22`) confirms the pre-audit removal and
missing document dependencies. This is an environment audit, not a replay of Paul's full model turn. The
coordinator's later p5 logs identify the skill the reviewer targeted, but do not
establish which rendering code produced the PDF.

All old managed skills were imported from the Finite skills snapshot in
`3ebc63e6` on 2026-10-03 (five days before this audit), then locally ported.
Declared versions below describe those files, not a verified upstream release.
Where no original revision is recorded, it is unknown; a `-finite` suffix is not
provenance or evidence of currency. Sizes are exact pre-change bytes per skill,
including helpers and references, excluding template site/README/manifest.

Hermes' official optional skills are its hub's highest-priority source; optional
means available to install, **not automatically present in an agent's catalog**.
Its default GitHub taps include [Anthropic](https://github.com/anthropics/skills),
OpenAI, Hugging Face, NVIDIA and gstack. Compared with:

- [Hermes v0.21.6](https://github.com/NousResearch/hermes-agent/tree/v0.21.6):
  the image's integrated PDF/Office helpers, native browser, web search/extraction,
  delegation, and official optional skills. The repo had ~252k stars at audit.
- [Anthropic skills](https://github.com/anthropics/skills/tree/683bc88e56f3e09ba94f7055977f3d3aa499f202/skills):
  latest commit on 2026-10-05; ~180k stars at audit. PDF/DOCX/PPTX/XLSX are
  source-available with restrictive document licenses, so do not copy them into
  our MIT distribution. Frontend-design is Apache-2.0 and a valid optional
  alternative; Hermes already includes design skills, so no new vendor copy.
- [obra/superpowers](https://github.com/obra/superpowers/tree/8ca22dba9a94f28898bbce59f2537ff4d87c747d):
  latest commit 2026-09-25; ~297k stars. Hermes already adapts its key code skills.
- [pbakaus/impeccable](https://github.com/pbakaus/impeccable/tree/c5ae03ee855256b8ea042f3bbd415e5c7ea7aca8):
  updated 2026-10-08; ~79k stars; Hermes offers impeccable 4.1.2 as optional.
- [Google Workspace CLI](https://github.com/googleworkspace/cli): ~31k stars,
  updated 2026-10-06; our image already pins gws 0.22.5 and adapts its token env.
  [nvk/llm-wiki](https://github.com/nvk/llm-wiki) supplies the retained brain
  method under MIT, not our Fragment search and file contract.

Popularity is evidence of maintenance/adoption, not proof of better output.
Prefer the pinned runtime's maintained integration over a second frozen copy.

## Inventory

Before this change **no Hermes bundled skill reached agent profiles**: the
Dockerfile deleted `/opt/hermes/skills`. Thus existing managed names did not
shadow an installed native skill, but generic task triggers competed among ours
and would compete with native skills once restored. All names differ from native
names except `grill-me` (Hermes optional). The overlap/facts column identifies
trigger competition and stale claims. `Bundled` below means upstream supplies a
skill; this PR makes those visible. Optional skills stay opt-in.

| Skill | Purpose | Source / declared version | Bytes | Hermes / newer upstream capability | Overlap or platform issue; decision |
|---|---|---|---:|---|---|
| meme-from-template-finite | Pillow meme composition | Finite local, unpinned | 5,235 | Optional meme-generation 2.0.0; not identical to manual composition | Meme triggers; false blanket Cairo/runtime claims; delete generic copy |
| cocod-finite | Cashu/Lightning wallet | cocod published 0.0.16 (latest, 2026-03-29) | 6,158 | No bundled Cashu equivalent; upstream 0.0.17 is private/unpublished | Keep/update per Paul: pin published 0.0.16, verified CLI; wallet credentials/funds prerequisite; Cashu-only trigger |
| trading-agent-finite | Market research and charts | Finite local; current six-package chart stack pinned 2026-10-09 | 6,430 | Optional stocks/finance skills; native web research replaces deleted Perplexity helper | Keep/update per Paul: multi-source analysis/charts; tested current imports/render; public data keyless, authenticated actions need credentials |
| grill-me | Stress-test a plan | Finite snapshot, original revision unknown | 635 | Optional grill-me 2.0.0 | Same name if optional installed; planning triggers; delete generic copy |
| image-generation-finite | Metered Fragment image job | Fragment rewrite, unversioned | 5,652 | Native image_generate uses FAL/Nous, not our Workers AI step | Image triggers; remove edit trigger and deleted meme reference; keep/update |
| inference-sh-cli-finite | External AI apps via infsh | okaris 1.0.0 | 14,753 | Optional inference-sh-cli 1.0.0 | Broad AI/image/video triggers; own login, outside platform; delete generic copy |
| find-nearby-finite | Keyless nearby-place lookup | Finite local 1.0.0 | 10,311 | Bundled maps 1.2.0 has geocoding/nearby/routing | Places triggers with goplaces/maps; delete generic copy |
| goplaces-finite | Google Places API lookup | Finite local 1.0.0 | 9,868 | Bundled maps 1.2.0 is keyless; no identical Google ratings helper | Places triggers; vendor API, not Fragment-specific; delete generic copy |
| music-generation-finite | ElevenLabs/FAL music | Finite REST examples 1.0.0; ElevenLabs music_v2_5 | 5,000 | Bundled songwriting-and-ai-music covers composition; official ElevenLabs skill recommends v2.5 | Keep/update per Paul: direct provider execution only; explicit credential check and current model; no general songwriting competition |
| nostr-agent-interface-cli-finite | Nostr external CLI | AustinKelsay skill 0.1.0; CLI source 3.1.0 at f19c8478 | 11,056 | No bundled equivalent; npm package absent, current upstream source builds | Keep/update per Paul: pinned source build verified; signer prerequisite, public npub is not a private key; CLI-specific trigger |
| generate-pdf-finite | Create PDFs with fpdf2/ReportLab | Community 1.1.0 | 7,402 | Bundled pdf 1.1.0 has JSON creator and validation helpers | PDF creation triggers; dependencies absent; Unicode claim wrong; delete generic copy |
| google-workspace-finite | Connected Google APIs/gws | Nous-derived Fragment rewrite 2.1.0 | 28,748 | Bundled google-workspace 1.2.0 uses local OAuth; wrong auth here | Same task trigger; deliberately omit native; keep/update to 2.2.0 |
| linear-finite | Linear GraphQL helper | Hermes-derived 1.1.0 | 20,381 | No identical bundled skill; vendor/hub when needed | No default Linear connection; optional third-party integration; delete generic copy |
| monday-com-finite | Monday GraphQL | Fragment port, unversioned | 2,225 | No identical bundled skill; vendor/hub when needed | No default Monday connection; standard API needs no fork; delete generic copy |
| nano-pdf-finite | AI editing of PDF pages | Community 1.0.0 | 1,501 | Bundled pdf references/nano-pdf-editing.md | PDF editing triggers; uninstalled CLI and absent Gemini key; delete generic copy |
| notion-finite | Notion REST API | Community 1.0.0 | 8,682 | Bundled notion 2.0.0 | Notion triggers; native skill is newer; no default token; delete generic copy |
| ocr-and-documents-finite | OCR and Office extraction | Hermes-derived 2.3.0 | 12,842 | Bundled pdf OCR references, docx/powerpoint/xlsx 1.1.0 | PDF/Office triggers; installs missing libraries; redundant; delete generic copy |
| pdf-workbench-finite | nano-pdf edit and review | Finite local 1.0.0 | 6,181 | Bundled pdf editing and rasterization helpers | PDF editing triggers; iframe screenshots unreliable, Gemini absent; delete generic copy |
| tufte-viz-finite | Analytical visualization design | Finite local, unpinned | 17,834 | Native design skills; no exact Tufte skill; install specialist if wanted | Visualization triggers; generic design, not platform-specific; delete generic copy |
| arxiv-finite | arXiv/Semantic Scholar research | Hermes-derived 1.1.0 | 17,095 | Bundled arxiv 1.0.0 and grounded-citations; not an exact feature match | Paper-search triggers; a local version number does not prove superiority; delete generic copy |
| blogwatcher-finite | RSS/blog tracking | Hyaxia/blogwatcher, community 1.0.0 | 1,246 | Optional blogwatcher 2.0.0 and rss-feeds | RSS triggers; newer upstream optional copy; delete generic copy |
| brain-finite | Fragment brain files/search/wiki | Fragment + nvk/llm-wiki MIT, unversioned | 7,162 | Bundled llm-wiki has wiki method, not the Fragment search/file API | Wiki triggers, intentional platform specialization; email/full-name update |
| domain-intel-finite | Domain reconnaissance | FurkanL0, unversioned | 21,032 | Optional domain-intel 1.0.0 | Domain-research triggers; duplicate snapshot; delete generic copy |
| duckduckgo-search-finite | DDGS keyless web search | gamedevCloudy 1.2.0 | 6,375 | Native web_search DDGS backend; optional duckduckgo-search 1.3.0 | Search triggers; native tools / newer optional skill; delete generic copy |
| model-council-finite | Panel through Fragment model tiers | Fragment rewrite, unversioned | 13,367 | Native delegate_task is not a multi-tier metered panel | Narrow to explicit tier opinions; not multiple frontier vendors; keep/update |
| parallel-cli-finite | Parallel research CLI | Hermes-derived 1.1.0 | 10,946 | Optional parallel-cli 1.1.0; native Parallel web backend | Research triggers; external credentials/login; delete generic copy |
| perplexity-research-finite | Perplexity research/Firecrawl | Finite local, unpinned | 12,143 | Native web_search supports PERPLEXITY_API_KEY; grounded-citations | Research triggers; native standard env works with swap; Firecrawl absent; delete generic copy |
| polymarket-finite | Prediction-market data | Hermes + Teknium helper 1.1.0; current public data contract | 17,267 | Optional Hermes Polymarket 1.0; official Polymarket agent-skills confirms token history | Keep/update per Paul: tested read-only stdlib helper; fix history token ID and fidelity units; authenticated trading needs a separate operator signer |
| research-paper-writing-finite | ML paper pipeline | Orchestra Research 1.0.0 | 176,722 | Optional research-paper-writing 1.1.0; grounded-citations | Paper-writing triggers; 176,722 bytes, old conference links; delete generic copy |
| x-api-finite | X API v2 lookups | Finite stdlib helper, X API v2 | 13,653 | Bundled xurl covers OAuth/posting; direct app-bearer reads are distinct | Keep/update per Paul: operator bearer prerequisite, 30s timeout; narrow to read-only direct API data |
| x-search-finite | Grok X search | waffledog-bot/paul-and-waffle / OpenUniverse, unpinned | 13,722 | Native web tools can use xAI backend; not identical analysis modes | Search triggers; standard XAI_API_KEY swap needs no duplicate skill; delete generic copy |
| apps-finite | Build/publish/share Fragment apps | Fragment rewrite + Finite design references, unversioned | 190,413 | Native popular-web-designs/design-md; optional impeccable 4.1.2 lack Fragment runtime API | Web-design triggers; remove generic references, fix names and private QA; keep/update |
| code-review-finite | Generic code review | Finite local, unpinned | 2,218 | Bundled requesting-code-review 2.1.0 / sdlc-review | Code-review triggers duplicate guidance; delete generic copy |
| git-finite | Fragment repo sync/history/rollback | Fragment rewrite, unversioned | 4,621 | Bundled github is not Fragment repository/sync semantics | Git triggers, intentional platform specialization; fix double hyphen; keep/update |
| impeccable-finite | Web visual-design audit | Finite local adaptation, unpinned | 13,227 | Optional impeccable 4.1.2; upstream pbakaus/impeccable; bundled design skills | Web-design triggers; duplicated house design rules; delete generic copy |
| plan-finite | Read-only planning mode | Hermes-derived 1.0.0 | 2,090 | Hermes native planning/config/workflows; upstream superpowers plans | Planning triggers; unnecessary private copy; delete generic copy |
| requesting-code-review-finite | Request systematic review | Hermes/obra-superpowers 1.1.0 | 6,177 | Bundled requesting-code-review 2.1.0 | Code-review triggers; native is newer; delete generic copy |
| subagent-driven-development-finite | Delegate implementation | Hermes/obra-superpowers 1.1.0 | 9,871 | Native delegate_task; optional subagent-driven-development 1.1.0 | Delegation triggers; optional workflow rather than global mandate; delete generic copy |
| systematic-debugging-finite | Root-cause debugging | Hermes/obra-superpowers 1.1.0 | 10,573 | Bundled systematic-debugging 1.1.0; upstream obra/superpowers | Debug triggers; duplicate fork; delete generic copy |
| test-driven-development-finite | Test-first coding | Hermes/obra-superpowers 1.1.0 | 9,653 | Bundled test-driven-development 1.1.0; upstream obra/superpowers | Coding/test triggers; duplicate fork; delete generic copy |
| writing-plans-finite | Detailed coding plans | Hermes/obra-superpowers 1.1.0 | 7,351 | Upstream obra/superpowers plans; Hermes planning/delegation tools | Planning triggers; optional specialist, not platform contract; delete generic copy |

## The PDF case

Before the change, “make me a PDF report” could select
`generate-pdf-finite` (fpdf2/ReportLab snippets), with overlapping OCR and PDF
editing descriptions. Hermes' native `pdf` was absent from agent profiles.
There is no dedicated PDF tool: the agent loads a skill and executes helpers
with `terminal`. The old generation skill incorrectly claimed broad Unicode
support in ReportLab's core Helvetica/Courier fonts and taught layout from
scratch; the workbench proposed screenshots of an embedded PDF viewer rather
than deterministic page rasterization. We cannot prove which path p5 chose
without that turn's transcript.

Verified actual imports/tools in both the pinned base and pre-change p5 image,
using `/opt/hermes/.venv/bin/python`, not a host or unrelated system Python:

| Capability | Before | This PR |
|---|---|---|
| ReportLab | absent | 5.0.1, imported; JSON helper rendered a real PDF |
| pypdf / pdfplumber | absent | 6.19.0 / 0.11.10, imported/read real PDFs |
| PyMuPDF / pypdfium2 | absent | 1.28.2 / 5.13.0, imported; rasterized every sample page |
| python-docx / python-pptx / openpyxl | absent | 1.2.0 / 1.0.2 / 3.1.5, imported |
| fpdf2 / WeasyPrint | absent | not added; two supported paths suffice |
| Pandoc / LaTeX (latex, pdflatex, xelatex, lualatex, tectonic) | absent | not added |
| Chromium print-to-PDF | installed | tested the pinned Hermes Chromium, not a downloaded browser |
| poppler / LibreOffice / nano-pdf | absent | optional; PDF rasterization uses pypdfium2 |
| Python Playwright | absent | not needed for offline CLI printing |

Dependencies and transitive versions/hashes are locked in
`images/hermes/document-requirements.txt`. Its transitive dependencies already
present in Hermes (Pillow, cryptography, cffi, charset-normalizer, pycparser,
typing-extensions) resolve to the same versions as the pinned base. The
installation does not move Hermes to a different dependency version.

Two offline paths were rendered with `docker run --network none`, no model:

1. [Structured JSON input](explorations/skills-audit/report.json), through
   Hermes' unmodified `skills/productivity/pdf/scripts/pdf_create.py`:
   **2 pages, 3,365 bytes**, selectable content. Clean headings, tables and page
   numbers, but plain default typography. Least model effort: emit a small
   JSON spec, run the existing helper, render and inspect pages.
2. [HTML/CSS input](explorations/skills-audit/report.html), through the pinned
   `/opt/hermes/tools/chromium-1208/chrome-linux64/chrome` with
   `--headless --no-sandbox --disable-dev-shm-usage --no-pdf-header-footer
   --print-to-pdf=<output> file://<input>`:
   **2 pages, 40,142 bytes**, selectable text and embedded fonts. Visually stronger
   hierarchy, whitespace, accent color, full-width tables and explicit page
   breaks; no manual drawing coordinates or PDF library code. This is the
   preferred path when the person asks for an attractive report.

All four pages were rasterized through Hermes' `pdf_page_image.py` at 100 DPI
and visually inspected: no clipping, missing content, broken table alignment
or extra pages. Text read-back checked both headings, the metrics and the
no-model statement (normalized whitespace). The system `/usr/bin/chromium`
also rendered successfully, but the recorded comparison uses Hermes' pinned
Chromium, which our runtime wrapper names. Generated PDFs and PNGs stayed in
scratch; the small inputs are committed for reproduction.

After this change the native `pdf` is in every agent's index, whether or not its
owner has a skills fragment. The platform skill gives a short route to its JSON
creator or Chromium and tells the agent to inspect rendered pages. This makes
better output easier; it does not establish that a live GLM turn will always
choose the better design. No paid model calls or p5 mutations were made.

## p5 background review / curator evidence

The coordinator supplied additional Workers Logs evidence from Paul's first p5
chat (2026-10-08): a background patch targeted `generate-pdf-finite`, logged
“Refusing background curator patch for skill 'generate-pdf-finite'”, then hit a
`PermissionError` beneath `/data/hermes/managed-skills/`. There were three
background model requests lasting 26–33 seconds each. This establishes that
background maintenance targeted our PDF skill; it does not reveal the proposed
patch's content or prove it would have made a better document.

Read the actual pinned image's `agent/background_review.py`,
`agent/turn_finalizer.py`, `agent/curator.py`, `run_agent.py`,
`tools/skill_manager_guards.py`, `tools/skill_manager_tool.py`,
`tools/skill_usage.py` and `hermes_cli/config_defaults.py`.
[Upstream curator documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/curator)
describes the separate periodic service; the pinned code resolves the distinction:

| Mechanism | v0.21.6 default / gate | Managed external skills |
|---|---|---|
| Per-turn self-improvement review | `auxiliary.background_review.enabled: true`; skill trigger after 10 tool iterations (`skills.creation_nudge_interval`), or a memory trigger; up to 16 review iterations, main model/runtime by default | Prompt prioritizes patching the loaded skill regardless of author. Existing content requires a fresh review-time read, then writes are attempted even for external skills. |
| Periodic curator | `curator.enabled: true`, weekly interval, at least two idle hours; `consolidate: false`, `prune_builtins: false`; first observation defers a run | External skills are excluded from eligibility, adoption and autonomous deletion. Default deterministic pruning makes no model calls. |

The observed patch refusal matches the **read-before-write guard**, not an
external-ownership patch guard. The review is told to reread the skill and retry
once. `_locate_for_write` checks external ownership for **delete only**;
`_guarded_write` then tries an atomic write. Our root-owned files correctly stop
that write at the OS boundary. Even the periodic curator's deletion/eligibility
protection does not provide a universal external-content-write guard for the
per-turn review fork.

Why would it try? The review prompts prioritize learning from corrections,
complaints about formatting, and non-trivial techniques, and prefer changing the
skill used for the task over saving a separate memory. A patch attempt is an
incentivized learning action, not an independent quality assessment. Ours does
have independently demonstrated weaknesses: missing libraries, wrong core-font
Unicode claims, overlapping PDF triggers, and manual layout snippets where
Hermes already supplies a structured renderer. Delete `generate-pdf-finite` and
the other PDF copies; use native `pdf` with the baked dependencies and the tested
Chromium path. Do not let a background model rewrite a shared release's bytes.

Keep managed and native external views **read-only**. Making them writable would
let one agent change every profile's shared instructions, bypass the skills
fragment's versioned edits, and lose changes at refresh/restart. For deliberate
customization, an agent-owned skill in its own fragment/profile can override the
native name and retain its history.

There is no v0.21.6 per-path/per-managed-skill background-review exclusion. Turning
`curator.enabled` off alone does **not** stop the observed per-turn model calls.
The supported automatic-review switch is `auxiliary.background_review.enabled`;
it also controls automatic memory reviews. Explicit skill/memory tools remain
available; an explicit `/refine` focus bypasses that automatic gate in Hermes
(the Fragment bridge does not forward slash commands). The separate periodic
curator can retain its safe default policy for agent-owned skills.

**Paul's revised decision, 2026-10-09:** automatic memory review is on.
Automatic skill review is on too, with the image-owned `fragment-skill-fork`
plugin enabled in each profile. The single review setting is now
`auxiliary.background_review: { enabled: true }`; both scheduling intervals
retain Hermes' defaults. The periodic curator and explicit tools stay available.

The plugin uses Hermes' supported `pre_tool_call` extension point. Before a flat
`skill_manage` patch/edit/write_file/remove_file on either of our external roots,
it copies the complete category/name package into the active profile's `skills/`.
Local names rank above external names. It transfers only exact-file fresh-read
marks, preserving the background review read-before-write rule and native
validation/security scanning. Copies are bounded, reject links/special files,
are writable, and are published under an interprocess lock without replacing
existing local content. External batch writes are refused with guidance to fork
via a single write first; deletes retain native ownership guards. Other external
roots keep native behavior. No Hermes source file is changed for this feature.
A failed subsequent patch can leave an unchanged local fork; removing that fork
restores the current managed baseline. Later managed releases never overwrite it.

Pinned `/opt/hermes` references: `hermes_cli/config_defaults.py:811` (global review
switch), `agent/agent_init.py:1325` (memory interval), `:1382` (skill interval),
`agent/turn_finalizer.py:760` (skill scheduling), `:784` (review spawn),
`agent/background_review.py:1081` (memory-triggered reviews still allow skill tools),
`tools/skill_manager_tool.py:384` (external lookup before writes), `:395` (in-place
write), `tools/skill_manager_guards.py:211` (fresh-read guard), and
`hermes_cli/plugins.py:1971` (supported pre-tool hook). There is no per-managed-skill
switch or native/configured fork-to-local facility. Setting only
`skills.creation_nudge_interval: 0` would suppress skill-triggered forks but would
not remove skill tools from memory-triggered reviews; it cannot guarantee no
read-only failures. The image plugin resolves the write target instead.

The Docker lane runs a scripted background-origin fresh-read/patch through real
plugin dispatch: the initial unread patch is refused, then a complete local copy
is patched, helpers remain intact, native resolution selects the copy, and the
baseline remains unchanged. An unread helper is still refused after forking.
[Debt entry](technical-debt-ledger.md#hermes-external-skills-need-an-image-copy-on-write-plugin).

The three calls represent 78–99 seconds of request duration, not necessarily an
equal addition to first-reply wall time: v0.21.6 starts reviews after delivery and
cancels an existing review at a subsequent live turn with a bounded two-second
acknowledgement wait. They still spend model budget and can contend with live
work. Without the full timing trace, do not claim all of that duration was added
to Paul's visible wait. Deleting our PDF copy fixes the bad routing/dependencies;
automatic review can still improve the read-only native PDF by forking it locally.
Deleting the duplicate alone would not have resolved the ownership conflict.

## Changes and verification

- Managed release: six Fragment contracts plus six operator integrations Paul retains; 29 generic copies deleted.
  Generic apps references replaced with routes to Hermes' own design skills.
- Image: pinned Hermes skills exposed via a read-only temporary view outside saved
  profiles. A managed name or alias suppresses its native counterpart; removal
  restores it. Agent-owned skills keep Hermes' higher precedence. The view uses real copies,
  so Hermes' resolved-path trust check does not warn on outside symlinks. Optional hub
  skills remain optional. Native Google OAuth is deliberately excluded.
- Document helpers: wheel-only, SHA-256-checked install at image build;
  no first-turn pip install into Hermes' sealed environment.
- Stale facts: `<label>--<suffix>`, emails for people, narrowed image/council
  triggers, no references to deleted skills, private browser QA without widening
  sharing, and container Chromium flags / detached QA servers.
- Docker test: all twelve real managed entrypoints, native document/code/research
  catalog entries, Google exclusion, read-only skills, actual PDF creation and
  read-back, agent-owned precedence, managed PDF shadow/removal and restoration.
  Unit tests also cover aliases, replay and fresh-view restoration. The image test
  checks automatic memory/skill review is on and writes fork locally; explicit tools remain available,
  and the periodic curator retains its no-LLM-consolidation default. All six restored
  headers pass Hermes' YAML parser, and the lane asserts structured metadata
  survives discovery rather than relying on its flat fallback parser.

Required checks, recorded for this branch:

- `cargo xtask check`: 529 host tests passed, 1 ignored; JavaScript syntax,
  host clippy and wasm clippy passed, warnings denied.
- Images `cargo test --workspace`: 201 passed, 15 Docker tests ignored.
- Images `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test -p fragment-bridge --test docker -- --ignored the_hermes_image`:
  1 passed, 16 filtered; unique tag `fragment-hermes:skills-audit-20261009-3edwyqbi-metadata`.
- Offline PDF comparison: 2 paths, 4 pages visually inspected; both content checks passed.

Paul explicitly keeps automatic memory review; model-call budget tuning remains his call.
Paul/coordinator's calls: merge and deploy this image/platform release, then
update existing computers' image pins. A platform deploy alone updates managed
files but does not replace the pinned image of an existing computer. The six restored integrations are available in the catalog, with explicit missing-credential guidance.

## Restored integrations — 2026-10-09

Paul retained cocod, Nostr CLI, X API, music, trading analysis and Polymarket; Linear
and Monday remain deleted. Every restored skill distinguishes provider identities
from Fragment `<label>--<suffix>` names and email people, and explicitly explains
missing operator credentials. Cashu wallets, public Nostr reads, public market
data and public Polymarket reads do not falsely require a shared API key. Signed
or authenticated actions need separately provisioned wallet/signer/exchange material.

- cocod: current published 0.0.16 installed and `--version`/`--help` ran in the pinned
  image with Bun 1.4.2; private upstream 0.0.17 is not a compatible npm upgrade.
- Nostr: npm 404; pinned source 3.1.0 built from its frozen lockfile and its real
  CLI `list-tools --json` ran in the pinned image. No signing key used.
- X: stdlib helper, current official recent-search contract, explicit missing-token
  error and bounded network timeout; no paid API request or token used.
- Music: current ElevenLabs `music_v2_5` explicitly selected; direct operator-backed
  requests separated from Hermes songwriting. Provider contracts inspected; no paid generation call.
- Trading: six current PyPI pins installed in a separate environment and imported;
  Plotly/Kaleido rendered a real chart through the pinned Chromium offline. `uv
  --no-config` avoids inheriting Hermes' older dependency cutoff in the user's environment.
- Polymarket: no SDK install; history corrected to token IDs, month interval and
  minute-spacing fidelity using the official vendor contract; deterministic helper check
  and live public price/month-history requests passed (1,441 history samples).
