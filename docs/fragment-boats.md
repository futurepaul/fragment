# fragment.boats: fragments on a domain of their own

Status: **decided 2026-09-25.** Paul answered the three open questions
(Answers, at the end). Slice 1 (decisions 2–4: isolation, framed
sign-in, sign-in and sign-out) is built for fragment.club, before the
move (PR `isolation-and-frames`); slice 2 (the move) and the PSL are
not. No DNS, Fly, or deploy change has been made.

Fragments move from `<label>--<username>.fragment.club` to
`<label>--<username>.fragment.boats`. The platform stays on
`fragment.club`. `fragment.boats` uses Namecheap's nameservers, like
fragment.club.

## Decisions proposed

1. **Two domains.** The platform (sign-in, the share sheet, `/join`,
   `/cli`, `/api`) stays on `fragment.club`. Fragments are served at
   `<label>--<username>.fragment.boats`. Existing fragments move to the
   new host in a hard cut. Their old hosts answer with a redirect.
   `fragment.boats` itself redirects to `fragment.club`.
2. **The desktop's frames sign in without the platform.** A frame's
   `src` is `__frame?name=…` on the desktop's own origin. That route is
   platform code, not the desktop's. It mints a single-use redemption and
   redirects the frame to the framed fragment's `__signin`. The framed
   fragment redeems it into a partitioned cookie (CHIPS) that is bound to
   the desktop's origin. The desktop's own code never holds a token.
   `__frame` is a capability a fragment declares (`frame`) and its owner
   allows in its share sheet (answer 3): the desktop is its first user.
3. **Isolation between fragments does not wait for the PSL.** The
   router applies a Fetch Metadata policy to every fragment host. A
   visitor's cookies count only for the fragment's own requests and for
   navigations. Another page cannot show a fragment in a frame at all
   unless the frame came through `__frame` (answer 3). On its own, this
   policy fixes the three open bugs, even on fragment.club today.
4. **Sign-in and sign-out cannot be triggered from another page.**
   `__signout` becomes a POST from the fragment's own page.
   `__signin` works only as a navigation. The platform asks
   "Continue to X?" before any fragment that is not yours, nor shared
   with you, learns who you are, wherever the sign-in started (answer 1).
5. **The Public Suffix List comes last.** Submit `fragment.boats` only
   after 1–4 are live and proven, and only once it serves thousands of
   people. The list declines smaller projects, and a listing is hard to
   undo. The listing adds browser-enforced isolation on top of 3. The
   design does not depend on it.

## Why

Today fragments and the platform are one site (`fragment.club`), so a
SameSite=Lax cookie rides along between them.

| Bug | Status | What closes it here |
|---|---|---|
| Clickjacking of platform pages (#19) | fixed: `frame-ancestors 'none'` | the move: the platform is cross-site from every fragment, so its cookie never rides into a fragment's frame |
| Another fragment's live socket opened with the visitor's cookie (#33) | fixed: `Origin` check | unchanged (kept) |
| `__join` posted from another fragment (#33) | fixed: `/join` on the platform | the move (the platform is cross-site) |
| A private fragment's scripts and images load cross-fragment with the cookie | **open** | decision 3 (a subresource from another fragment carries no cookie that counts); later the PSL (the browser stops sending the cookie at all) |
| An app's own routes can be posted to cross-fragment as the visitor | **open** | decision 3 (only a GET or HEAD navigation from elsewhere counts, as with SameSite=Lax) |
| Sign-in and sign-out links can be triggered from another page | **open** | decision 4 |

### Which pairs are one site

| Pair | Today | After the move | After the PSL lists fragment.boats |
|---|---|---|---|
| platform ↔ fragment | same site | **cross-site** | cross-site |
| fragment ↔ fragment | same site | same site | **cross-site** |
| the desktop ↔ a frame in it | same site | same site | **cross-site** |

The middle column may last a long time. Browsers pick up a PSL change
only in their next releases, and some platforms only in an OS update
(PSL wiki, *Derivative Propagation Timing*). The list may also decline
us (see *The Public Suffix List*). Every rule below is designed to hold
in both of the last two columns.

## Hosts and routing

Fleet variables (`fleets/fragment-club.json`):

| Variable | Value | Note |
|---|---|---|
| `FRAGMENT_HOST_SUFFIX` | `fragment.boats` | was `fragment.club` |
| `FRAGMENT_PLATFORM_URL` | `https://fragment.club` | new in this fleet file; its default (the suffix) would now be wrong |
| `FRAGMENT_LEGACY_HOST_SUFFIX` | `fragment.club` | new: old fragment hosts redirect |

The router's order (`cell/src/lib.rs`, `route`):

1. The platform's exact host: the platform. This is checked first, so
   the e2e's listed mode may put the platform under the fragments'
   suffix. A fragment's flat name always holds `--`, so the platform's
   host is never a fragment's.
2. A host that is a fragment's (`<flat>.<suffix>`): serve it, as today.
3. The suffix itself (`fragment.boats`): `308` to the platform, keeping
   the path and query.
4. A fragment's old host (`<flat>.<legacy suffix>`): `308` to the same
   path and query on the new host for GET and HEAD. For any other
   method, `410` with a JSON body saying where the fragment moved. An
   upgrade is refused. A page loaded before the cut reloads onto the new
   host.
5. Any other label under either suffix: `404`, as today.
6. Anything else (an IP, `localhost`, the `fly.dev` name): the platform,
   as today (dev and the CLI use `127.0.0.1`).

Keep the old hosts' DNS and certificate for at least a year. Old share
links and invites in people's messages keep working through the
redirect. It costs nothing: `*.fragment.club` already has its
certificate, and the platform still needs `fragment.club`.

What moves and what does not:

- **Repos:** named `<label>--<username>`, not by host. Nothing moves.
- **Sessions:** kept in the registry by fragment name, so they stay
  valid. The browser's cookie is on the old host, though, so each person
  signs in once per fragment. `__signin` comes straight back when
  already signed in.
- **Share links:** `?view=<token>` is in the URL, so the redirect
  carries it. The `fragview` cookie is set again on the new host.
- **Browser storage** on the old origins (the desktop's layout, drafts)
  is lost. That is acceptable for a hard cut.
- **Push:** subscriptions are stored in the fragment's cell by endpoint,
  so sends keep working. The old origin's service worker still shows
  them, and a click opens the old host, which redirects. People
  subscribe again from the new host when they next allow push there.
- **The CLI, agents, and the API** all use the platform URL. Unchanged.
  URLs they print come from the API (`Config::canonical`), which follows
  the suffix.
- **WorkOS:** the redirect URI stays `https://fragment.club/auth/callback`.

## Sign-in

### Top-level visits (the flow is unchanged; it is now cross-site)

- **Direct:** `X/__signin?return=/p` is a top-level GET. It redirects to
  `fragment.club/auth/fragment`. A SameSite=Lax cookie is sent on a
  top-level GET navigation from another site, so the platform session
  arrives. The platform mints a redemption and redirects to
  `X/__signin?token=`. X's response sets `__Host-fragment_site` (Lax).
  A cookie of any SameSite value may be set on a top-level navigation's
  response, even after a cross-site redirect (RFC 6265bis, storage
  model), so the chain works. The e2e proves it (below).
- **Share link:** `X/?view=<token>` is a top-level visit. `fragview` is
  first-party there. Signing in afterwards is the direct flow.
- **Invite:** `fragment.club/join/X?token=` posts on the platform, then
  goes to `/auth/fragment` (same-origin), then to X. Unchanged.
- **The platform's home and "new fragment":** links and redirects come
  from `canonical`, so they point at fragment.boats. Unchanged.
- A top-level visit to a fragment's URL with no session there goes
  through `fragment.club/auth/fragment` and back, as `__signin` does, and
  any other refusal a browser navigates to is a page, not JSON
  (docs/api.md, Opening a fragment by its URL).

Watch item: every sign-in bounces through fragment.club. Safari's ITP
and Chrome's bounce-tracking mitigation (on when third-party cookies are
blocked) clear the storage of redirectors that people never use
directly. People do use fragment.club directly (sign-in, the share
sheet, home), so no action is needed. Watch for reports of being signed
out of fragment.club after weeks away.

### In the desktop's frames: the problem

The desktop (`templates/desktop/`) frames the owner's fragments. Each
frame's `src` is `X/__signin?return=`. Inside the frame, that redirects
to `fragment.club/auth/fragment`, which reads the platform session. After
the move, that frame request is cross-site from the desktop's top-level
page, and a SameSite=Lax cookie is never sent into a cross-site frame. A
SameSite=None platform cookie would not fix it:

- Safari blocks every third-party cookie;
- Firefox partitions them (the platform's cookie in a frame under the
  desktop would be a different, empty cookie);
- Chrome blocks them in Incognito and for people who turn them off.

So every framed fragment would land on WorkOS's login inside a frame and
fail. Once the PSL lists fragment.boats, the framed fragment is
cross-site from the desktop too. Then even X's own cookie set inside the
frame has to be `SameSite=None` and, to survive third-party cookie
blocking, `Partitioned`.

### Options

| Option | Works in | Cost | Verdict |
|---|---|---|---|
| **A.** Keep today's hop through the platform inside the frame | nowhere after the move (above) | — | broken |
| **B.** The desktop's JavaScript fetches a token from its own origin and puts it in the frame's `src` | the browsers C works in | the desktop is code its owner's agent rewrites. It would hold, for 60 s, a bearer sign-in as the owner on every fragment the owner has, and could send it anywhere. That breaks "the desktop holds no authority over any of them" (`desktop.js`, decision 4) | rejected |
| **C.** The frame's `src` is `__frame` on the desktop's origin: platform code mints the redemption and redirects the frame to X's `__signin`, which sets a partitioned cookie bound to the desktop | Chrome 114+; Firefox (Total Cookie Protection since 2022, CHIPS since 141); Safari 18.4 and 26.2+. Safari 18.5–26.1 works while fragments are one site, and after the PSL shows a visible fallback | about one registry round trip per frame load | **recommended** |
| **D.** No cookie: a per-frame token held in memory or sessionStorage and sent as a header by `__fragment.js` | calls from `__fragment.js` only | a header cannot authenticate the frame's own page load, images, scripts, `__file`, app routes, or a WebSocket handshake (it takes no headers). A members-only fragment's page would not load at all. Making every template a single-page app behind a service worker that adds headers is a rewrite of every template | rejected |
| **E.** Storage Access API | all three, with prompts | a click inside every frame, and a prompt in Chrome and past a threshold in Firefox, per fragment once the PSL makes each its own site. Each fragment needs a first-party visit within 30 days. Grants lapse after 30 days. It opens only unpartitioned `SameSite=None` cookies, so the top-level cookie would have to drop Lax | rejected (a possible later fallback) |
| **F.** Other ideas: proxy frames through the desktop's origin; put the desktop on the platform's origin; Related Website Sets; open fragments only in tabs | — | the first two put an author's code on an origin it must not have. Chrome retired Related Website Sets (October 2025), and they were Chrome-only. Tabs only drops the desktop | rejected |

### The recommended design (C)

**`GET <desktop>/__frame?name=<fragment>&return=<path>`** is a new
platform route on every fragment's origin, next to `__fragments`:

1. It must be a frame of that origin's own page: `Sec-Fetch-Dest:
   iframe` and `Sec-Fetch-Site: same-origin`, both required. Headers
   Fetch Metadata sends cannot be forged by page script. Every browser
   that supports partitioned cookies sends them.
2. The caller must be the desktop's owner, signed in on the desktop's
   origin (its `fragment_site` cookie, sent because the frame is
   same-origin with the top-level page). The fragment must declare the
   `frame` capability at live, and its owner must have allowed it (the
   share sheet; `PUT /api/f/<name>/grants/frame`). `name` must be in the
   owner's list.
3. The registry mints a **frame redemption** from that site session's
   platform session: single-use, 60 s, for `name` only, bound to the
   desktop's origin.
4. It answers `302` to `<X>/__signin?token=…&return=…` with
   `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.

The desktop's code never sees the token. A parent page cannot read the
URL its cross-origin frame was redirected to. `fetch()` of `__frame`
fails on its `Sec-Fetch-Dest` (it is `empty`), and could not read a
redirect's `Location` anyway (an opaque redirect). A service worker that
forwards the frame's own navigation gets an opaque redirect too. Resource
Timing in the parent names only the frame's first URL.

**Redeeming a frame redemption at X's `__signin?token=`:**

- It requires `Sec-Fetch-Dest: iframe`. A frame redemption shown to a
  top-level navigation is refused and spent.
- It sets `__Host-fragment_frame=<token>; Path=/; Max-Age=…; Secure;
  HttpOnly; SameSite=None; Partitioned`. Before the PSL, its partition
  is `fragment.boats` (all fragments share it). After, its partition is
  the desktop's own site.
- It redirects to `__signin?check=frame&return=…`. If the cookie came
  back, the check goes on to `return`. If not (the browser blocks it), it
  answers a small page: "X can't sign you in inside this page. [Open X
  in a tab]". The page also posts `{fragment:
  "signin-blocked", name}` to the embedder's origin, so the desktop can
  show one notice. This is the fallback for Safari 18.5–26.1 after the
  PSL, and for anyone who blocks all cookies in frames.

**The frame session:** a site session row with `embedder = <desktop
origin>`. It expires with its platform session and ends with
`/auth/logout`, as site sessions do. It is bounded separately from
top-level sessions (the newest 4 per platform session and fragment), so
a desktop reloading its frames never ends the owner's top-level session
on X. Each frame load mints one redemption. A desktop restoring a chat
and eight panes mints nine at once, inside the 16 unspent a session may
hold.

**Frame-bound responses:** any response to a frame navigation that a
frame session signed in answers `Content-Security-Policy:
frame-ancestors <embedder>`. Before the PSL, the frame cookie's
partition is shared by every fragment, so another fragment that frames X
gets X's frame cookie sent with the navigation. The browser then refuses
to show X there, because every ancestor must match the embedder. After
the PSL, the partition already scopes it, and the header also keeps out
nested frames (the desktop, then another site, then X). Chrome's
partition key carries a cross-site-ancestor bit that does the same, but
the design does not count on every browser's key having one. The header is also sent on the page's `304`s, and a page answered
to a frame is `private, no-cache`, so no cached copy without it is
reused.

**The desktop** (`templates/desktop/site/desktop.js`): `framed(url,
path)` becomes `__frame?name=<name>&return=<path>`. `openFile` goes
through it too (with `return=/__file?path=…`), so a file pane never
depends on another pane having signed in first. It listens for
`signin-blocked`. The share sheet is unchanged: a top-level popup on
fragment.club, where the Lax cookie is sent.

## Isolation between fragments (decision 3)

The router decides which of a request's cookies count, from Fetch
Metadata, before the request reaches the fragment's cell. It extends
today's `cookies_count`, which already refuses cookies on an upgrade
that names no `Origin`. The site's own cookies are `fragment_site`,
`fragview`, and `fragment_anon`.

| The request, as the browser sends it | The site's own cookies | `fragment_frame` |
|---|---|---|
| `Sec-Fetch-Site: same-origin` (the fragment's own page: fetches, images, forms, its own links) | count | count |
| a navigation of the top-level window (`navigate`, `document`), GET or HEAD, from anywhere (`none`, `same-site`, `cross-site`) | count (SameSite=Lax semantics) | — |
| a navigation of a frame (`navigate`, `iframe`), GET or HEAD | — | count; the page answers `frame-ancestors <embedder>` |
| anything else from another page: images, scripts, `fetch`, form POSTs, a POST navigation | — | — |
| a WebSocket upgrade | counts only when `Origin` is the fragment's own (today's check) | same |
| no `Sec-Fetch-Site` at all (a browser before 2023, or not a browser) | as today | — |

A frame navigation that brings no live frame session answers
`frame-ancestors 'self'`, before the PSL and after: one fragment shows
another only through `__frame`, which only a page that declares `frame`,
allowed by its owner and viewed by them, has (answer 3).

How this closes the open bugs, before or after the PSL:

- **Scripts and images across fragments:** another fragment's page is
  `same-site`, so its `<img>` or `<script>` of X counts no cookie. X
  answers as to a stranger.
- **Posting to an app's routes across fragments:** a form POST or
  `fetch` from another page counts no cookie, and neither does a POST
  navigation. `__op` already refuses anything but JSON.
- **Clickjacking a fragment as its visitor:** a frame of X gets only the
  frame session, bound to its embedder, or `frame-ancestors 'self'`.
- Missing headers mean an old browser, which the policy treats as today
  (web.dev's resource isolation guidance does the same). An attacker
  cannot remove the headers from a current browser.

### Sign-in and sign-out (decision 4)

- `__signout`: `GET` shows a page with a button. `POST` requires
  `Origin` to be the fragment's own. It ends every session the request
  carries (top-level and frame) and clears both cookies.
- `__signin` without a token: only a top-level navigation (or no Fetch
  Metadata). In a frame, it answers the "open X in a tab" page; from an
  image, a script, or a fetch, 403.
- The platform's `/auth/fragment` mints without asking for the person's
  own fragments and those shared with them (it asks the fragment), and
  for one they said yes to before. Otherwise it shows "Continue to X as
  @paul?" as a platform page: unframed, with a button that arms after a
  moment, like the share sheet's. The yes is remembered per person and
  fragment; signing out of X there (`__signout`) forgets it, so X asks
  again (answer 1). The Referer rule this section first proposed is
  gone: X's own page sending a visitor to its `__signin` is the case
  answer 1 asks about.
- A subresource or frame can no longer complete a sign-in at all. The
  platform's cookie is never sent to a cross-site subresource or frame.

## Cookies on a fragment's origin

| Cookie | Attributes | Set by | Counts (table above) |
|---|---|---|---|
| `__Host-fragment_site` | `Secure; HttpOnly; SameSite=Lax; Path=/` | `__signin?token=` from a top-level navigation | the fragment's own requests, top-level navigations |
| `__Host-fragment_frame` (new) | `Secure; HttpOnly; SameSite=None; Partitioned; Path=/` | `__signin?token=` of a frame redemption, in a frame | the fragment's own requests, frame navigations; pages bound to the embedder |
| `fragview`, `fragment_anon` | as today (Lax) | `?view=`, the first call | as `fragment_site` |

Over http (dev, the e2e), the names drop the `__Host-` prefix, as today.
`Secure` stays, since `SameSite=None` and `Partitioned` require it:
Chrome 89+ and Firefox 75+ accept `Secure` cookies from `localhost`.

## Browser support the design rests on

Current versions (caniuse, 2026-09-25): Chrome 153, Firefox 155, Safari
26.6.

| Fact | Chrome | Firefox | Safari |
|---|---|---|---|
| Third-party cookies by default | allowed (Google kept them, April 2025); blocked in Incognito and by choice | partitioned per top-level site (Total Cookie Protection, default since June 2022) | all blocked since 13.1 (March 2020) |
| CHIPS (`Partitioned`) | 114+; still works when third-party cookies are blocked | 141+ (re-enabled July 2025) | 18.4; **off in 18.5–26.1** (a bug outside WebKit); back in 26.2 (December 2025). Needs `SameSite=None; Secure`. Domains ITP classifies as trackers may be refused |
| Partition key | top-level site, plus a cross-site-ancestor bit (CDP `hasCrossSiteAncestor`) | the top-level site (MDN) | the top-level site (WebKit) |
| Fetch Metadata (`Sec-Fetch-*`) | 76+ | 90+ | 16.4+ |
| Storage Access API | prompts; per (top-level, embedded) site; 30 days; `SameSite=None` cookies only | prompts past a threshold; 30 days | needs a prior first-party visit; 30 days |
| Related Website Sets | retired October 2025 (CHIPS and FedCM stay) | never | never |

What that means for the desktop:

- **Before the PSL** (frames same-site), every current browser works:
  - Safari 18.5–26.1 ignores the unknown `Partitioned` attribute and
    keeps the cookie as a normal same-site cookie;
  - the others partition it under `fragment.boats`.
- **After the PSL:**
  - Chrome, Firefox 141+, and Safari 26.2+ work through CHIPS;
  - Firefox before 141 works through Total Cookie Protection;
  - Safari 18.5–26.1 shows the fallback.

  Copies of the list bundled with an OS update only with the OS (PSL
  wiki), so the listing likely reaches Safari only in a release shipped
  after it merges, which would be 26.2 or later and have CHIPS. So the
  Safari gap may never be seen.
- iOS browsers other than Safari use WebKit, so they follow Safari's
  rows.

## The Public Suffix List

What the list requires for a PRIVATE entry (the wiki's Guidelines and
the PR template):

- **The `_psl` TXT record:** `_psl.fragment.boats` holds the PR's URL,
  e.g. `https://github.com/publicsuffix/list/pull/NNNN`. It must stay
  **for as long as the entry is listed**. The wiki says to leave these
  records in place to announce that continued inclusion is desired, and
  that missing ones will mark entries for automated removal.
- **Registration:** more than **2 years** left on `fragment.boats` when
  the PR is opened, stated in the rationale, with a commitment to keep
  more than 1 year left for as long as the domain is listed.
- **The PR template:**
  - a description of the organization (at least three sentences);
  - a robust reason;
  - the `dig` output;
  - a statement that no third-party limit is being worked around (we
    have none: one wildcard certificate, no Let's Encrypt rate limits);
  - a **role-based email** answered within 30 days;
  - an **abuse contact** reachable from the site (fragment.club needs an
    abuse page or address first);
  - an acknowledgement that rollback is slow and uncertain.
- **The entry:** a new subsection sorted by organization name among the
  PRIVATE domains, `// fragment : https://fragment.club` and `//
  Submitted by … <role@…>`, then `fragment.boats`. Not
  `*.fragment.boats`, which would make each fragment a suffix itself.
- **Review:** volunteers, with no service level. The wiki says "NO
  SERVICE LEVEL AGREEMENTS ON TIME". Weeks to months.
- **Propagation:** each browser and OS picks the change up in its own
  releases. The wiki says there is no way to speed it up. Removal
  cascades the same way, and rollbacks are the volunteers' lowest
  priority.
- **Size:** "projects not serving more then thousands of users are
  quite likely to be declined" (Guidelines, non-acceptance factors).

The listing is worth having even with decision 3 in place. Once
browsers ship it:

- they stop sending one fragment's cookies to another at all;
- they refuse `Domain=fragment.boats` cookies (cookie tossing into other
  fragments' app cookies);
- they partition each fragment's cache and storage separately;
- Chrome's Site Isolation puts each fragment in its own process
  (Spectre-class reads).

This is the list's intended use: "owners of privately-registered domains
who themselves issue subdomains to mutually-untrusting parties".

**When to submit:** after the move is live, the e2e's listed mode (below)
is green, and Paul has checked the desktop in the three browsers. Also
only once fragment.boats serves thousands of people, which the invite-
only alpha does not. Until then, decision 3 is the isolation, and the
middle column of the site table is the steady state.

## Fly and DNS

Now (read-only `dig`, 2026-09-25):

| Name | Record | Value |
|---|---|---|
| `fragment.club` | A / AAAA | `66.241.125.20` / `2a09:8280:1::199:8a1c:0` |
| `*.fragment.club` | A / AAAA | the same (`x--y.fragment.club` resolves to them) |
| `_acme-challenge.fragment.club` | CNAME | `fragment.club.nwd56j0.flydns.net.` |
| `fragment.boats` | A | `162.255.119.93` (Namecheap's URL forward: `302` to `http://www.fragment.boats/`) |
| `www.fragment.boats` | CNAME | `parkingpage.namecheap.com.` |
| `fragment.club`, `fragment.boats` | NS | `pdns1/pdns2.registrar-servers.com` (Namecheap) |
| either | CAA | none, so Let's Encrypt may issue |

For fragment.boats, Paul adds at Namecheap (Advanced DNS). He first
deletes the URL Redirect record on `@` and the `www` CNAME to the
parking page:

| Type | Host | Value |
|---|---|---|
| CNAME | `_acme-challenge` | the value `flyctl certs setup fragment.boats -a fragment-club` prints; expected `fragment.boats.nwd56j0.flydns.net.` (fragment.club's pattern) |
| A | `@` | `66.241.125.20` |
| AAAA | `@` | `2a09:8280:1::199:8a1c:0` |
| A | `*` | `66.241.125.20` |
| AAAA | `*` | `2a09:8280:1::199:8a1c:0` |
| TXT | `_psl` | the PSL PR's URL, **only when that PR is opened** |

Leave the SPF TXT record (Namecheap's email forwarding) alone. The
wildcard does not answer TXT queries for `_psl`, so the explicit record
is the one that counts.

Certificates. The DNS-01 challenge through the `_acme-challenge` CNAME
validates both, before any traffic moves (Fly's docs):

```
flyctl certs add fragment.boats -a fragment-club
flyctl certs add "*.fragment.boats" -a fragment-club
flyctl certs check "*.fragment.boats" -a fragment-club   # until Issued
```

These are Paul's to approve (they change the Fly app). Cost: the
wildcard is $1 a month. The apex is a single-hostname certificate,
within the organization's first 10 free (Fly pricing).

The apex, `fragment.boats`, points at the app, and the router answers
`308` to `https://fragment.club/` with the path kept. Pointing it at the
app rather than Namecheap's forward gives HTTPS under our own
certificate and keeps the redirect in code.

## Code changes and their size

The change is about 1,300 lines, most of them tests. It comes in two
slices. Slice 1 is useful on fragment.club today, so it lands before
the move.

**Slice 1: isolation, framed sign-in, sign-in and sign-out** (about
1,000 lines; built, e2e sections `isolation` and `frames`)

- `cell/src/lib.rs` (router, about 120 lines):
  - the cookie rule;
  - `__frame` routing;
  - passing the request's context (top-level, frame, same-origin, other)
    to the fragment's cell;
  - the platform's exact host checked before the suffix.
- `cell/src/auth.rs` (about 200 lines):
  - the frame cookie;
  - `__signin`'s rules, and the check and fallback pages;
  - `__signout` as a POST;
  - the confirmation on `/auth/fragment`.
- `cell/src/routed.rs` and `cell/src/serve.rs` (about 100 lines):
  - `Credential::Frame`, resolved with its embedder;
  - `frame-ancestors` on frame navigations, and on their `304`s.
- `cell/src/fragment.rs` (about 60 lines): `__frame`'s owner,
  capability, and list checks, as `__fragments` makes them.
- `cell/src/registry/signin.rs` and `calls.rs` (about 120 lines):
  - an `embedder` column on `redemptions` and `sessions`;
  - `MintFrame`, from a site session's parent;
  - the frame bound;
  - `Session` matching the kind.
- `templates/desktop/site/desktop.js` (about 30 lines):
  - `framed` through `__frame`;
  - `openFile` through it;
  - the `signin-blocked` notice.
- The e2e (about 350 lines, below) and the docs (about 120 lines).

**Slice 2: the move** (about 300 lines)

- `cell/src/config.rs` (about 20 lines): `FRAGMENT_LEGACY_HOST_SUFFIX`.
- The router (about 50 lines): the apex and old-host redirects.
- `fleets/fragment-club.json`: three variables.
- The e2e (about 150 lines): the hosted e2e checks both redirects.
- The docs (about 60 lines).

Docs each slice updates:

- `docs/api.md`: Sign-in, the cookies, the isolation rules, and
  `__frame`. Its "one site with the platform" passages go.
- `docs/platform.md`: a `__frame` row; the framing note now says the
  platform is cross-site.
- `docs/operate.md`: DNS, certificates, the fleet variables.
- `docs/ROADMAP.md`: a decision 19 amending decision 16, whose "every
  fragment shares the platform's domain" no longer holds.
- `docs/finite-integration.md`: the browser-sessions row (the
  per-origin exchange gains frames).
- Comments that say "one site" in `auth.rs`, `share.rs`, and `lib.rs`.

## The e2e

Today the e2e has two shapes:

- the default: the platform on `127.0.0.1`, fragments on
  `*.fragment.localhost`;
- `start_as_browsers_see_it`: the platform on `fragment.localhost`, one
  site with every fragment.

Chrome treats an unknown top-level domain's last label as the suffix,
so `a.fragment.localhost` and `b.fragment.localhost` are one site, while
`a.localhost` and `b.localhost` are two. The new shapes:

- **Two sites (the default; the production shape before the PSL):**
  - the platform at `fragment.localhost`;
  - fragments at `<flat>.boats.localhost`, one site with each other and
    cross-site from the platform.

  `start_as_browsers_see_it` goes (a hard cut). The desktop and share
  lanes run here.
- **Listed (the shape after the PSL):**
  - `FRAGMENT_HOST_SUFFIX=localhost`, so each fragment is its own site;
  - the platform still at `fragment.localhost` (the router checks the
    platform's host first).

  The desktop lane runs here too, with third-party cookies blocked
  through CDP `Network.setCookieControls({enableThirdPartyCookieRestriction:
  true})`. That is Chrome behaving as Safari 26.2 does: only
  `Partitioned` cookies in cross-site frames.
- **A probe first:** that Chrome keeps a `Secure; SameSite=None;
  Partitioned` cookie set by `http://<x>.localhost` in a frame, and sends
  `Sec-Fetch-*` there. Both should hold: `*.localhost` is potentially
  trustworthy. If either fails, the listed mode runs behind a throwaway
  local CA, with Chrome told to trust it.

The checks each mode adds (valid and invalid, as the engineering style
asks):

- **Top-level:**
  - a direct `__signin` signs in across sites;
  - a share link opens as a viewer and then signs in;
  - an invite joins and lands signed in.
- **The desktop:** a chat and an app frame sign in through `__frame`,
  and a message sent in the chat is the owner's. A file pane opens. The
  frame's pages carry `frame-ancestors <desktop>`.
- **`__frame` is refused:**
  - top-level;
  - fetched;
  - to someone who is not the owner;
  - for a fragment not in the list;
  - without the capability.

  A frame redemption is refused top-level, and is spent.
- **The fallback:** with the frame cookie blocked (the check hop), the
  notice shows and the desktop hears `signin-blocked`.
- **An attacker fragment's page** (the owner signed in on X both
  top-level and in the desktop):
  - its `<img>` of X's members-only image fails;
  - its `<script>` of X's defines nothing;
  - its form POST to X's app route arrives anonymous;
  - its frame of X shows nothing (blocked by `frame-ancestors`);
  - its `<img src=X/__signout>` leaves the owner signed in;
  - its top-level link to `fragment.club/auth/fragment?name=X` shows
    the confirmation instead of signing in.
- **Hosts:**
  - an old host answers `308` to the new one, and `410` to a POST;
  - the suffix's apex answers `308` to the platform;
  - another label under either suffix answers `404`.

The hosted e2e adds the redirects. Chrome is the only browser the e2e
drives. Paul checks Safari and Firefox by hand at the rollout (below).
That covers the shape before the PSL for real. The shape after the PSL
rests on the e2e's listed mode and on the browsers' documented behavior
(open question 2).

## Rollout

1. Slice 1: merge (CI green), then deploy to fragment.club (Paul
   approves). The three open bugs close there and then, and the desktop
   frames through `__frame`. A desktop made before keeps its old
   `site/desktop.js` (frames through `__signin`), whose panes now offer
   "open in a tab": make a new one, or copy the template's
   `site/desktop.js` and `fragment.json` into it; then allow its frames
   in its share sheet.
2. DNS and certificates for fragment.boats (Paul, below).
3. Slice 2: merge, then deploy with the new fleet variables (Paul
   approves). Then the hosted e2e.
4. Paul checks the desktop in Safari (26.2 or later), Firefox, and
   Chrome on fragment.boats: sign in, open a chat and an app, send a
   message, open a file.
5. Much later: the PSL (below).

## What Paul does

In order:

1. Read this; answer the open questions; approve slices 1 and 2.
2. Any time before slice 2 deploys, at Namecheap, for
   **fragment.boats**:
   - delete the URL Redirect record on `@` and the `www` CNAME
     (`parkingpage.namecheap.com`);
   - add CNAME `_acme-challenge` → the value `flyctl certs setup
     fragment.boats -a fragment-club` prints (expected
     `fragment.boats.nwd56j0.flydns.net.`).
3. **Fly certificates (yours to approve; changes the app; $1 a month
   for the wildcard):**
   ```
   flyctl certs add fragment.boats -a fragment-club
   flyctl certs add "*.fragment.boats" -a fragment-club
   flyctl certs check "*.fragment.boats" -a fragment-club   # until Issued
   ```
4. At Namecheap, for fragment.boats:
   - A `@` `66.241.125.20`;
   - AAAA `@` `2a09:8280:1::199:8a1c:0`;
   - A `*` `66.241.125.20`;
   - AAAA `*` `2a09:8280:1::199:8a1c:0`.

   Leave every fragment.club record and certificate as it is: the
   platform and the old hosts' redirects use them.
5. Approve the slice 2 deploy (`cargo xtask deploy fragment-club` with
   the three fleet variables).
6. Check the desktop in Safari, Firefox, and Chrome (rollout step 4).
7. **The PSL: not now.** When fragment.boats serves thousands of people,
   and after steps 1–6:
   - renew fragment.boats so that more than 2 years remain;
   - publish an abuse contact on fragment.club and set up a role email
     that is answered within 30 days;
   - open the PR with the template filled in;
   - add TXT `_psl` = `https://github.com/publicsuffix/list/pull/<N>`,
     and keep it and more than a year of registration for as long as
     the entry is listed.

## Answers (Paul, 2026-09-25)

1. **Being shown to a fragment's author: ask once, strangers only.**
   Signing in stays silent on your own fragments and on those shared
   with you. On anyone else's, the platform asks once ("Continue to X as
   you?") before X learns who you are; until then you are a visitor
   there. The answer is remembered per person and fragment; signing out
   of X there undoes it (the next sign-in asks again). ROADMAP decision
   4 is amended.
2. **Safari: prove it in Chrome, plus one real check.** The e2e's
   `frames` section runs the listed mode (every fragment its own site)
   in a Chrome launched with `--test-third-party-cookie-phaseout`: the
   desktop's frames sign in through `__frame`, and a fragment whose
   cookies a site setting blocks shows the "open in a tab" fallback.
   WebKit's documented CHIPS behavior covers Safari; Paul checks real
   Safari once after the deploy.
3. **Embedding: chats will embed apps.** So `__frame` is not
   desktop-only: it is the `frame` capability a fragment declares in
   `fragment.json`, honored only once its owner allows it in the share
   sheet ("Your fragments inside it"). The desktop is the first user; a
   chat showing an app inline is the next. A framed fragment shows only
   inside a page that holds the capability and went through `__frame`.
   Amended 2026-09-26 (Paul): making a desktop with the platform's
   new-fragment form is that allow (the form says so); any other way asks
   in the share sheet (docs/platform.md, `frame`).

## Not in scope

- **Login CSRF with an attacker's own redemption.** Someone who mints a
  redemption for themselves can send a victim to `X/__signin?token=` and
  sign the victim in as themselves on X. This predates the move. The
  fix is a nonce cookie set by `__signin` before the platform hop and
  checked at redemption. It is a follow-up.
- **Reserved cookie names in app responses.** An app's route could set
  a cookie named like the platform's on its own origin. That harms only
  its own fragment. Stripping those names from app responses is a
  follow-up.
- **HSTS** for either domain.

## Sources

- PSL: [Guidelines](https://github.com/publicsuffix/list/wiki/Guidelines), [PR template](https://github.com/publicsuffix/list/blob/main/.github/pull_request_template.md)
- Safari: [full third-party cookie blocking, 13.1](https://webkit.org/blog/10218/full-third-party-cookie-blocking-and-more/); [CHIPS in 18.4](https://webkit.org/blog/16574/webkit-features-in-safari-18-4/); [off in 18.5, WebKit bug 292975](https://bugs.webkit.org/show_bug.cgi?id=292975); [back in 26.2](https://webkit.org/blog/17640/webkit-features-for-safari-26-2/)
- Firefox: [Total Cookie Protection by default](https://blog.mozilla.org/en/mozilla/firefox-rolls-out-total-cookie-protection-by-default-to-all-users-worldwide/); [141 release notes (CHIPS)](https://www.firefox.com/en-US/firefox/141.0/releasenotes/)
- Chrome: [CHIPS](https://privacysandbox.google.com/cookies/chips); [third-party cookies stay, April 2025](https://privacysandbox.google.com/blog/privacy-sandbox-next-steps); [Privacy Sandbox retirements, October 2025](https://privacysandbox.google.com/blog/update-on-plans-for-privacy-sandbox-technologies); [CDP Network domain (`setCookieControls`, `CookiePartitionKey.hasCrossSiteAncestor`)](https://chromedevtools.github.io/devtools-protocol/tot/Network/)
- [MDN: Partitioned cookies](https://developer.mozilla.org/en-US/docs/Web/Privacy/Guides/Third-party_cookies/Partitioned_cookies); [MDN: Storage Access API](https://developer.mozilla.org/en-US/docs/Web/API/Storage_Access_API); [MDN: Fetch metadata](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Fetch_metadata); [caniuse: Sec-Fetch-Site](https://caniuse.com/mdn-http_headers_sec-fetch-site)
- Secure cookies on localhost: [Chromium issue 40120372](https://issues.chromium.org/issues/40120372), [Firefox bug 1618113](https://bugzilla.mozilla.org/show_bug.cgi?id=1618113)
- Fly: [custom domains and wildcard certificates](https://docs.fly.io/networking/custom-domain/); [pricing](https://docs.fly.io/about/pricing/)
