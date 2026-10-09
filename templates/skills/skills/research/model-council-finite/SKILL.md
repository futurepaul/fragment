---
name: model-council-finite
description: Use when the user explicitly asks for a council, panel, jury, debate, or multiple opinions from Fragment's model tiers to weigh an important decision, plan, research question, code direction, or strategy.
---

# Model Council

Use this skill only when the user explicitly asks for a model council, a debate
between Fragment model tiers, or multiple independent model opinions. This skill is
intentionally expensive compared with a normal answer.

## Workflow

1. Restate the question in one sentence.
2. Choose the closest mode:
   - `decision` for choosing between options.
   - `strategy` for product, business, roadmap, or architecture direction.
   - `research` for an evidence-seeking question.
   - `code-review` for implementation risk and software design critique.
3. If the question depends on current facts, benchmark numbers, vendor docs,
   prices, model availability, legal/compliance details, or citations, gather
   source notes first with the appropriate research skill. Put those notes in a
   file and pass them with `--input-file`.
4. Run the council script:

```bash
python3 ${HERMES_SKILL_DIR}/scripts/model_council.py \
  --mode decision \
  --question "Should we do X or Y?"
```

5. Read the full output before answering the user.
6. Give the user the synthesis, the strongest disagreements, and your own final
   recommendation.

## Notes

- The council is the platform's model tiers, called through your computer's
  model intercept as you (`x-fragment-agent: $FRAGMENT_AS_AGENT`), each call
  metered to your owner: `medium` and `cheap` by default, and `high` while
  the deployment has it on. Other vendors' frontier models are not offered
  here, so this panel is narrower than a cross-vendor one: say so when the
  user asked for "multiple frontier models".
- Override the default panel with `MODEL_COUNCIL_MODELS` or `--models`, using
  comma-separated tiers (`cheap`, `medium`, `high`).
- For sensitive tasks, do not send secrets, private keys, passwords, tokens, or
  unnecessary private data into the council prompt.
- For research claims that require citations, use a grounded research skill
  first, then feed the source notes into the council.
- Treat council output as deliberation, not proof. If the panel gives concrete
  numbers without supplied sources, label them as estimates before presenting
  them to the user.
