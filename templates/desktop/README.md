# desktop

Your fragments side by side: chats in the middle, apps and files stacked
in a viewer on the right, all of them fragments of yours. Each shows in
its own frame, signed in on its own origin; the desktop holds no
authority over any of them.

- `fragment.json` asks for the `fragments` capability: its page, viewed
  by its owner, may list the owner's fragments and make new ones
  (`__fragments`). Anyone else, editors included, is refused.
- It asks for `frame` too: each frame is its own `__frame`, which signs
  the frame in on its fragment's origin for this page only. The platform
  honors it once you allow it in the desktop's share sheet ("Your
  fragments inside it"); until then the desktop says so, with a button
  that opens that sheet.
- `app.mjs` keeps which fragments are chats (`chats`, `add_chat`,
  `remove_chat`); every other fragment is an app.
- Sharing is the platform's: each row's `…` menu has Share, which opens
  the platform's share sheet (`/share/<name>`, from `__fragments`) in a
  window of its own; this page cannot drive it. Rows are badged with how
  many people besides you (and your agents) are in them, and a globe when
  anyone may open them, from `sharing` in your list (reading it wakes none
  of the fragments).
- `site/layout.js` is the frame (sidebar | chat | viewer, one CSS grid
  resized with split-grid), `site/viewer.js` the stacked panes, and
  `site/desktop.js` the rest. Layout and open panes are remembered in the
  browser.

Keys: ⌘B hides the sidebar, ⌥⌘B the viewer. Drag a pane's header to
reorder it, double-click it to maximize.
