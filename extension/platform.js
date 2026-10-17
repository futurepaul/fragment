// The Fragment extension on the platform's own page (docs/explorations/
// owner-browser.md, a spike): the page's Settings hands it a one-time
// ticket to connect this browser, or asks it to disconnect, and hears its
// status, all by window messages (`{fragmentBrowser: …}`) between the page
// and this script. A classic content script: it runs only where the
// manifest's `matches` say the platform is.

function post(status) {
  window.postMessage({ fragmentBrowser: "status", connected: !!status?.connected, computer: status?.computer ?? null, link: status?.link ?? null, error: status?.error ?? null, reload: !!status?.reload }, location.origin);
}

// The worker, asked, and its answer told to the page. Once the extension
// is reloaded or removed, this script is left on a page already open, cut
// off from it (Chrome takes its runtime away, and a message would throw):
// the page is told to load again, which brings the extension's new one.
const CUT_OFF = { connected: false, reload: true };
function ask(m) {
  if (!chrome.runtime?.id) return post(CUT_OFF);
  chrome.runtime.sendMessage(m).then(post, () => post(CUT_OFF));
}

window.addEventListener("message", (e) => {
  if (e.source !== window || e.origin !== location.origin) return;
  const m = e.data;
  if (!m || typeof m !== "object") return;
  if (m.fragmentBrowser === "connect" && typeof m.url === "string") ask({ type: "connect", url: m.url });
  else if (m.fragmentBrowser === "disconnect") ask({ type: "disconnect" });
  else if (m.fragmentBrowser === "hello") ask({ type: "status" });
});

chrome.runtime.onMessage.addListener((m) => {
  if (m?.type === "status") post(m.status);
});

ask({ type: "status" });
