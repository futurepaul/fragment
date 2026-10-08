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
| Sign-in, sessions, CLI approval (`/auth/*`, `/cli`; the shell's tabs sign in through `/auth/frame`; a stranger's fragment asks first, at `/auth/fragment`) | Security: it mints the credentials every other surface trusts. Which of a browser's cookies count on a fragment's origin is the router's (Fetch Metadata), so another fragment's page reaches it as a stranger (docs/api.md, Sign-in). | WorkOS AuthKit; the registry |
| The shell (`/`, `/settings`): sidebar, tabs, profile and settings, search, first run; to a page of a fragment you own that asks, your agents, and adding one of them to that fragment (docs/api.md, The shell) | Security: it holds your session and frames your fragments and computers. The thin page users can't break. A page asks it, never grants: the shell checks the page is your own fragment's, at its own origin, offers only your own agents, and adds one only once you confirm it in the shell's own dialog, every time. | The public fragment, computer, identity and ledger APIs |
| Identity and delegation | Security: who an agent acts for, and at what role. | The registry |
| Connections and the egress swap | Security: it holds the route to your accounts' tokens and the operator's keys. | WorkOS Pipes; the computer's intercepts |
| Usage, credit and plans | Billing integrity. | The ledger API (read-only to fragments) |
| The share sheet (`/share/<name>`) | Security: it acts as the fragment's owner, so no fragment's code (which its author or an agent rewrites) may frame, fetch or script it (docs/api.md, Sharing). | The members and invites API |
| An operator's wipe of a person (`/api/people/{person}/wipe`, `fragment operator wipe`) | Security and account integrity: only the deployment's operators delete a person, and it reaches every object of theirs (their fragments, computer, ledger, lists and registry rows), which no fragment may (docs/api.md, Operators). | The registry, and each object's own end (a fragment's delete, a computer's saves) |
| Test levers and the e2e's sign-in (`/api/test/*`; previews and the local e2e only, 404 elsewhere) | Proof: the hosted e2e signs `@e2e.test` people in and pulls levers on a preview's real vendors, with no real account. A secret only a branch deploy takes gates it, and on a preview it reaches the e2e's own fragments and people alone (docs/secrets.md). | The registry, the ledger, and the fragment cells' test controls |

## Fragment plumbing (not special cases)

Every fragment gets these routes (docs/api.md, Serving), and none of
them knows what a fragment is for: `__fragment.js`, `__signin` and
`__signout`, `__op`, `__live` and `__watch`, `__people`, `__members`,
`__blob`, `__files` and `__file` (the vault UI), `__sw.js` and
`__push-*`, `__preview.svg`, people's pictures
(`/api/users/{u}/picture`), and the frame-session redeem path the
shell's tabs use (`__signin?token=` in a frame, from `/auth/frame`). A
refusal of the router's that a browser navigates to is a page in the
platform's look, not JSON; an app's own answers pass as they are
(docs/api.md, Opening a fragment by its URL).

## Not special cases

These are fragments or computers, or components any fragment may use:

- Chats, agents, brains, the skills set and apps are fragments. The blessed
  ones run the platform's current template version.
- A computer's screen is a page its image serves, reached through the
  generic port proxy.
- Platform components any fragment may use: the vault UI (files and
  notes, used by brains) and the browser library (`__fragment.js`).
