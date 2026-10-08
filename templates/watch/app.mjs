// Watch: pages and prices, checked every hour. The cron trigger runs
// `sweep`, which starts one `check` run per watch: it fetches the page,
// picks the part watched, and `seen` keeps it, publishing a change to
// `changes` and pushing it to everyone who asked. A check that fails
// says why on `problems`; a page that does not answer is retried with
// backoff first, then its run is held: `fragment runs <name> --status
// held` lists them, and `fragment replay <name> <run>` checks again.
import { DurableObject } from "cloudflare:workers";

const WATCHES_MAX = 25;
const VALUE_MAX = 500;
const REDIRECTS_MAX = 3;
const HEADER = /^([A-Za-z0-9-]{1,64}):\s*(\S.*)$/;

// A page that answered, but not with what the watch follows: checking
// again will not help, so its run succeeds, saying so.
class Problem extends Error {}

const clip = (s, n) => [...s].slice(0, n).join("");
const ENTITIES = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };
const decode = (s) =>
  s.replace(/&(?:#(\d+)|#x([0-9a-f]+)|([a-z]+));/gi, (m, dec, hex, name) =>
    name ? ENTITIES[name.toLowerCase()] ?? m : String.fromCodePoint(dec ? Number(dec) : parseInt(hex, 16)));
const squash = (s) => decode(s).replace(/\s+/g, " ").trim();

// The part of a page a watch follows: in JSON, the value at `pick`'s
// path (`data.price`); in HTML, the text of what `pick` selects (a CSS
// selector); without a pick, the page's text.
async function partOf(page, pick) {
  if ((page.headers.get("content-type") || "").includes("json")) {
    let v;
    try {
      v = page.json();
    } catch {
      throw new Problem("its answer is not JSON");
    }
    for (const key of pick ? pick.split(".") : []) v = v?.[key];
    if (v === undefined || v === null) throw new Problem(`nothing at ${pick}`);
    return typeof v === "string" ? v : JSON.stringify(v);
  }
  const html = page.text();
  if (!pick) return squash(html.replace(/<(script|style)\b[\s\S]*?<\/\1>/gi, " ").replace(/<[^>]*>/g, " "));
  let found = "";
  let matched = false;
  try {
    await new HTMLRewriter()
      .on(pick, { element() { matched = true; found += " "; }, text(t) { found += t.text; } })
      .transform(new Response(html))
      .text();
  } catch (e) {
    throw new Problem(`${pick} is not a selector (${e.message})`);
  }
  if (!matched) throw new Problem(`nothing matched ${pick}`);
  return squash(found);
}

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS watches (
      id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL, label TEXT NOT NULL, pick TEXT NOT NULL, header TEXT NOT NULL,
      value TEXT, hash TEXT, checked_at INTEGER, changed_at INTEGER, at INTEGER NOT NULL)`);
  }

  // `header`, when given, is one line sent with each check, such as
  // `Authorization: Bearer {{SHOP_KEY}}`: the platform fills in the secret
  // on the way out, and the app never holds it.
  add({ url, label, pick, header }) {
    let parsed;
    try {
      parsed = new URL(url.trim());
    } catch {
      throw new Error("that is not a URL");
    }
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") throw new Error("a watch is an http(s) page");
    header = (header || "").trim();
    if (header && !HEADER.test(header)) throw new Error("a header is one line, `Name: value`");
    if (this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM watches").one().n >= WATCHES_MAX) {
      throw new Error(`at most ${WATCHES_MAX} watches; remove one first`);
    }
    pick = (pick || "").trim();
    label = (label || "").trim() || clip(`${parsed.hostname}${pick ? ` ${pick}` : ""}`, 100);
    const { id } = this.ctx.storage.sql
      .exec("INSERT INTO watches (url, label, pick, header, at) VALUES (?, ?, ?, ?, ?) RETURNING id", parsed.href, label, pick, header, Date.now())
      .one();
    return { id };
  }

  remove({ id }) {
    this.ctx.storage.sql.exec("DELETE FROM watches WHERE id = ?", id);
    return { id };
  }

  // every watch, the newest first (a header says only that there is one)
  list() {
    const watches = this.ctx.storage.sql
      .exec("SELECT id, url, label, pick, header != '' AS header, value, checked_at, changed_at FROM watches ORDER BY id DESC")
      .toArray();
    return { watches: watches.map((w) => ({ ...w, header: w.header === 1 })) };
  }

  watch({ id }) {
    return this.ctx.storage.sql.exec("SELECT url, pick, header FROM watches WHERE id = ?", id).toArray()[0] ?? null;
  }

  // the cron trigger's: one check run per watch
  async sweep(input, job) {
    const { watches } = await job.call("list", {});
    for (const w of watches) await job.call("check", { id: w.id });
    return { started: watches.length };
  }

  // A check's problem is a record on `problems`, not a mutation: a held
  // run's replay takes its steps again under the same keys, and its
  // `seen` would meet that mutation's id with another input (409).
  async check({ id }, job) {
    const w = await job.call("watch", { id });
    if (!w) return { gone: true };
    const headers = {};
    const line = HEADER.exec(w.header);
    if (line) headers[line[1]] = line[2];
    try {
      let url = w.url;
      let page = await job.fetch(url, { headers });
      // a fetch answers a redirect: follow it, as a step of its own (the
      // header goes only to the watch's own site)
      for (let i = 0; i < REDIRECTS_MAX && page.status >= 300 && page.status < 400 && page.headers.get("location"); i++) {
        url = new URL(page.headers.get("location"), url).href;
        page = await job.fetch(url, { headers: new URL(url).origin === new URL(w.url).origin ? headers : {} });
      }
      if (!page.ok) throw new Problem(`it answered ${page.status}`);
      const part = await partOf(page, w.pick);
      return await job.call("seen", { id, value: clip(part, VALUE_MAX), hash: await sha256(part) });
    } catch (e) {
      const held = !(e instanceof Problem);
      await job.publish("problems", { watch: id, problem: clip(e.message, 300), run: job.run, held });
      if (held) throw e;
      return { problem: e.message };
    }
  }

  // what a check found: a new value is a change (pushed to everyone who
  // asked), unless it is the first
  seen({ id, value, hash }, call) {
    const w = this.ctx.storage.sql.exec("SELECT label, value, hash FROM watches WHERE id = ?", id).toArray()[0];
    if (!w) return { gone: true };
    const now = Date.now();
    const changed = w.hash !== null && w.hash !== hash;
    this.ctx.storage.sql.exec(
      "UPDATE watches SET value = ?, hash = ?, checked_at = ?, changed_at = CASE WHEN ? THEN ? ELSE changed_at END WHERE id = ?",
      value, hash, now, changed ? 1 : 0, now, id,
    );
    if (changed) {
      call.publish("changes", { watch: id, label: w.label, before: w.value, after: value });
      call.push("changes", { title: `${w.label} changed`, body: `${clip(w.value, 200)} → ${clip(value, 200)}`, tag: `watch-${id}`, url: "./" });
    }
    return { changed };
  }
}
