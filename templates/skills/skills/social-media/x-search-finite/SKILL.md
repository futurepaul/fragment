---
name: x-search-finite
description: Search and analyze X/Twitter using Grok's x_search. Topic search, account analysis, side-by-side debate classification, and freeform questions.
---

# X Search

Search and analyze X/Twitter content using Grok + x_search. Adapted from [OpenUniverse](https://github.com/AnthonyRonning/openuniverse) and pulled from [paul-and-waffle](https://github.com/waffledog-bot/paul-and-waffle/tree/main/skills/x-search).

## Setup

Needs only Python's standard library. Your computer holds no keys: the
helper calls xAI's REST Responses API with the operator's key placeholder
`fragment-key:xai` (hosts `api.x.ai`) and `x-fragment-agent:
$FRAGMENT_AS_AGENT`, and the computer swaps in the real key, metered to your
owner. `XAI_API_KEY`, when set, is sent instead. A 401 means this deployment
does not offer the `xai` key: say so rather than hunting for one.

## Commands

Use the local helper directly:

```bash
SKILL_ROOT="${HERMES_SKILL_DIR}"
python3 "$SKILL_ROOT/x-search.py" search "bitcoin etf" --limit 5
python3 "$SKILL_ROOT/x-search.py" topic "AI regulation" --sides "Pro-regulation|Anti-regulation" --limit 10
python3 "$SKILL_ROOT/x-search.py" account @elonmusk --topics "AI,Bitcoin,Free speech,Mars"
python3 "$SKILL_ROOT/x-search.py" ask @jack "What does he think about Nostr?"
```

## Output

All output is markdown. Pipe to a file to save reports:

```bash
python3 "$SKILL_ROOT/x-search.py" account @jack --topics "Bitcoin,Nostr,Bluesky" > reports/jack-analysis.md
```

## How It Works

This skill uses Grok with the built-in `x_search` tool, so it only needs the xAI key and does not require separate X API credentials.

- Model: defaults to `grok-4-1-fast`
- Only Grok 4+ supports `x_search`
- Output is citation-heavy markdown intended for saving or synthesis

## Features

- Topic search with engagement summaries
- Two-sided topic classification
- Account analysis across configurable topics
- Freeform account questions
- Markdown reports with tweet links

## Notes

- Prefer `search` or `ask` for broader investigations; `account` is best when you already know the handle you care about.
- Ask for high-engagement or viral tweets if you want better signal.
- The `topic` command depends on Grok returning parseable JSON between phases, so `ask` is often more reliable for complex investigations.

## Sources

- Upstream skill: https://github.com/waffledog-bot/paul-and-waffle/tree/main/skills/x-search
- OpenUniverse reference: https://github.com/AnthonyRonning/openuniverse
