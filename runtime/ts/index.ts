// Router worker: public entry. Routes to cells, verifies NIP-98 on control
// routes, stamps x-fragment-pubkey (and strips any inbound spoof).
import { verifyNip98, sha256Hex } from "./auth.js";
import { RT_CLIENT_SOURCE } from "./rt-client.js";

export { FragmentCell } from "./cell.js";

function json(data, status = 200) {
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json" } });
}

function stripAuth(headers) {
  const h = new Headers(headers);
  h.delete("x-fragment-pubkey");
  return h;
}

export default {
  async fetch(request, env) {
    // Behind an ingress (Caddy in production) rebuild request.url from the
    // forwarded headers so NIP-98 u-tags and generated URLs carry the public
    // origin, not the loopback listener. celld's listener only accepts
    // Caddy's traffic, so these headers can't arrive spoofed from outside.
    const xfProto = request.headers.get("x-forwarded-proto");
    const xfHost = request.headers.get("x-forwarded-host");
    if (xfProto || xfHost) {
      const orig = new URL(request.url);
      const proto = (xfProto || orig.protocol.replace(":", "")).split(",")[0].trim();
      const host = (xfHost || request.headers.get("host") || orig.host).split(",")[0].trim();
      if (`${proto}://${host}` !== orig.origin) {
        request = new Request(`${proto}://${host}${orig.pathname}${orig.search}`, request);
      }
    }
    const url = new URL(request.url);
    const path = url.pathname;
    const registry = () => env.FRAGMENT.getByName("_registry");
    const cell = (name) => env.FRAGMENT.getByName(name);

    // Forward to a cell with (optionally verified) identity headers.
    const toCell = async (name, newPath, verifiedPubkey) => {
      const headers = stripAuth(request.headers);
      if (verifiedPubkey) headers.set("x-fragment-pubkey", verifiedPubkey);
      // the public URL (path form the visitor used), so served apps can see
      // their own paths instead of the internal /__serve/ route
      headers.set("x-fragment-url", request.url);
      const target = url.origin + newPath + url.search;
      const hasBody = request.method !== "GET" && request.method !== "HEAD";
      return cell(name).fetch(new Request(target, {
        method: request.method,
        headers,
        body: hasBody ? request.body : undefined,
        // @ts-ignore duplex needed for streaming bodies
        duplex: hasBody ? "half" : undefined,
      }));
    };

    // NIP-98 gate. Returns { pubkey } or a Response (error).
    const gate = async () => {
      const bodyBytes = (request.method === "GET" || request.method === "HEAD") ? null : await request.clone().arrayBuffer();
      const res = await verifyNip98(request, bodyBytes);
      return res.ok ? { pubkey: res.pubkey } : { error: json({ error: res.error }, 401) };
    };
    // Optional NIP-98 (serving routes): absent header → anonymous, bad header → 401.
    const softGate = async () => {
      if (!request.headers.get("authorization")) return { pubkey: null };
      return gate();
    };

    try {
      // ---- infra ----
      if (path === "/__internal/ping") return new Response("pong");

      if (path.startsWith("/__internal/f/")) {
        const rest = path.slice("/__internal/f/".length);
        const name = rest.slice(0, rest.indexOf("/"));
        // The internal plane exists only for loader-isolate loopback traffic
        // (workflow/app/rooms ctx calls), which cells authenticate with their
        // own run tokens. Two namespaces are never reachable over HTTP, from
        // anyone: the registry and cell init. Both are exercised exclusively
        // through the FRAGMENT binding by the router itself (create flow,
        // syncRolesToRegistry, slug-map). Without this block, anyone who can
        // reach the router could mint registry rows without a key, map
        // arbitrary slugs onto other fragments, or win the __cell/init race
        // on a not-yet-initialized name.
        if (name === "_registry" || rest.slice(name.length).startsWith("/__cell/")) {
          return new Response("not found", { status: 404 });
        }
        // Defense in depth when the host sets FRAGMENT_HOST_SECRET (celld:
        // CELLD_VAR_FRAGMENT_HOST_SECRET; CF: a wrangler secret): loopback
        // calls must carry x-fragment-host-secret. Cells stamp the value into
        // loaded workers' env, so ctx keeps working unchanged. Unset (plain
        // local dev) → per-cell run-token auth only, as before.
        const want = env.FRAGMENT_HOST_SECRET;
        if (want) {
          const got = request.headers.get("x-fragment-host-secret") || "";
          const [hw, hg] = await Promise.all([sha256Hex(want), sha256Hex(got)]);
          if (hw !== hg) return json({ error: "bad host secret" }, 403);
        }
        return toCell(name, path, null);
      }

      // ---- control API ----
      if (path === "/api/fragments" && request.method === "POST") {
        const g = await gate();
        if (g.error) return g.error;
        const { name } = await request.json().catch(() => ({}));
        if (!name || typeof name !== "string") return json({ error: "body: {name}" }, 400);
        const created = await registry().fetch("http://x/__registry/create", {
          method: "POST", body: JSON.stringify({ name, ownerHex: g.pubkey }),
        });
        if (!created.ok) return created;
        const init = await cell(name).fetch("http://x/__cell/init", {
          method: "POST", body: JSON.stringify({ name, ownerHex: g.pubkey }),
        });
        const info = await init.json();
        const canonical = env.FRAGMENT_SUBDOMAIN_HOST
          ? `https://${encodeURIComponent(name)}.${env.FRAGMENT_SUBDOMAIN_HOST}/`
          : `${url.origin}/f/${name}/`;
        return json({ name, npub: info.npub, viewToken: info.viewToken, inboxToken: info.inboxToken, canonical });
      }
      if (path === "/api/fragments" && request.method === "GET") {
        const g = await gate();
        if (g.error) return g.error;
        const r = await registry().fetch(`http://x/__registry/list?pubkey=${g.pubkey}`);
        return new Response(r.body, { status: r.status, headers: { "content-type": "application/json" } });
      }
      if (path.startsWith("/api/f/")) {
        const rest = path.slice("/api/f/".length);
        const name = rest.slice(0, rest.indexOf("/") === -1 ? undefined : rest.indexOf("/"));
        if (!name) return json({ error: "bad path" }, 400);
        const cellPath = "/api" + rest.slice(name.length); // strip /f/<name>
        // the inbox is token-gated inside the cell, not nostr-gated
        if (cellPath === "/api/inbox" && request.method === "POST") return toCell(name, cellPath, null);
        const g = await gate();
        if (g.error) return g.error;
        return toCell(name, cellPath, g.pubkey);
      }

      // ---- rt client ----
      if ((path.startsWith("/f/") || path.startsWith("/d/")) && path.endsWith("/__rt.js")) {
        return new Response(RT_CLIENT_SOURCE, { headers: { "content-type": "text/javascript; charset=utf-8", "cache-control": "no-store" } });
      }

      // ---- live sync channel ----
      if (path.startsWith("/f/") && path.endsWith("/__watch")) {
        const name = path.split("/")[2];
        const g = await softGate();
        if (g.error) return g.error;
        const headers = stripAuth(request.headers);
        if (g.pubkey) headers.set("x-fragment-pubkey", g.pubkey);
        return cell(name).fetch(new Request(`${url.origin}/__watch${url.search}`, { method: request.method, headers }));
      }

      // ---- rooms ----
      if (path.startsWith("/f/") && path.includes("/__room/")) {
        const name = path.split("/")[2];
        const room = path.slice(path.indexOf("/__room/") + "/__room/".length);
        const g = await softGate();
        if (g.error) return g.error;
        const q = new URLSearchParams(url.search);
        q.set("draft", "blessed");
        const headers = stripAuth(request.headers);
        if (g.pubkey) headers.set("x-fragment-pubkey", g.pubkey);
        return cell(name).fetch(new Request(`${url.origin}/__room/${room}?${q}`, { method: request.method, headers }));
      }
      if (path.startsWith("/d/") && path.includes("/__room/")) {
        const slug = path.split("/")[2];
        const room = path.slice(path.indexOf("/__room/") + "/__room/".length);
        const r = await registry().fetch(`http://x/__registry/slug?s=${encodeURIComponent(slug)}`);
        if (!r.ok) return json({ error: "unknown draft" }, 404);
        const { name } = await r.json();
        const q = new URLSearchParams(url.search);
        q.set("draft", slug);
        return cell(name).fetch(new Request(`${url.origin}/__room/${room}?${q}`, { method: request.method, headers: stripAuth(request.headers) }));
      }

      // ---- site serving ----
      if (path.startsWith("/f/")) {
        const name = path.split("/")[2];
        if (!name) return new Response("not found\n", { status: 404 });
        const rest = path.slice(`/f/${name}/`.length).replace(/^\//, "");
        const g = await softGate();
        if (g.error) return g.error;
        return toCell(name, `/__serve/b/${rest}`, g.pubkey);
      }
      if (path.startsWith("/d/")) {
        const slug = path.split("/")[2];
        if (!slug) return new Response("not found\n", { status: 404 });
        const rest = path.slice(`/d/${slug}/`.length).replace(/^\//, "");
        const r = await registry().fetch(`http://x/__registry/slug?s=${encodeURIComponent(slug)}`);
        if (!r.ok) return json({ error: "unknown draft" }, 404);
        const { name } = await r.json();
        return toCell(name, `/__serve/d/${slug}/${rest}`, null);
      }

      return new Response("fragment host. see /f/<name>/ for fragments.\n", { status: 404 });
    } catch (e) {
      return json({ error: String((e && e.stack) || e) }, 500);
    }
  },
};
