// The Fragment extension on the platform's own page (docs/explorations/
// owner-browser.md, a spike): the page's Settings hands it a one-time
// ticket to connect this browser, or asks it to disconnect, and hears its
// status, all by window messages (`{fragmentBrowser: …}`) between the page
// and this script. A classic content script: it runs only where the
// manifest's `matches` say the platform is.

function post(status) {
  window.postMessage({ fragmentBrowser: "status", connected: !!status?.connected, computer: status?.computer ?? null, link: status?.link ?? null, error: status?.error ?? null }, location.origin);
}

window.addEventListener("message", (e) => {
  if (e.source !== window || e.origin !== location.origin) return;
  const m = e.data;
  if (!m || typeof m !== "object") return;
  if (m.fragmentBrowser === "connect" && typeof m.url === "string") chrome.runtime.sendMessage({ type: "connect", url: m.url }).then(post);
  else if (m.fragmentBrowser === "disconnect") chrome.runtime.sendMessage({ type: "disconnect" }).then(post);
  else if (m.fragmentBrowser === "hello") chrome.runtime.sendMessage({ type: "status" }).then(post);
});

chrome.runtime.onMessage.addListener((m) => {
  if (m?.type === "status") post(m.status);
});

chrome.runtime.sendMessage({ type: "status" }).then(post);
