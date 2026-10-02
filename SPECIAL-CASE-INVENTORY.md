# Special-case inventory

Every platform feature that is not expressible as a fragment, with the
reason it is an exception. A feature belongs here only when making it
platform code is a security win, or it is blessed UI that users must
not be able to break and that we iterate on with the platform. This is
not a list of things only we can build: each entry says which public
API it uses, so a fragment could build the same thing on top of those
APIs, and the list stays short (Paul, 2026-10-02; `docs/cloudflare-v1.md`
decision 6).

Adding an entry needs its reason and its API. Removing one is always
welcome.

| Surface | Why it is platform | Built on |
|---|---|---|
| Sign-in, sessions, CLI approval (`/auth/*`, `/cli`) | Security: it mints the credentials every other surface trusts. | WorkOS AuthKit; the registry |
| The desktop shell (`/chat`) | Security: it holds your session across all your fragments. Also blessed UI. | The public chat, agent and fragment APIs |
| Chat (the Chat DO and its view) | Blessed UI: the core loop, which must not break. | The public chat API; a fragment may draw its own chat over it |
| The Computer tab (screen, take over) | Security: it drives your computer. | The Computer DO's screen socket |
| Connections and the token swap | Security: it holds the route to your accounts' tokens. | WorkOS Pipes; the computer's intercepts |
| Agent management (profiles on the computer) | Security: it writes to your computer. The agent's definition itself is an ordinary `agent` fragment repo. | The agent repo; the Computer DO |
| Usage, credits and plans | Billing integrity. | The ledger API (read-only to fragments) |
| The share sheet and invites | Security: it acts as the fragment's owner. | The members and invites API |
| Settings and profile | Security: account and billing. It lists skills, sites and brains, which are ordinary fragments. | The person, ledger and fragment APIs |

## Platform components any fragment may use

These are blessed UI offered to every fragment, not exclusive to the
platform. They do not count against the list above.

- The vault UI (files and notes, used by brains).
- The browser library (`__fragment.js`).
