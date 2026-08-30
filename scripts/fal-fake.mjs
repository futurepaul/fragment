#!/usr/bin/env node
// A stand-in fal.ai queue for e2e (scripts/dev with E2E_FAL_FAKE=1 points
// CELLD_VAR_FRAGMENT_FAL_BASE here). Implements exactly the queue surface the
// runtime uses — submit, status, result, plus media serving — with two
// deliberate wire shapes so both placement paths get exercised:
//   - image outputs carry content-length  → the cell STREAMS them through
//   - video outputs are chunked (no length) → the cell buffers, then uploads
// Deterministic per-request bodies keep sha assertions stable per job while
// still differing across jobs.
import http from "node:http";

const port = Number(process.argv[2] || 9942);
const jobs = new Map();

const rand = (n) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => b.toString(16).padStart(2, "0")).join("");
const bodyFor = (seedHex, kind) => {
  // xorshift over the request id: same id → same bytes → same sha256
  let h = parseInt(seedHex.slice(0, 8), 16) || 1;
  const size = kind === "video" ? 300 * 1024 : 120 * 1024;
  const chunks = [];
  let left = size;
  while (left > 0) {
    h ^= h << 13; h >>>= 0; h ^= h >>> 17; h ^= h << 5; h >>>= 0;
    const take = Math.min(left, 65536);
    chunks.push(Buffer.alloc(take, h & 0xff));
    left -= take;
  }
  return Buffer.concat(chunks);
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  const send = (code, obj, headers = {}) => {
    const buf = typeof obj === "string" || Buffer.isBuffer(obj) ? obj : JSON.stringify(obj);
    res.writeHead(code, { "content-type": typeof obj === "object" && !Buffer.isBuffer(obj) ? "application/json" : "application/octet-stream", ...headers });
    res.end(buf);
  };

  if (url.pathname === "/health") return send(200, { ok: true, name: "fal-fake" });

  // media serving is a plain CDN fetch — real fal serves output files
  // unauthenticated, so the placement path must not rely on the key here
  const media = url.pathname.match(/^\/media\/([0-9a-f]+)\.(jpeg|mp4)$/);
  if (media && req.method === "GET") {
    const job = jobs.get(media[1]);
    if (!job) return send(404, { detail: "no such media" });
    const buf = bodyFor(media[1], job.kind);
    // video rides chunked (no content-length) on purpose: the runtime must
    // fall back to its bounded-buffer placement; image rides a length and
    // streams through
    if (media[2] === "mp4") {
      res.writeHead(200, { "content-type": "video/mp4", "transfer-encoding": "chunked" });
      res.write(buf.subarray(0, 100_000));
      res.end(buf.subarray(100_000));
      return;
    }
    return res.end(buf);
  }

  // the auth contract: every queue call must carry an Authorization: Key
  // header — the value itself is the dev stack's dummy (accept any
  // non-empty key so E2E_FAL_KEY can vary without syncing two places)
  const auth = req.headers.authorization || "";
  if (!auth.startsWith("Key ") || auth.length < 8) return send(401, { detail: "missing fal key" });

  const m = url.pathname.match(/^\/(.+)\/requests\/([0-9a-f]+)\/status$/);
  if (m && req.method === "GET") {
    const job = jobs.get(m[2]);
    if (!job) return send(404, { detail: "no such request" });
    job.polls++;
    const status = job.polls <= 2 ? "IN_QUEUE" : job.polls <= 4 ? "IN_PROGRESS" : "COMPLETED";
    return send(200, status === "IN_QUEUE" ? { status, queue_position: 1 } : { status });
  }

  const r = url.pathname.match(/^\/(.+)\/requests\/([0-9a-f]+)$/);
  if (r && req.method === "GET") {
    const job = jobs.get(r[2]);
    if (!job) return send(404, { detail: "no such request" });
    if (job.polls < 5) return send(422, { detail: [{ msg: "not finished yet" }] });
    if (job.input.prompt === "FAIL:force-error") {
      jobs.delete(r[2]);
      return send(400, { detail: [{ msg: "forced generation failure (bad prompt guard)" }] });
    }
    return job.kind === "video"
      ? send(200, { video: { url: `http://127.0.0.1:${port}/media/${r[2]}.mp4`, content_type: "video/mp4" }, timings: { inference: 4.2 } })
      : send(200, { images: [{ url: `http://127.0.0.1:${port}/media/${r[2]}.jpeg`, content_type: "image/jpeg" }], timings: { inference: 2.1 }, seed: 7 });
  }

  if (req.method === "POST" && /^\/[A-Za-z0-9][-A-Za-z0-9._/]+$/.test(url.pathname)) {
    let raw = "";
    req.on("data", (c) => { raw += c; });
    req.on("end", () => {
      let input = null;
      try { input = JSON.parse(raw); } catch {}
      if (!input || typeof input.prompt !== "string" || !input.prompt) return send(422, { detail: [{ msg: "prompt required" }] });
      const id = rand(16);
      const kind = input.duration !== undefined || url.pathname.includes("video") ? "video" : "image";
      jobs.set(id, { model: url.pathname.slice(1), input, kind, polls: 0 });
      if (jobs.size > 500) jobs.delete(jobs.keys().next().value);
      send(200, {
        request_id: id,
        status_url: `http://127.0.0.1:${port}${url.pathname}/requests/${id}/status`,
        response_url: `http://127.0.0.1:${port}${url.pathname}/requests/${id}`,
        cancel_url: `http://127.0.0.1:${port}${url.pathname}/requests/${id}/cancel`,
      });
    });
    return;
  }

  send(404, { detail: "fal-fake: no such route" });
});

server.listen(port, "127.0.0.1", () => console.log(`fal-fake listening on 127.0.0.1:${port}`));
