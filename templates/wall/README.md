# wall

A page anyone with the link posts to: a guestbook, or a party's wall.
New posts land on every open page as they are written.

- A post is a record on `posts`, a channel people post to (`"post":
  "public"`; `fragment.post`). The platform appends it with no app code,
  naming its poster: a person signed in as themselves (the page shows
  their username and picture), anyone else as the anonymous principal
  their browser is given, with the name they typed. A visitor holding
  only the public role spends a public call per post: 60 a minute each,
  600 a minute across the fragment. The channel keeps its newest 10 000.
- The page follows `posts` (its newest 200, then live), and `wall`, a
  live query: the title and intro an editor gave it (`about`), and the
  posts an editor took down (`hide`), which the page leaves out. The
  channel is append-only, so a hidden post is off the wall, not out of
  the log.
- An editor's post may carry a photo: the page shrinks it to 1600 px,
  uploads it as a blob (`fragment.blob`), and names it in the post's
  `attachments`, which keeps it while the channel keeps the post. Only
  editors upload blobs, and a blob is read by viewers and up: on a
  `link` fragment (the default) everyone with the link sees the photos;
  on a `public` one, people who are not members do not.

For a wall only people signed in may post to, say so on the channel:
`"posts": { "read": "public", "post": "public", "signedIn": true }`. An
anonymous visitor's post is then refused (401), and the page offers them
the sign-in.

From the CLI: `fragment post <name> posts --body '{"name":"Ana","text":
"hello"}'`, `fragment channel <name> posts --follow`, `fragment call
<name> hide --input '{"seq":3}'`.
