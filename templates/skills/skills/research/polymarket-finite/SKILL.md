---
name: polymarket-finite
description: "Fetch public Polymarket market data with the bundled deterministic Python helper: prices, books, outcome-token history and trades. Read-only; no wallet execution."
version: 1.2.0
author: Hermes Agent + Teknium
tags: [polymarket, prediction-markets, market-data, trading]
---

## Deployment prerequisites

The bundled helper uses Python's standard library; no SDK or CLI package is installed. Public reads need **no API key**. Authenticated wallet/trading operations would require operator signing credentials that the deployment may not have; say so plainly if requested, and do not pretend this read-only helper supports them. Hermes' optional Polymarket skill and general research tools cover broader research; this skill owns the tested direct-data helper. [Current upstream data contract](https://github.com/Polymarket/agent-skills/blob/main/market-data.md).

Fragment names are `<label>--<suffix>`; people are email addresses. Provider handles, wallet addresses and Nostr pubkeys are separate identities. Never interpret them as Fragment names or people.


# Polymarket

Use the bundled helper script instead of hand-assembling curl requests.

No API key is needed for these public reads. History takes a **CLOB outcome token ID**, not a condition ID; `1m` means one month, and `--fidelity` is minutes between samples.

Script path:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py --help
```

## Workflow

1. `search` when the user asks about a topic or event
2. `event` or `market` once you have a slug
3. `price`, `book`, `history`, or `trades` for deeper market inspection

## Commands

Search:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py search \
  --query "OpenAI funding" \
  --limit 5
```

Trending events:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py trending \
  --limit 10
```

Specific event:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py event \
  --slug "some-event-slug"
```

Specific market:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py market \
  --slug "some-market-slug"
```

Token price and orderbook:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py price \
  --token-id "TOKEN_ID"

python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py book \
  --token-id "TOKEN_ID" \
  --limit 10
```

Price history and trades:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py history \
  --token-id "CLOB_OUTCOME_TOKEN_ID" \
  --interval 1m \
  --fidelity 30

python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py trades \
  --token-id "CLOB_OUTCOME_TOKEN_ID" \
  --limit 10
```

Machine-readable output:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/polymarket.py search \
  --query "OpenAI funding" \
  --json
```

## Notes

- Gamma API is best for discovery and slugs.
- CLOB API is best for prices, orderbooks, and history.
- Data API is best for recent trades.
- Prices are probabilities: `0.65` means `65%`.
- Gamma returns `outcomes`, `outcomePrices`, and `clobTokenIds` as JSON strings inside JSON; the helper script normalizes those for you.
