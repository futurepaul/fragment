# Bundled skills audit — 2026-10-08

41 managed skills (747,818 bytes including helpers/references) before the audit;
**six kept and updated, zero upstream skills vendored/replaced, 35 deleted**.
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
missing document dependencies. This is an environment audit, not a replay of
Paul's original model turn; it cannot identify which skill that turn loaded.

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
| cocod-finite | Cashu/Lightning wallet | cocod 0.0.16 metadata | 6,158 | No bundled Cashu equivalent; use vendor skill when requested | Wallet/payment triggers; obsolete exact npm pin; delete generic copy |
| trading-agent-finite | Market research and charts | Finite local, unpinned | 6,430 | Optional stocks 0.1.0 / finance skills; not a full equivalent | Charts/market triggers; not a Fragment contract; delete generic copy |
| grill-me | Stress-test a plan | Finite snapshot, original revision unknown | 635 | Optional grill-me 2.0.0 | Same name if optional installed; planning triggers; delete generic copy |
| image-generation-finite | Metered Fragment image job | Fragment rewrite, unversioned | 5,652 | Native image_generate uses FAL/Nous, not our Workers AI step | Image triggers; remove edit trigger and deleted meme reference; keep/update |
| inference-sh-cli-finite | External AI apps via infsh | okaris 1.0.0 | 14,753 | Optional inference-sh-cli 1.0.0 | Broad AI/image/video triggers; own login, outside platform; delete generic copy |
| find-nearby-finite | Keyless nearby-place lookup | Finite local 1.0.0 | 10,311 | Bundled maps 1.2.0 has geocoding/nearby/routing | Places triggers with goplaces/maps; delete generic copy |
| goplaces-finite | Google Places API lookup | Finite local 1.0.0 | 9,868 | Bundled maps 1.2.0 is keyless; no identical Google ratings helper | Places triggers; vendor API, not Fragment-specific; delete generic copy |
| music-generation-finite | ElevenLabs/FAL music | Finite local 1.0.0 | 5,000 | Bundled songwriting-and-ai-music; native audio generation is not an identical provider | Music triggers; optional vendor credentials; delete generic copy |
| nostr-agent-interface-cli-finite | Nostr external CLI | Finite port, original revision unknown | 11,056 | No identical bundled skill; use vendor instructions/hub | No name collision; external CLI unrelated to Fragment; delete generic copy |
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
| polymarket-finite | Prediction-market data | Hermes Agent + Teknium 1.1.0 | 17,267 | Optional polymarket 1.0.0; not identical helper coverage | Prediction-market triggers; user-installed specialty; delete generic copy |
| research-paper-writing-finite | ML paper pipeline | Orchestra Research 1.0.0 | 176,722 | Optional research-paper-writing 1.1.0; grounded-citations | Paper-writing triggers; 176,722 bytes, old conference links; delete generic copy |
| x-api-finite | X API v2 lookups | Finite local, unpinned | 13,653 | Bundled xurl; not identical credential contract | X triggers; X bearer provider absent from default catalog; delete generic copy |
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

## Changes and verification

- Managed release: six Fragment contracts; 35 generic copies deleted.
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
- Docker test: all six real managed entrypoints, native document/code/research
  catalog entries, Google exclusion, read-only skills, actual PDF creation and
  read-back, agent-owned precedence, managed PDF shadow/removal and restoration.
  Unit tests also cover aliases, replay and fresh-view restoration.

Required checks, recorded for this branch:

- `cargo xtask check`: 529 host tests passed, 1 ignored; JavaScript syntax,
  host clippy and wasm clippy passed, warnings denied.
- Images `cargo test --workspace`: 201 passed, 15 Docker tests ignored.
- Images `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test -p fragment-bridge --test docker -- --ignored the_hermes_image`:
  1 passed, 16 filtered; unique tag `fragment-hermes:skills-audit-20261008-3edwyqbi`.
- Offline PDF comparison: 2 paths, 4 pages visually inspected; both content checks passed.

Paul/coordinator's calls: merge and deploy this image/platform release, then
update existing computers' image pins. A platform deploy alone updates managed
files but does not replace the pinned image of an existing computer. Specialty
vendor/wallet skills can be installed explicitly when needed; the generic
managed set no longer promises them to every agent.
