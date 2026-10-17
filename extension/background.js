// The Fragment extension's worker (docs/explorations/owner-browser.md, a
// spike): it lets its owner's agents use this browser. Its parts:
// link.js holds the computer's socket and this browser's link (and comes
// back with it on its own), cdp.js answers the guest's CDP over the tabs
// the agent may use, and access.js keeps the person's choice of those.
// Here they are wired, and the platform's page (platform.js) and the
// popup heard.

import * as access from "./access.js";
import * as cdp from "./cdp.js";
import * as link from "./link.js";

link.wire({ frame: cdp.said, ended: cdp.ended, changed: tell });

// Every page of the platform's hears its status (platform.js).
function tell(status) {
  chrome.tabs.query({}).then((all) => {
    for (const tab of all) chrome.tabs.sendMessage(tab.id, { type: "status", status }).catch(() => {});
  });
}

chrome.runtime.onMessage.addListener((m, sender, reply) => {
  if (sender.id !== chrome.runtime.id) return;
  // only the extension's own popup chooses what the agent may use
  const popup = sender.url?.startsWith(chrome.runtime.getURL("popup.html"));
  if (m?.type === "connect") {
    // only the platform's own page hands it a ticket (platform.js runs there alone)
    if (!sender.tab || popup) return reply({ ...link.current(), error: "a ticket comes from the platform's page" });
    link.connect(m.url, sender.origin ?? sender.url).then(() => reply(link.current()), (e) => reply({ ...link.current(), error: e.message }));
    return true;
  }
  if (m?.type === "disconnect") {
    link.forget().then(() => reply(link.current()));
    return true;
  }
  if (m?.type === "status") return reply(link.current());
  if (m?.type === "view") {
    access.view().then((v) => reply({ ...v, status: link.current() }));
    return true;
  }
  if (m?.type === "choose") {
    if (!popup) return reply({ error: "only the extension's own popup chooses" });
    access
      .choose(m)
      .then(() => link.current().connected && cdp.refresh())
      .then(access.view)
      .then((v) => reply({ ...v, status: link.current() }), (e) => reply({ error: e.message }));
    return true;
  }
});

// a worker that starts (Chrome's start, an alarm, a message) comes back
// with its link, if it has one
chrome.runtime.onStartup.addListener(() => link.resume());
link.resume();
