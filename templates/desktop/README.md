# desktop

Your fragments side by side: chats in the middle, apps and files stacked
in a viewer on the right, all of them fragments of yours. Each shows in
its own frame, signed in on its own origin; the desktop holds no
authority over any of them.

- `fragment.json` asks for the `fragments` capability: its page, viewed
  by its owner, may list the owner's fragments and make new ones
  (`__fragments`). Anyone else, editors included, is refused.
- `app.mjs` keeps which fragments are chats (`chats`, `add_chat`,
  `remove_chat`); every other fragment is an app.
- `site/layout.js` is the frame (sidebar | chat | viewer, one CSS grid
  resized with split-grid), `site/viewer.js` the stacked panes, and
  `site/desktop.js` the rest. Layout and open panes are remembered in the
  browser.

Keys: ⌘B hides the sidebar, ⌥⌘B the viewer. Drag a pane's header to
reorder it, double-click it to maximize.
