# desktop

Your fragments side by side: chats in the middle, apps, computers, and
files stacked in a viewer on the right, all of them fragments of yours.
Each shows in
its own frame, signed in on its own origin; the desktop holds no
authority over any of them.

- `fragment.json` asks for the `fragments` capability: its page, viewed
  by its owner, may list the owner's fragments and make new ones
  (`__fragments`). Anyone else, editors included, is refused.
- It asks for `frame` too: each frame is its own `__frame`, which signs
  the frame in on its fragment's origin for this page only. The platform
  honors it once you allow it: making the desktop with the platform's
  new-fragment form does (the form says so), or its share sheet ("Your
  fragments inside it"). `__fragments` says which (`frame`); without it
  the desktop shows a notice in place of its panes, with a button that
  opens that sheet, and opens nothing in a frame.
- A new desktop is yours alone (`members`) until you share it.
- A chat is a fragment New chat named (`chat-…`), and a computer one New
  computer named (`computer-…`, from the pet template: a Sprite of its
  own, on your budget), wherever you made it; every other fragment is an
  app. `app.mjs` keeps the chats this desktop made (`chats`, `add_chat`,
  `remove_chat`), listed first, newest first.
- A computer opens as a pane, like an app. Its page is what keeps it
  awake: while its pane is open, and 5 minutes after it closes. Your
  agent runs commands on it from any chat (its `run` job;
  docs/computers.md).
- Sharing is the platform's: each row's `…` menu has Share, which opens
  the platform's share sheet (`/share/<name>`, from `__fragments`) in a
  window of its own; this page cannot drive it. Rows are badged with how
  many people besides you (and your agents) are in them, and a globe when
  anyone may open them, from `sharing` in your list (reading it wakes none
  of the fragments).
- An app pane's header says the same, and who may open it, with a Share
  button; and who else has the app open now, as faces (`__presence`: one
  read for the open app panes every 12 seconds while this page is in
  view, which wakes only those fragments).
- `site/layout.js` is the frame (sidebar | chat | viewer, one CSS grid
  resized with split-grid), `site/viewer.js` the stacked panes, and
  `site/desktop.js` the rest. Layout and open panes are remembered in the
  browser.

Keys: ⌘B hides the sidebar, ⌥⌘B the viewer. Drag a pane's header to
reorder it, double-click it to maximize.
