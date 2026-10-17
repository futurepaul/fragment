// What the agent may use of this browser: the person's choice, in the
// popup (docs/explorations/owner-browser.md, a spike). Only the tabs it
// opens (the default, grouped as "Agent"), those and the tabs the person
// chooses, or every tab in every window. A tab outside it is never
// listed, attached or touched (cdp.js asks here).

// the person's choice, least first
export const ACCESS = ["own", "chosen", "all"];

// the tabs the agent opened, and the tabs the person chose
const own = new Set();
const chosen = new Set();
let access = "own";

// kept: the person's level across restarts; the agent's tabs and the
// chosen ones for this browser's session (a tab's id lasts only as long),
// so a worker Chrome stopped and started again (a dropped connection, a
// laptop's sleep) still knows them
export const ready = Promise.all([
  chrome.storage.local.get("access").then((v) => (access = ACCESS.includes(v.access) ? v.access : "own")),
  chrome.storage.session.get(["own", "chosen"]).then((v) => {
    (v.own ?? []).forEach((id) => own.add(id));
    (v.chosen ?? []).forEach((id) => chosen.add(id));
  }),
]);

// saved only once loaded: a worker Chrome woke for a tab's event never
// writes its empty sets over the kept ones
async function keep() {
  await ready;
  return chrome.storage.session.set({ own: [...own], chosen: [...chosen] });
}

// A tab the agent opened: its own.
export async function adopt(tabId) {
  await ready;
  own.add(tabId);
  await keep();
}

// Whether the agent may use tab `tabId` now.
export function allowed(tabId) {
  return own.has(tabId) || (access === "chosen" && chosen.has(tabId)) || access === "all";
}

// The tabs the agent may use, as Chrome's targets (`{id, tabId, title, url}`).
export async function usable() {
  await ready;
  const targets = await chrome.debugger.getTargets();
  return targets.filter((t) => t.type === "page" && t.tabId !== undefined && allowed(t.tabId));
}

// The person's choice, from the popup: the level, or a tab chosen.
export async function choose(m) {
  await ready;
  if (m.access !== undefined) {
    if (!ACCESS.includes(m.access)) throw new Error(`no access ${m.access}`);
    access = m.access;
    await chrome.storage.local.set({ access });
  }
  if (m.tabId !== undefined) {
    if (m.on) chosen.add(m.tabId);
    else chosen.delete(m.tabId);
    await keep();
  }
}

// A tab closed: it is neither the agent's nor chosen any more.
export async function closed(tabId) {
  await ready;
  if ([own.delete(tabId), chosen.delete(tabId)].some(Boolean)) await keep();
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
