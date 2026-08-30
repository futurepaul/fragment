// fragment gen — a prompt box that makes images and videos. No dials: the
// platform's cheap defaults ride underneath (a ~1MP jpeg image, a 5s 768p
// video clip). Generated media is placed in the fragment's working copy
// under gen/ — it serves at __file?path=… and syncs to your local folder
// on the next `fragment pull` like any other file.
//
// Routes (relative to the fragment root — works under /f/<name>/ and a
// canonical subdomain alike):
//   POST start    {kind: "image"|"video", prompt} → {job}   (returns at once)
//   POST status   {job} → {status: "queued"|"working"|"done", file?}
//   GET  recent   → {files: […gen/ rows…]}                   (the grid on load)
//
// The job object is opaque and carries no secrets — the page round-trips
// it while polling. Sleeping happens browser-side and isolate-side, never
// cell-side, so the fragment stays responsive while generating.
const j = (body, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

export default {
  async fetch(req, ctx) {
    const path = new URL(req.url).pathname.replace(/^\/+/, "");

    if (req.method === "POST" && path === "start") {
      const { kind, prompt } = await req.json().catch(() => ({}));
      if (kind !== "image" && kind !== "video") return j({ error: 'kind must be "image" or "video"' }, 400);
      if (typeof prompt !== "string" || !prompt.trim()) return j({ error: "prompt required" }, 400);
      try {
        const job = await ctx.gen.start(kind, prompt.trim().slice(0, 4000));
        return j({ job });
      } catch (e) {
        return j({ error: String((e && e.message) || e) }, 502);
      }
    }

    if (req.method === "POST" && path === "status") {
      const { job } = await req.json().catch(() => ({}));
      try {
        return j(await ctx.gen.status(job));
      } catch (e) {
        // the cell's own error text (fal verdicts, missing key) verbatim
        return j({ status: "error", error: String((e && e.message) || e) }, 200);
      }
    }

    if (req.method === "GET" && path === "recent") {
      const rows = (await ctx.files.index("gen/"))
        .filter((f) => /\.(jpe?g|png|webp|mp4|webm|mov)$/i.test(f.path));
      rows.sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0));
      return j({ files: rows.slice(0, 36) });
    }

    return new Response("not found\n", { status: 404 });
  },
};
