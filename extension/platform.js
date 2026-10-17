// The Fragment extension on the platform's own page (docs/explorations/
// owner-browser.md, a spike): the page's Settings hands it a one-time
// ticket to connect this browser, or asks it to disconnect, and hears its
// status, all by window messages (`{fragmentBrowser: …}`) between the page
// and this script. A classic content script: it runs only where the
// manifest's `matches` say the platform is, and again on the pages already
// open when the extension is installed, reloaded or updated
// (background.js).
(() => {
  // The newest copy on the page answers it. Once the extension is reloaded
  // or updated, the copy before is cut off from it (Chrome takes its runtime
  // away, and a message would throw); the worker puts its own here, which
  // marks itself the newest (the DOM is the one thing the two share), and
  // the old one falls silent. A cut-off copy no newer one replaced tells
  // the page to load again, which brings the extension's own.
  const me = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  const root = document.documentElement;
  if (root) root.dataset.fragmentExtension = me;
  const newest = () => !root || root.dataset.fragmentExtension === me;

  function post(status) {
    if (!newest()) return;
    window.postMessage(
      {
        fragmentBrowser: "status",
        connected: !!status?.connected,
        connecting: !!status?.connecting,
        computer: status?.computer ?? null,
        link: status?.link ?? null,
        error: status?.error ?? null,
        why: status?.why ?? null,
        reload: !!status?.reload,
      },
      location.origin,
    );
  }

  // The worker, asked, and its answer told to the page.
  const CUT_OFF = { connected: false, reload: true };
  function ask(m) {
    if (!chrome.runtime?.id) return post(CUT_OFF);
    try {
      chrome.runtime.sendMessage(m).then(post, () => post(CUT_OFF));
    } catch {
      post(CUT_OFF);
    }
  }

  window.addEventListener("message", (e) => {
    if (e.source !== window || e.origin !== location.origin || !newest()) return;
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
})();
