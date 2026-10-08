// Brief: your feeds, read and summed up each morning. The cron trigger
// runs `brief` every hour, and it goes on only at the brief's hour where
// you are: it fetches each feed, keeps what is new since the last brief,
// asks a model to sum it up (a text step, paid from the owner's ledger),
// publishes the brief to `briefs` (the archive the page reads), and
// pushes it to every browser that asked. "Brief me now" runs it at once.
import { DurableObject } from "cloudflare:workers";

const FEEDS_MAX = 20;
const ITEMS_PER_FEED = 8;
const ITEMS_MAX = 40;
const REDIRECTS_MAX = 3;
const DAY_MS = 24 * 3600 * 1000;
const WRITE = `You write someone's morning brief from the new items in their feeds. Lead with what matters most. Write 3 to 6 short lines, each starting with "- ", in plain text with no headings and no preamble, and end each line with its source in parentheses.`;
const ACCEPT = "application/rss+xml, application/atom+xml, application/xml;q=0.9, text/xml;q=0.9, text/html;q=0.8";

const clip = (s, n) => [...s].slice(0, n).join("");
const ENTITIES = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };
const decode = (s) =>
  s.replace(/&(?:#(\d+)|#x([0-9a-f]+)|([a-z]+));/gi, (m, dec, hex, name) =>
    name ? ENTITIES[name.toLowerCase()] ?? m : String.fromCodePoint(dec ? Number(dec) : parseInt(hex, 16)));
// an element's text: CDATA unwrapped, markup (escaped or not) dropped
const text = (s) => decode(s.replace(/<!\[CDATA\[([\s\S]*?)\]\]>/g, "$1").replace(/<[^>]*>/g, " ")).replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
const tag = (xml, name) => new RegExp(`<${name}\\b[^>]*>([\\s\\S]*?)</${name}>`, "i").exec(xml)?.[1] ?? "";
const web = (href, base) => {
  try {
    const u = new URL(href, base);
    return u.protocol === "https:" || u.protocol === "http:" ? u.href : "";
  } catch {
    return "";
  }
};

// A feed's title and items, RSS or Atom.
function parse(xml, base) {
  const head = xml.split(/<(?:item|entry)\b/i)[0];
  const items = [...xml.matchAll(/<(item|entry)\b[\s\S]*?<\/\1>/gi)].map(([x]) => ({
    title: clip(text(tag(x, "title")), 200),
    link: web(text(tag(x, "link")) || decode(/<link\b(?![^>]*rel="(?:self|edit|replies|enclosure)")[^>]*href="([^"]+)"/i.exec(x)?.[1] ?? ""), base),
    at: Date.parse(text(tag(x, "pubDate") || tag(x, "published") || tag(x, "updated") || tag(x, "dc:date"))) || null,
    summary: clip(text(tag(x, "description") || tag(x, "summary") || tag(x, "content")), 300),
  }));
  return { title: clip(text(tag(head, "title")), 200), items: items.filter((i) => i.title) };
}

// A GET, its redirects followed (a fetch answers them), each a step.
async function get(job, url) {
  let page = await job.fetch(url, { headers: { accept: ACCEPT } });
  for (let i = 0; i < REDIRECTS_MAX && page.status >= 300 && page.status < 400 && page.headers.get("location"); i++) {
    url = new URL(page.headers.get("location"), url).href;
    page = await job.fetch(url, { headers: { accept: ACCEPT } });
  }
  return { page, url };
}

const hourOf = (at, tz) => Number(new Intl.DateTimeFormat("en-US", { timeZone: tz, hour: "numeric", hourCycle: "h23" }).format(at));

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS feeds (
      id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL UNIQUE, title TEXT, problem TEXT, at INTEGER NOT NULL)`);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK (id = 1), hour INTEGER NOT NULL, tz TEXT, last INTEGER)");
  }

  // a feed, or a site's page that names one (`probe` finds it)
  add_feed({ url }) {
    const href = web(url.trim());
    if (!href) throw new Error("a feed is an http(s) URL");
    if (this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM feeds").one().n >= FEEDS_MAX) throw new Error(`at most ${FEEDS_MAX} feeds; remove one first`);
    if (this.ctx.storage.sql.exec("SELECT id FROM feeds WHERE url = ?", href).toArray().length) throw new Error("that feed is in your brief already");
    return this.ctx.storage.sql.exec("INSERT INTO feeds (url, at) VALUES (?, ?) RETURNING id", href, Date.now()).one();
  }

  remove_feed({ id }) {
    this.ctx.storage.sql.exec("DELETE FROM feeds WHERE id = ?", id);
    return { id };
  }

  // A new feed's first read: its title, or why it is no feed. A site's
  // page that names its feed (`<link rel="alternate">`) is swapped for it.
  async probe({ id }, job) {
    const feed = (await job.call("state", {})).feeds.find((f) => f.id === id);
    if (!feed) return { gone: true };
    let { page, url } = await get(job, feed.url);
    let found = page.ok ? parse(page.text(), url) : null;
    const named = found && !found.items.length && /<link\b[^>]*type="application\/(?:rss|atom)\+xml"[^>]*>/i.exec(page.text())?.[0];
    const href = named && web(decode(/href="([^"]+)"/i.exec(named)?.[1] ?? ""), url);
    if (href) {
      ({ page, url } = await get(job, href));
      found = page.ok ? parse(page.text(), url) : null;
    }
    if (!page.ok) return await job.call("probed", { id, problem: `it answered ${page.status}` });
    if (!found.items.length) return await job.call("probed", { id, problem: "there is no feed there, or it is empty" });
    return await job.call("probed", { id, url: clip(url, 2000), title: found.title || new URL(url).hostname });
  }

  probed({ id, url, title, problem }) {
    if (url && this.ctx.storage.sql.exec("SELECT id FROM feeds WHERE url = ? AND id != ?", url, id).toArray().length) {
      problem = "that feed is in your brief already";
      url = title = undefined;
    }
    this.ctx.storage.sql.exec("UPDATE feeds SET url = COALESCE(?, url), title = COALESCE(?, title), problem = ? WHERE id = ?", url ?? null, title ?? null, problem ?? null, id);
    return { id };
  }

  // the hour of the brief, where its readers are (the page sets it)
  set_time({ hour, tz }) {
    try {
      new Intl.DateTimeFormat("en-US", { timeZone: tz });
    } catch {
      throw new Error(`${tz} is not a time zone`);
    }
    this.ctx.storage.sql.exec("INSERT INTO settings (id, hour, tz) VALUES (1, ?, ?) ON CONFLICT (id) DO UPDATE SET hour = excluded.hour, tz = excluded.tz", hour, tz);
    return { hour, tz };
  }

  state() {
    const s = this.ctx.storage.sql.exec("SELECT hour, tz, last FROM settings WHERE id = 1").toArray()[0];
    const feeds = this.ctx.storage.sql.exec("SELECT id, url, title, problem FROM feeds ORDER BY id").toArray();
    return { feeds, hour: s?.hour ?? 7, tz: s?.tz ?? null, last: s?.last ?? null, now: Date.now() };
  }

  sent({ at }) {
    this.ctx.storage.sql.exec("INSERT INTO settings (id, hour, last) VALUES (1, 7, ?) ON CONFLICT (id) DO UPDATE SET last = excluded.last", at);
    return { at };
  }

  // A cron tick (`at`) makes a brief only at the brief's hour; a call
  // makes one now. A feed that does not answer is named in the brief.
  async brief({ at }, job) {
    const s = await job.call("state", {});
    const tz = s.tz ?? "UTC";
    if (at !== undefined && (hourOf(at, tz) !== s.hour || !s.feeds.length)) return { due: false };
    const now = at ?? s.now;
    const since = s.last ?? now - DAY_MS;
    const fresh = [];
    const failed = [];
    for (const feed of s.feeds) {
      const name = feed.title ?? new URL(feed.url).hostname;
      try {
        const { page, url } = await get(job, feed.url);
        if (!page.ok) throw new Error(`it answered ${page.status}`);
        const items = parse(page.text(), url).items.filter((i) => (i.at ? i.at > since : s.last === null));
        fresh.push(...items.slice(0, ITEMS_PER_FEED).map((i) => ({ ...i, feed: name })));
      } catch (e) {
        failed.push({ feed: name, why: clip(String(e.message), 200) });
      }
    }
    const items = fresh.sort((a, b) => (b.at ?? 0) - (a.at ?? 0)).slice(0, ITEMS_MAX);
    const date = new Intl.DateTimeFormat("en-US", { timeZone: tz, weekday: "long", month: "long", day: "numeric" }).format(now);
    let said = "Nothing new in your feeds since the last brief.";
    if (items.length) {
      const listed = items.map((i) => `${i.feed}: ${i.title}${i.summary ? `. ${i.summary}` : ""}`).join("\n");
      ({ text: said } = await job.ai.text({ messages: [{ role: "system", content: WRITE }, { role: "user", content: listed }], max_tokens: 1200 }));
    }
    said = clip(said.trim(), 6000);
    await job.publish("briefs", { date, at: now, text: said, items: items.map(({ feed, title, link }) => ({ feed, title, link })), failed }, "brief");
    await job.call("sent", { at: now });
    if (items.length) await job.push("briefs", { title: `Your brief for ${date}`, body: clip(said.replace(/^[-*\s]+/, ""), 140), tag: "brief", url: "./" });
    return { items: items.length, failed: failed.length };
  }
}
