# Starter pages before and after the optchat style port

Blank and Todo, served by the local cell and opened in Chromium at
380 × 800, the width of a narrow app pane. Both light and dark are the
browser's emulated system preferences. Todo is connected to its live
operations and channels; the two tasks are added through its browser API.
These are page screenshots, without the shell's surrounding title bar.

Before: the pages and stylesheet from `b5e76cbc` (master at this branch's
start). After: this PR's port of optchat `6e3b5d77` and `aea794f1`.
The `templates` e2e lane captures these views and desktop views in its
run's scratch (`template-pages/`); keep them with `FRAGMENT_E2E_KEEP=1`.

| Page | Before | After |
| --- | --- | --- |
| Blank, light | [Before](before/blank-pane-light.png) | [After](after/blank-pane-light.png) |
| Blank, dark | [Before](before/blank-pane-dark.png) | [After](after/blank-pane-dark.png) |
| Todo, light | [Before](before/todo-pane-light.png) | [After](after/todo-pane-light.png) |
| Todo, dark | [Before](before/todo-pane-dark.png) | [After](after/todo-pane-dark.png) |
