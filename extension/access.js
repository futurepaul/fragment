// What the agent may use of this browser: the person's choice, in the
// popup (docs/explorations/owner-browser.md, a spike). Only the tabs it
// opens (the default, grouped as "Agent"), those and the tabs the person
// chooses, or every tab in every window. A tab outside it is never
// listed, attached or touched (cdp.js asks here).

// the person's choice, least first
export const ACCESS = ["own", "chosen", "all"];

// the tabs the agent opened, and the tabs the person chose
export const own = new Set();
const chosen = new Set();
let access = "own";

// the person's choice, kept: the level across restarts, the chosen tabs
// for this browser's session (a tab's id lasts only as long)
export const ready = Promise.all([
  chrome.storage.local.get("access").then((v) => (access = ACCESS.includes(v.access) ? v.access : "own")),
  chrome.storage.session.get("chosen").then((v) => (v.chosen ?? []).forEach((id) => chosen.add(id))),
]);

// Whether the agent may use tab `tabId` now.
export function allowed(tabId) {
  return own.has(tabId) || (access === "chosen" && chosen.has(tabId)) || access === "all";
}

// The tabs the agent may use, as Chrome's targets (`{id, tabId, title, url}`).
export async function usable() {
  const targets = await chrome.debugger.getTargets();
  return targets.filter((t) => t.type === "page" && t.tabId !== undefined && allowed(t.tabId));
}

// The person's choice, from the popup: the level, or a tab chosen.
export async function choose(m) {
  if (m.access !== undefined) {
    if (!ACCESS.includes(m.access)) throw new Error(`no access ${m.access}`);
    access = m.access;
    await chrome.storage.local.set({ access });
  }
  if (m.tabId !== undefined) {
    if (m.on) chosen.add(m.tabId);
    else chosen.delete(m.tabId);
    await chrome.storage.session.set({ chosen: [...chosen] });
  }
}

// A tab closed: it is neither the agent's nor chosen any more.
export async function closed(tabId) {
  own.delete(tabId);
  if (chosen.delete(tabId)) await chrome.storage.session.set({ chosen: [...chosen] });
}

// What the popup shows of it: the level, and every tab with whether the
// agent may use it (and why).
export async function view() {
  await ready;
  const tabs = (await chrome.tabs.query({})).filter((t) => !t.url?.startsWith("chrome-extension://"));
  return {
    access,
    tabs: tabs.map((t) => ({ id: t.id, title: t.title ?? "", url: t.url ?? "", own: own.has(t.id), chosen: chosen.has(t.id), allowed: allowed(t.id) })),
  };
}
