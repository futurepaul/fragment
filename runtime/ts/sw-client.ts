// The __sw.js push service worker served per fragment (a reserved route in
// serve.ts, sibling of the router's __rt.js). Push-only by design: it
// installs NO fetch handler, so a registered worker never sits in any
// page's network path — it exists purely to turn push payloads into
// notifications and clicks into navigation. The payload is the JSON the
// server's web-push half sends: { title, body, tag, url } — url rides in
// notification.data and becomes the click-through target. Malformed
// payloads degrade to a generic "fragment" notification, never a throw.
export const SW_CLIENT_SOURCE = `
/* fragment sw-client v1 — push only: no fetch handler, ever */
self.addEventListener("push", (event) => {
  let p = {};
  try { p = (event.data && event.data.json()) || {}; } catch (e) { p = {}; }
  if (!p || typeof p !== "object") p = {};
  const title = (typeof p.title === "string" && p.title) || "fragment";
  const opts = {
    body: typeof p.body === "string" ? p.body : "",
    tag: typeof p.tag === "string" ? p.tag : undefined,
    data: typeof p.url === "string" ? p.url : undefined,
  };
  event.waitUntil(self.registration.showNotification(title, opts).catch(() => {}));
});
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data;
  event.waitUntil((async () => {
    try {
      const list = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const c of list) if (url && c.url === url) return await c.focus();
      if (url) { const w = await self.clients.openWindow(url); if (w) return; }
      for (const c of list) return await c.focus();
    } catch (e) {}
  })());
});
`;
