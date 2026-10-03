# Special-case inventory

The platform is fragments and computers (`docs/cloudflare-v1.md`, "The
rule"). This file lists everything else the platform does, with the
reason it is not a fragment or a computer. A thing belongs here only
when making it platform code is a security win, or it is the thin
shell users can't break. Each entry names the public API it sits on, so
a fragment could build the same thing on top of those APIs. The list
stays short (Paul, 2026-10-02).

Adding an entry needs its reason and its API. Removing one is always
welcome. Nothing here may name an agent runtime (Hermes), a template or
a vendor's product beyond the credential it holds.

| Surface | Why it is platform | Built on |
|---|---|---|
| Sign-in, sessions, CLI approval (`/auth/*`, `/cli`; the shell's tabs sign in through `/auth/frame`) | Security: it mints the credentials every other surface trusts. | WorkOS AuthKit; the registry |
| The shell (`/chat`): sidebar, tabs, profile, search, first run | Security: it holds your session and frames your fragments and computers. The thin page users can't break. | The public fragment, computer, identity and ledger APIs |
| Identity and delegation | Security: who an agent acts for, and at what role. | The registry |
| Connections and the egress swap | Security: it holds the route to your accounts' tokens and the operator's keys. | WorkOS Pipes; the computer's intercepts |
| Usage, credit and plans | Billing integrity. | The ledger API (read-only to fragments) |
| The share sheet and invites | Security: it acts as the fragment's owner. | The members and invites API |

## Interim, until phase 5

On master after the phase 1 cut, the shell's place is held by `/`
(choose a username) and `/settings` (your fragments, a new fragment,
CLI pairing, your picture), plus `/join/{name}`. All three go when the
shell lands.

## Fragment plumbing (not special cases)

Every fragment gets these routes, and none of them knows what a
fragment is for: `__signin` and `__signout`, `__op`, `__live`,
`__people`, `__blob`, `__files` (the vault UI), people's pictures
(`/api/users/{u}/picture`), and the frame-session redeem path the
shell's tabs use (`__signin?token=` in a frame, from `/auth/frame`).

## Not special cases

These are fragments or computers, or components any fragment may use:

- Chats, agents, brains, the skills set and apps are fragments. The blessed
  ones run the platform's current template version.
- A computer's screen is a page its image serves, reached through the
  generic port proxy.
- Platform components any fragment may use: the vault UI (files and
  notes, used by brains) and the browser library (`__fragment.js`).
