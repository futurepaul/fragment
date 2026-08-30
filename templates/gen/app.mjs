// fragment gen — a prompt box that makes images and videos. No dials: the
// host's model defaults ride underneath (a ~1MP jpeg image, a 5s 768p
// video clip), and every generation lands in the fragment's working copy
// under gen/ — it serves at __file?path=… and syncs to your local folder
// on the next `fragment pull` like any other file.
//
// Routes (relative to the fragment root — works under /f/<name>/ and a
// canonical subdomain alike):
//   POST generate  {kind: "image"|"video", prompt} → {file}   (held ~5-10s)
//   GET  recent    → {files: […gen/ rows…]}                    (the grid on load)
import { generateImage, generateVideo } from "ai";

const j = (body, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

export default {
  async fetch(req, ctx) {
    const path = new URL(req.url).pathname.replace(/^\/+/, "");

    if (req.method === "POST" && path === "generate") {
      const { kind, prompt } = await req.json().catch(() => ({}));
      if (kind !== "image" && kind !== "video") return j({ error: 'kind must be "image" or "video"' }, 400);
      if (typeof prompt !== "string" || !prompt.trim()) return j({ error: "prompt required" }, 400);
      try {
        const result = kind === "video"
          ? await generateVideo({ prompt: prompt.trim().slice(0, 4000) })
          : await generateImage({ prompt: prompt.trim().slice(0, 4000) });
        const media = kind === "video" ? result.video : result.image;
        return j({ file: { path: media.path, size: media.size, mime: media.mediaType, url: media.url } });
      } catch (e) {
        // host misconfiguration (missing keys) and model verdicts both land
        // as readable text — the page shows it in the card
        return j({ error: String((e && e.message) || e) }, 200);
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
