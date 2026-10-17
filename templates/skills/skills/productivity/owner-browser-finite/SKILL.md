---
name: owner-browser-finite
description: Your owner's own browser ("my browser", "my tabs"). Load it before you open, read or use anything in it, and when they ask whether you can. It is on their computer, signed in as them, once they connect it in Fragment's Settings.
---

# Your owner's browser

Your owner can connect the browser on their own computer to yours, from
Fragment's Settings, under Browsers, with the Fragment extension in that
browser. It is signed in as them on every site. "My browser", "my
Chrome", their tabs and their windows mean this one. Your native browser
tools drive another: the browser on your own desktop.

## 1. Check that it is connected

```
curl -s http://browser.fragment.internal/json/version
```

Connected, it names the browser (`Browser`) and its socket
(`webSocketDebuggerUrl`). Otherwise it says that it is not connected:
tell your owner to connect it from Settings, under Browsers, and wait for
them. It goes when their computer sleeps or the browser closes: check
again whenever a command below says it is gone.

## 2. Drive it from your terminal

With agent-browser, in the session `owner`, on the socket your computer
carries to it:

```
agent-browser --session owner --cdp ws://browser.fragment.internal/devtools/browser tab
agent-browser --session owner --cdp ws://browser.fragment.internal/devtools/browser open https://example.com
agent-browser --session owner --cdp ws://browser.fragment.internal/devtools/browser snapshot
```

Every command takes those two flags; `agent-browser --help` lists the
commands. `close` lets go of the browser and leaves it open.

## What it lets you use

`tab` lists only the tabs your owner lets you use, never every tab they
have open: at first just the ones you open, which join the "Agent" tab
group in their browser. Your owner gives you more, tabs of theirs or all
of them, from the Fragment extension's icon: when you need one of theirs,
ask them to give it to you there.

## Acting as your owner

On every site there, what you do, your owner did. Use it for your owner
alone: in a chat you share with others, on your owner's own messages.
Ask them first, with `clarify`, before you pay, buy, book, or type a
password there.
