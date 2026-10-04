# macrofiche against the contract

What macrofiche (`github.com/futurepaul/macrofiche`) answers where its
`docs/contract.md` says otherwise, and what is macrofiche's to fix. Each
entry gives the request, the answer the contract expects, and the answer
that came back.

## How it is measured

- The e2e on macrofiche: `FRAGMENT_E2E_CODESTORE=macrofiche
  MACROFICHE_BIN=<binary> cargo xtask e2e` (docs/self-host.md, seam 5).
- Its `codestore` section probes the answers fragment relies on that no
  lane asserts, and prints each failure as request, expected, actual
  (`crates/e2e/src/lanes/codestore.rs`). On macrofiche it also probes
  where the service and the fake differ (the url form, problem bodies,
  ephemeral refs, restore to the tip, a token for another repo).
- The same section passes 17 of 17 on the fake run as a process of its
  own (`fake-codestorage`), so a failure on macrofiche is macrofiche's
  answer, not the probe's.

## Mismatches

None recorded yet. On 2026-10-03 macrofiche had finished its design
(phase 1) and its engine (phase 2, commit `4f90ea2`), but had no server
binary to run: its service is phase 3.

## Not mismatches (fragment's side, or the design's)

- **Webhooks are per org in macrofiche's config; the cell's are per
  fragment** (`/api/f/<name>/webhook`, each with its own secret). So no
  macrofiche webhook can reach a cell today, and pins move by `refresh`
  and the poll, as on the hosted fleet. Either a per-repo registration
  (`PUT /api/repos/{repo}/webhook {url, secret}`, the contract's question
  2) or an org-wide receiving route on the platform closes it.
- **The merge preview (contract section 15, fragment PR #127) is not on
  this branch.** A CLI deploy after a rollback here merges blind, so
  against macrofiche's three-way merge it keeps the rollback's reverts.
  That is fixed on fragment's side by PR #127.
