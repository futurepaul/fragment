// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const SW_CLIENT_SOURCE = `
/* fragment sw-client v1 \u2014 push only: no fetch handler, ever */
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
export {
  SW_CLIENT_SOURCE
};
