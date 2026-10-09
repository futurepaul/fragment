---
name: x-api-finite
description: Read exact X API v2 post data, recent-search results and profiles with the operator application bearer token and bundled Python helper. Use xurl for user OAuth or posting.
---

## Deployment prerequisites

Requires the operator's `X_API_BEARER_TOKEN` and an X API plan with access to the requested endpoint. This deployment may not offer that credential: check it is set, then say plainly when unavailable; do not keep retrying or hunt for keys. Only Python's standard library is required; no pip package or CLI is installed. Hermes' bundled `xurl` owns user OAuth and write actions. This helper is read-only application-bearer access. [Current recent-search contract](https://docs.x.com/x-api/posts/search-recent-posts).

Fragment names are `<label>--<suffix>`; people are email addresses. Provider handles, wallet addresses and Nostr pubkeys are separate identities. Never interpret them as Fragment names or people.


# X API

Use the official X API v2 when you need exact post data instead of Grok search summaries.

Use this skill when:

- the human pasted an `x.com/.../status/...` or `twitter.com/.../status/...` URL
- you need the real post text, author, metrics, or media metadata
- browser access is failing because X blocks or rate-limits web scraping
- you want recent-search results from the actual X API, not LLM synthesis

## Setup

Your computer holds no keys: `X_API_BEARER_TOKEN` holds the operator's
key's placeholder (it names you), the helper sends it to `api.x.com` as a
bearer token, and the computer swaps in the real one, metered to your
owner. Unset, this deployment does not offer the `x` key (the platform
does not yet): say so rather than hunting for one.

Use the local helper directly:

```bash
python3 ${HERMES_SKILL_DIR}/x-api.py lookup https://x.com/jack/status/20
python3 ${HERMES_SKILL_DIR}/x-api.py search "from:jack nostr" --limit 5
python3 ${HERMES_SKILL_DIR}/x-api.py user @jack
python3 ${HERMES_SKILL_DIR}/x-api.py conversation https://x.com/jack/status/20 --limit 10
```

## Workflow

1. If the human pasted a specific X URL, start with `lookup`.
2. If they want the surrounding replies, use `conversation`.
3. If they want broader discovery, use `search`.
4. If they want exact account metadata, use `user`.
5. Prefer this skill over browser-based X inspection unless the task specifically needs rendered UI.

## Notes

- `lookup` accepts full URLs, bare status IDs, or multiple inputs at once.
- `search` uses the X v2 recent-search endpoint and returns exact post links plus metrics.
- `conversation` is a convenience wrapper over recent search using `conversation_id:<tweet_id>`.
- This skill is read-only. It does not post, like, or follow.
