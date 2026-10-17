/* fragment's service worker: push only, no fetch handler, ever */
self.addEventListener("push", (event) => {
  let p = null;
  try {
    p = event.data && event.data.json();
  } catch (e) {}
  if (!p || typeof p !== "object") p = {};
  const title = (typeof p.title === "string" && p.title) || "fragment";
  const opts = {
    body: typeof p.body === "string" ? p.body : "",
    tag: typeof p.tag === "string" ? p.tag : undefined,
    data: typeof p.url === "string" ? p.url : undefined,
    // who it is from: an agent's picture, an address on the web
    icon: typeof p.icon === "string" && /^https?:\/\//.test(p.icon) ? p.icon : undefined,
  };
  event.waitUntil(self.registration.showNotification(title, opts).catch(() => {}));
});
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  // a relative url (a chat's "./") is the fragment's page, beside this worker;
  // only a page of this origin is opened: a push names its url, and a click
  // on it must not take the person to another site (or a javascript: url)
  let url = null;
  try {
    const named = typeof event.notification.data === "string" ? new URL(event.notification.data, self.location) : null;
    url = named && named.origin === self.location.origin ? named.href : null;
  } catch (e) {}
  event.waitUntil(
    (async () => {
      try {
        const list = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
        for (const c of list) if (url && c.url === url) return await c.focus();
        if (url) {
          const w = await self.clients.openWindow(url);
          if (w) return;
        }
        for (const c of list) return await c.focus();
      } catch (e) {}
    })(),
  );
});
