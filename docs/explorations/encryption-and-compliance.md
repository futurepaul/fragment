# Encryption at rest and the compliance vector

Exploration 4, evaluated 2026-09-07. Two questions: how does the thin
wrapper (celld + s3 + code.storage, all swappable) get encrypted at rest,
and what would "industry grade" / SOC 2 / HIPAA actually cost? Sources:
our own `docs/encryption-research.md` (the prior investigation — still
accurate), the celld repo, code.storage's docs, and finite-mono's
finite-brain ADRs.

## Where plaintext lives

| Layer | Contents | State |
|---|---|---|
| celld node (VPS) | resident cell SQLite (RAM), 512 MiB asset disk cache | plaintext; FDE is ours |
| the bucket | LTX WAL (SQLite pages), assets, R2-mapped blobs | plaintext — celld has no at-rest story (verified: nothing in docs; "treat bucket access as fleet administrator access") |
| code.storage | repo contents | inside their attested boundary |
| transit | client↔ingress TLS; peer traffic | celld's answer for peers is an overlay (WireGuard/Tailscale) |

Key verified fact: **celld delegates at-rest entirely to the bucket
provider.** There is no WAL encryption, no KMS hook, no SQLCipher. So the
encryption story is a menu that maps onto the research doc's threat
ladder.

## The menu

- **Level (b) — substrate encryption (zero code, do now).** Bucket SSE
  (S3 SSE / GCS default / R2 at-rest), VPS full-disk encryption,
  WireGuard/Tailscale peer overlay per celld's own docs. This is what
  most SOC 2 shops actually run. Label honestly per the research doc:
  tamper-evidence and disk-thief protection, not privacy from the
  operator.
- **Level (c) fix — wrap what the cell holds (small, already designed).**
  Research doc Stage 1: the fragment npub secret is generated *inside*
  the cell today (`registry.ts`); move generation client-side and store
  the cell's copy wrapped under `FRAGMENT_HOST_SECRET`; wrap the secrets
  table at rest. No behavior change.
- **Level (d) — opt-in `--private` fragments (research doc Stage 2).**
  One root key per fragment derived from the user's nostr secret,
  AES-SIV deterministic filenames (cell keeps routing/listing/drafts),
  ChaCha20/AES-GCM content envelopes, age-style multi-recipient wraps to
  editor npubs, printable recovery key. Private fragments accept a
  restricted contract (no public serving, snapshot reads, restricted
  ctx) — the fork the research doc proved unavoidable, scoped to opt-in.
- **Upstream ask, not ours:** encrypted WAL frames under a fleet key
  would close the bucket gap for *all* celld users. Fits their threat
  model; worth an issue, not a fork.

## Provider-swap nuance

Thin wrapper over swappable providers ⇒ prefer **provider-agnostic
encryption**: generic bucket SSE and client-side crypto survive
S3↔GCS↔R2↔code.storage swaps; SSE-KMS with provider-managed keys welds
you to that provider. Crypto sits at the two layers every provider
shares: bucket-level and app-level.

## What finitebrain does, and the slice worth stealing

finite-brain-core owns per-folder keys, AES-256-GCM envelopes with
domain-bound AAD (`brainId`, `folderId`, `objectId`, `keyVersion`),
NIP-44/59 primitives, ciphertext-only store, client-owned atomic grant
workflows, deny-plaintext-egress-by-default, ephemeral client plaintext
(ADR 0003/0014/0015/0039). That machine exists because the Brain *is*
the trust-boundary product. Fragment's product is compute-over-files —
workflows read files server-side — so true E2EE forks it. Steal the thin
slice: client-side key generation, host-secret wrapping at rest, and
`--private` fragments (one root key per fragment, not a grant graph).

## The SOC 2 / HIPAA lift, sized

- **What code.storage's SOC 2 Type 2 + HIPAA buy:** their service's
  controls are attested; repo content sits inside an attested boundary.
  It does not extend to our celld VPS or bucket — those are our
  subprocessors.
- **Fragment itself getting SOC 2 Type 2:** an organizational audit, not
  a code badge — policies, access/change management, vendor inventory,
  monitoring, annual pen test, 3–12 month observation window; Vanta/
  Drata-class tooling ≈ $20–60k/yr + audit fees; the real cost is
  operating discipline forever. Don't.
- **HIPAA:** not a certification — a legal posture requiring BAAs across
  every subprocessor (AWS/GCS yes; Cloudflare per-component). OSS
  self-hosted software cannot carry it; deployments can.

**The vector: inherit, don't attest.** celld-on-Cloudflare inherits CF's
SOC 2 / HIPAA-eligible components; S3/GCS bring BAAs; code.storage
brings theirs. Fragment's code contributes controls it already has —
the events ledger is a real audit trail (every run/grant/write as an
immutable event), token-gated visibility, secrets-by-name-only. The
missing artifact is a **self-hoster compliance guide** (FDE, bucket SSE,
overlay networking, key management, encrypted backups) — a writing task,
not an engineering one. That document is the "industry-grade self-
hosted" proof.

## Verdict

Do level (b) immediately (config, not code) and the level (c) fix
(days). Keep `--private` fragments as designed in the research doc for
real per-fragment privacy. Never chase SOC 2 for the software itself —
compose attested providers, keep the wrapper thin, and publish the
self-hoster guide. The npub-identity story remains the differentiator;
the compliance story is inheritance plus an honest threat model.
