// The mind's web (docs/optchat.md, "The web"): what the tools web_search,
// web_fetch and research do, through `job.fetch` alone, the app's one way
// out. Each is a few steps of the turn that calls it; their results are
// logged as any tool's (a `tool` and an `echo`, capped).
//
// A search provider is chosen by the fragment secret it needs, which the
// owner sets (`fragment secret set <mind> NAME`). The app never sees a
// secret nor which exist: a fetch names one as `{{NAME}}` in a header, and
// the platform adds it at the egress point or fails the step "no secret
// named NAME". So the keyed providers are tried in turn, and a secret found
// missing is passed over for the rest of the turn (`io.passed`). With no
// key it is DuckDuckGo's HTML page; when DuckDuckGo asks this server to
// prove it is human (it does, of a datacenter's address), Wikipedia's
// search, saying so, and DuckDuckGo is passed over for the rest of the
// turn too.
//
// `io` is the turn's: `fetch(url, init)` and `text(opts)` are its steps,
// `room(n)` whether n more steps may be taken, `passed` the providers (by
// their secret, or their name) passed over. A fetch's answer is kept whole
// in the run's 4 MiB of answers (a page up to 1 MiB), so `io.fetch`
// refuses past a run's room for one.
import { fence } from "./files.mjs";
import { RESEARCH, SONAR } from "./prompts.mjs";

export const SEARCH_MAX = 10;
export const SEARCH_DEFAULT = 6;
const REDIRECTS_MAX = 3;
const SNIPPET_CHARS = 400;
const TITLE_CHARS = 200;
// research: the results it reads, each cut to so much for the cheap model,
// and that model's answer.
const RESEARCH_RESULTS = 5;
const RESEARCH_PAGES = 3;
const RESEARCH_PAGE_CHARS = 12_000;
const RESEARCH_TOKENS = 2000;
// Asked again of a page over a fetch's 1 MiB: its head, where honoured.
const RANGE = "bytes=0-524287";
const UA = "Mozilla/5.0 (compatible; FragmentMind/1.0; +https://fragment.club)";
const KEYS_NOTE =
  "The owner can give the mind a search key, as one of its secrets (`fragment secret set <mind> NAME`): PERPLEXITY_API_KEY, BRAVE_API_KEY or TAVILY_API_KEY.";

const enc = encodeURIComponent;
const describe = (e) => (e instanceof Error ? e.message : String(e));
const cutChars = (s, n) => {
  const cps = Array.from(s);
  return cps.length > n ? `${cps.slice(0, n - 1).join("")}…` : s;
};
const httpUrl = (s, base) => {
  try {
    const u = new URL(String(s), base);
    return u.protocol === "http:" || u.protocol === "https:" ? u : null;
  } catch {
    return null;
  }
};

// ---- HTML, as text ----

const NAMED = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: "\u00a0", ensp: " ", emsp: " ", thinsp: " ", hairsp: " ",
  shy: "", zwj: "", zwnj: "", ndash: "–", mdash: "—", minus: "−", hellip: "…", lsquo: "‘", rsquo: "’", sbquo: "‚",
  ldquo: "“", rdquo: "”", bdquo: "„", laquo: "«", raquo: "»", lsaquo: "‹", rsaquo: "›", copy: "©", reg: "®",
  trade: "™", middot: "·", bull: "•", times: "×", divide: "÷", deg: "°", plusmn: "±", micro: "µ", para: "¶",
  sect: "§", euro: "€", pound: "£", yen: "¥", cent: "¢", larr: "←", rarr: "→", uarr: "↑", darr: "↓", harr: "↔",
  le: "≤", ge: "≥", ne: "≠", asymp: "≈", infin: "∞", frac12: "½", frac14: "¼", frac34: "¾", sup2: "²", sup3: "³",
  aacute: "á", agrave: "à", acirc: "â", auml: "ä", aring: "å", atilde: "ã", aelig: "æ", ccedil: "ç", eacute: "é",
  egrave: "è", ecirc: "ê", euml: "ë", iacute: "í", igrave: "ì", icirc: "î", iuml: "ï", ntilde: "ñ", oacute: "ó",
  ograve: "ò", ocirc: "ô", ouml: "ö", otilde: "õ", oslash: "ø", uacute: "ú", ugrave: "ù", ucirc: "û", uuml: "ü",
  yacute: "ý", yuml: "ÿ", szlig: "ß", Aacute: "Á", Eacute: "É", Iacute: "Í", Oacute: "Ó", Uacute: "Ú", Ntilde: "Ñ",
  Auml: "Ä", Ouml: "Ö", Uuml: "Ü", Ccedil: "Ç",
};

/// Character references decoded (named ones from a common set; numeric ones
/// all, a surrogate or 0 as U+FFFD). Only references ended by `;`.
export function decode(s) {
  return String(s).replace(/&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[A-Za-z][A-Za-z0-9]{1,31});/g, (m, e) => {
    if (e[0] === "#") {
      const n = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return n > 0 && n <= 0x10ffff && !(n >= 0xd800 && n <= 0xdfff) ? String.fromCodePoint(n) : "\uFFFD";
    }
    return NAMED[e] ?? m;
  });
}

const strip = (s) => String(s).replace(/<[^>]*>/g, "");
/// Inline HTML as one line of text.
const inline = (s) => decode(strip(s)).replace(/\s+/g, " ").trim();
const attr = (attrs, name) => {
  const m = new RegExp(`\\b${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, "i").exec(attrs);
  return m ? (m[1] ?? m[2] ?? m[3]) : null;
};

// Elements whose content is no reading: scripts, styles, media, and the
// page's chrome around its text (by its tag; by a role or a class that
// says so, on an element whose end tag is never left out; hidden ones).
const DROPPED = new Set(["head", "script", "style", "noscript", "template", "svg", "math", "canvas", "iframe", "object", "video", "audio", "nav", "footer", "aside", "button", "select", "dialog"]);
const CLOSED = new Set(["div", "section", "span", "ul", "ol", "table", "header", "form", "details", "menu", "figure", "sup"]);
const VOID = new Set(["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr", "param"]);
const CHROME_ROLE = /\brole\s*=\s*["']?(?:navigation|banner|contentinfo|search|menu|menubar|toolbar|dialog|complementary)\b/i;
const CHROME_CLASS = /^(?:nav|navbar|navbox|navigation|menu|sidebar|breadcrumbs?|dropdown|vector-dropdown|toc|mw-editsection|skip-link|reference|noprint)$/i;
const HIDDEN = /(?:^|\s)hidden(?:\s|=|$)|\baria-hidden\s*=\s*["']?true\b/i;
const TAG = /<(\/?)([a-zA-Z][a-zA-Z0-9-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g;
const BLOCK = /<\/?(?:div|section|article|main|header|ul|ol|dl|dt|dd|table|thead|tbody|tfoot|tr|blockquote|figure|figcaption|details|summary|address|form|fieldset|center)\b[^>]*>/gi;
const visible = (h) => h.replace(/<[^>]*>/g, "").replace(/\s+/g, " ").length;

function chrome(name, attrs) {
  if (DROPPED.has(name)) return true;
  if (!CLOSED.has(name)) return false;
  if (HIDDEN.test(attrs) || CHROME_ROLE.test(attrs)) return true;
  return (attr(attrs, "class") ?? "").split(/\s+/).some((c) => CHROME_CLASS.test(c));
}

// The page less its chrome: each such element cut with what it holds, to
// its matching end tag (the same tags counted); one whose end is not found
// stays.
function unchromed(h) {
  let out = "";
  let from = 0;
  const tags = new RegExp(TAG.source, "g");
  for (let m = tags.exec(h); m !== null; m = tags.exec(h)) {
    const name = m[2].toLowerCase();
    if (m[1] || !chrome(name, m[3])) continue;
    if (VOID.has(name) || /\/\s*$/.test(m[3])) {
      out += `${h.slice(from, m.index)} `;
      from = tags.lastIndex;
      continue;
    }
    const same = new RegExp(`<(\\/?)${name}\\b(?:[^>"']|"[^"]*"|'[^']*')*>`, "gi");
    same.lastIndex = tags.lastIndex;
    let depth = 1;
    for (let n = same.exec(h); n !== null && depth > 0; n = depth > 0 ? same.exec(h) : null) {
      if (n[1]) depth--;
      else if (!/\/\s*>$/.test(n[0])) depth++;
    }
    if (depth > 0) continue;
    out += `${h.slice(from, m.index)} `;
    from = same.lastIndex;
    tags.lastIndex = from;
  }
  return out + h.slice(from);
}

// The page's main part when it marks one (<main>, else its <article>s) and
// it holds text; else its body.
function mainPart(h) {
  const low = h.toLowerCase();
  for (const t of ["main", "article"]) {
    const a = low.search(new RegExp(`<${t}\\b`));
    const b = low.lastIndexOf(`</${t}>`);
    if (a >= 0 && b > a && visible(h.slice(a, b)) > 400) return h.slice(a, b);
  }
  const body = /<body\b[^>]*>/i.exec(h);
  return body ? h.slice(body.index + body[0].length) : h;
}

function link(attrs, inner, base) {
  const text = strip(inner).replace(/\s+/g, " ").trim();
  const href = attr(attrs, "href");
  const u = href === null ? null : httpUrl(decode(href), base);
  if (!text || u === null) return text ? ` ${text} ` : " ";
  // a link to a place on this same page is its words alone
  if (u.hash && u.href.split("#")[0] === String(base).split("#")[0]) return text;
  return `[${text}](${u.href})`;
}

/// A page's title and readable text, as markdown-ish lines: its main part
/// with scripts, styles and its chrome (nav, footer, aside) dropped;
/// headings as `#`, list items as `- `, links as `[words](url)` made
/// absolute against `base`, preformatted text fenced, tables' cells split
/// by ` | `, and blank lines between blocks.
export function htmlText(html, base) {
  const src = String(html).replace(/\u0000/g, "");
  const title = inline(/<title\b[^>]*>([\s\S]*?)<\/title>/i.exec(src)?.[1] ?? "");
  let h = unchromed(src.replace(/<!--[\s\S]*?-->/g, " "));
  h = mainPart(h);
  const pres = [];
  h = h.replace(/<pre\b[^>]*>([\s\S]*?)<\/pre>/gi, (_, inner) => {
    pres.push(decode(strip(inner.replace(/<br\s*\/?>/gi, "\n"))));
    return `\n\n\u0000${pres.length - 1}\u0000\n\n`;
  });
  h = h.replace(/<a\b([^>]*)>([\s\S]*?)<\/a>/gi, (_, attrs, inner) => link(attrs, inner, base));
  h = h.replace(/<h([1-6])\b[^>]*>/gi, (_, n) => `\n\n${"#".repeat(Number(n))} `).replace(/<\/h[1-6]\s*>/gi, "\n\n");
  h = h.replace(/<li\b[^>]*>/gi, "\n- ").replace(/<br\s*\/?>/gi, "\n").replace(/<hr\b[^>]*>/gi, "\n\n---\n\n");
  h = h.replace(/<\/(?:td|th)\s*>/gi, " | ").replace(/<\/?p\b[^>]*>/gi, "\n\n").replace(BLOCK, "\n");
  const text = decode(strip(h))
    .split("\n")
    .map((l) => l.replace(/[ \t\f\v\r\u00a0]+/g, " ").trim())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim()
    .replace(/\u0000(\d+)\u0000/g, (_, k) => fence(pres[Number(k)] ?? ""));
  return { title: cutChars(title, TITLE_CHARS), text };
}

// ---- reading a page ----

const HTML_TYPE = /^(?:text\/html|application\/xhtml\+xml)$/;
const TEXT_TYPE = /^(?:text\/|application\/(?:json|xml|javascript|x-javascript|yaml|x-yaml|toml|x-ndjson|ld\+json|rss\+xml|atom\+xml)$|application\/[a-z0-9.-]+\+(?:json|xml)$)/;

/// What a fetched answer reads as: an HTML page's title and text, a text
/// file's text, or why it is neither.
export function page(url, type, body) {
  const mime = String(type ?? "").split(";")[0].trim().toLowerCase();
  const sniffed = mime === "" || mime === "application/octet-stream";
  if (HTML_TYPE.test(mime) || (sniffed && /^\s*(?:<!doctype html|<html\b)/i.test(body))) return { url, ...htmlText(body, url) };
  if (TEXT_TYPE.test(mime) || (sniffed && !/[\u0000-\u0008\uFFFD]/.test(body.slice(0, 4096)))) return { url, title: "", text: body };
  return { url, error: `it is ${mime || "of no type"}, not a page or text: hand it to the computer to open` };
}

const WIKIPEDIA_HOST = /^([a-z][a-z0-9-]*)(?:\.m)?\.wikipedia\.org$/;

// A Wikipedia article: its page is up to megabytes of HTML (past a fetch's
// 1 MiB, whatever range is asked), so its plain text comes from
// Wikipedia's own API, a twentieth of it, its `== headings ==` as `#`s.
// Null when that does not answer: the page is read as any other.
async function wikipediaText(io, u) {
  const host = WIKIPEDIA_HOST.exec(u.hostname);
  if (host === null || !u.pathname.startsWith("/wiki/") || !io.room(2)) return null;
  try {
    const title = decodeURIComponent(u.pathname.slice("/wiki/".length));
    const api = `https://${host[1]}.wikipedia.org/w/api.php?action=query&prop=extracts&explaintext=1&redirects=1&format=json&formatversion=2&titles=${enc(title)}`;
    const res = await io.fetch(api, { headers: { "user-agent": UA, accept: "application/json" } });
    const p = res.status === 200 ? res.json()?.query?.pages?.[0] : null;
    if (typeof p?.extract !== "string" || !p.extract.trim()) return null;
    const text = p.extract.replace(/^(={2,6}) *(.+?) *\1 *$/gm, (_, eq, h) => `${"#".repeat(eq.length)} ${h}`);
    return { url: u.href, title: `${p.title} - Wikipedia`, text };
  } catch {
    return null;
  }
}

/// web_fetch: a page fetched (following at most REDIRECTS_MAX redirects;
/// one over a fetch's 1 MiB asked again for its head, where the server
/// honours a range) and read as text; a Wikipedia article from Wikipedia's
/// API. Answers {url, title, text} or {url, error}.
export async function read(io, raw) {
  let u = httpUrl(String(raw ?? "").trim());
  if (u === null) return { url: String(raw ?? ""), error: "web_fetch takes an http(s) URL" };
  const wiki = await wikipediaText(io, u);
  if (wiki !== null) return wiki;
  let ranged = false;
  for (let hops = 0; ; ) {
    if (!io.room(1)) return { url: u.href, error: "this turn is out of steps for the web" };
    const headers = { "user-agent": UA, accept: "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5" };
    if (ranged) Object.assign(headers, { range: RANGE, "accept-encoding": "identity" });
    let res;
    try {
      res = await io.fetch(u.href, { headers });
    } catch (e) {
      const why = describe(e);
      if (!ranged && /more than \d+ bytes|result is over/.test(why)) {
        ranged = true;
        continue;
      }
      return { url: u.href, error: why };
    }
    if (res.status >= 300 && res.status < 400) {
      const next = httpUrl(res.headers.get("location") ?? "", u);
      if (next === null) return { url: u.href, error: `it answered ${res.status} with no http(s) place to go` };
      if (++hops > REDIRECTS_MAX) return { url: u.href, error: `it redirected more than ${REDIRECTS_MAX} times` };
      u = next;
      continue;
    }
    if (res.status >= 400) return { url: u.href, error: `it answered ${res.status}` };
    const p = page(u.href, res.headers.get("content-type"), res.text());
    return ranged && !p.error ? { ...p, text: `${p.text}\n\n[… the page goes on past its first 512 KiB …]` } : p;
  }
}

/// A read page as a tool's result: its title, its address, its text (the
/// turn caps it, head and tail, as every result).
export function pageText(p) {
  if (p.error) return `Error: ${p.url}: ${p.error}.`;
  const text = p.text.trim() || "(no text)";
  return p.title ? `# ${p.title}\n${p.url}\n\n${text}` : `${p.url}\n\n${text}`;
}

// ---- searching ----

// Search results from a provider's list: each an http(s) URL, a title, and
// a snippet as one line of text.
function results(list, one) {
  if (!Array.isArray(list)) throw new Error("its answer has no list of results");
  const out = [];
  for (const x of list) {
    const r = one(x ?? {});
    const u = httpUrl(r.url ?? "");
    if (u === null || out.some((o) => o.url === u.href)) continue;
    out.push({ title: cutChars(inline(r.title ?? ""), TITLE_CHARS), url: u.href, snippet: cutChars(inline(r.snippet ?? ""), SNIPPET_CHARS) });
  }
  return out;
}

const REFUSED = "it asked this server to prove it is human";

/// DuckDuckGo's HTML results page, read: each result's link (its target,
/// out of DuckDuckGo's redirect; an ad has none) and its snippet. Throws
/// when the page is DuckDuckGo asking this server to prove it is human.
export function ddgResults(status, html) {
  if (status === 202 || /anomaly-modal|challenge-form|g-recaptcha/.test(html)) throw new Error(REFUSED);
  const out = [];
  let last = null;
  for (const m of String(html).matchAll(/<a\b([^>]*)>([\s\S]*?)<\/a>/gi)) {
    const cls = attr(m[1], "class") ?? "";
    if (/\bresult__a\b/.test(cls)) {
      const href = httpUrl(decode(attr(m[1], "href") ?? ""), "https://duckduckgo.com/");
      const target = href && href.hostname.endsWith("duckduckgo.com") ? httpUrl(href.searchParams.get("uddg") ?? "") : href;
      last = target && !out.some((o) => o.url === target.href) ? { title: m[2], url: target.href, snippet: "" } : null;
      if (last) out.push(last);
    } else if (/\bresult__snippet\b/.test(cls) && last && !last.snippet) last.snippet = m[2];
  }
  return results(out, (x) => x);
}

const json = (body) => ({ method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
const authed = (init, headers) => ({ ...init, headers: { ...init.headers, ...headers } });

/// The search providers, in the order they are tried: those a secret
/// names, then DuckDuckGo with none. Each makes its request and reads its
/// answer's results.
export const SEARCHERS = [
  {
    name: "Perplexity",
    secret: "PERPLEXITY_API_KEY",
    request: (q, n) => ["https://api.perplexity.ai/search", authed(json({ query: q, max_results: n }), { authorization: "Bearer {{PERPLEXITY_API_KEY}}" })],
    results: (res) => results(res.json().results, (x) => ({ title: x.title, url: x.url, snippet: x.snippet })),
  },
  {
    name: "Brave",
    secret: "BRAVE_API_KEY",
    request: (q, n) => [`https://api.search.brave.com/res/v1/web/search?q=${enc(q)}&count=${n}`, { headers: { accept: "application/json", "x-subscription-token": "{{BRAVE_API_KEY}}" } }],
    results: (res) => results(res.json().web?.results ?? [], (x) => ({ title: x.title, url: x.url, snippet: x.description })),
  },
  {
    name: "Tavily",
    secret: "TAVILY_API_KEY",
    request: (q, n) => ["https://api.tavily.com/search", authed(json({ query: q, max_results: n }), { authorization: "Bearer {{TAVILY_API_KEY}}" })],
    results: (res) => results(res.json().results, (x) => ({ title: x.title, url: x.url, snippet: x.content })),
  },
  {
    name: "DuckDuckGo",
    secret: null,
    request: (q) => [`https://html.duckduckgo.com/html/?q=${enc(q)}`, { headers: { "user-agent": UA, accept: "text/html" } }],
    results: (res) => ddgResults(res.status, res.text()),
  },
];

// When DuckDuckGo gives nothing: Wikipedia's own search, which answers any
// server (Wikimedia asks for a user agent that says who calls).
const WIKIPEDIA = {
  name: "Wikipedia",
  secret: null,
  request: (q, n) => [`https://en.wikipedia.org/w/api.php?action=query&list=search&format=json&utf8=1&srlimit=${n}&srsearch=${enc(q)}`, { headers: { "user-agent": UA, accept: "application/json" } }],
  results: (res) =>
    results(res.json().query?.search, (x) => ({ title: x.title, url: `https://en.wikipedia.org/wiki/${enc(String(x.title ?? "").replaceAll(" ", "_"))}`, snippet: x.snippet })),
};

/// web_search: the first provider that answers with results, its keyed
/// ones first (a missing secret passed over, and remembered in
/// `io.passed`), then DuckDuckGo (remembered there too when it refuses
/// this server), then Wikipedia. Answers {provider, results: [{title, url,
/// snippet}], notes}: `notes` says what failed on the way (a key that was
/// refused, a provider down).
export async function search(io, q, n = SEARCH_DEFAULT) {
  const notes = [];
  for (const p of [...SEARCHERS, WIKIPEDIA]) {
    if (io.passed.has(p.secret ?? p.name)) {
      if (p.secret === null) notes.push(`${p.name}: passed over (it refused this server this turn)`);
      continue;
    }
    if (!io.room(1)) {
      notes.push("this turn is out of steps for the web");
      break;
    }
    let res;
    try {
      res = await io.fetch(...p.request(q, n));
    } catch (e) {
      const why = describe(e);
      if (p.secret !== null && why.includes(`no secret named ${p.secret}`)) io.passed.add(p.secret);
      else notes.push(`${p.name}: ${why}`);
      continue;
    }
    let found = [];
    try {
      // DuckDuckGo's challenge page comes as a 202, which its reader names
      const read = res.status === 200 || (p.name === "DuckDuckGo" && res.status === 202);
      if (!read) throw new Error(`it answered ${res.status}${res.status === 401 || res.status === 403 ? " (is its key right?)" : ""}`);
      found = p.results(res);
    } catch (e) {
      const why = describe(e);
      if (why === REFUSED) io.passed.add(p.name);
      notes.push(`${p.name}: ${why}`);
      continue;
    }
    if (found.length) return { provider: p.name, results: found.slice(0, n), notes };
    notes.push(`${p.name}: no results`);
  }
  return { provider: null, results: [], notes };
}

/// Search results as a tool's result: numbered, each its title, its
/// address and its snippet; then what failed on the way, and, when only
/// Wikipedia answered, why and what would search the whole web.
export function searchText(q, r) {
  const tried = r.notes.length ? `\n(${r.notes.join("; ")})` : "";
  if (!r.results.length) return `No results for ${JSON.stringify(q)}.${tried}${r.provider === null ? `\n${KEYS_NOTE}` : ""}`;
  const lines = r.results.map((x, k) => `${k + 1}. ${x.title || x.url}\n   ${x.url}${x.snippet ? `\n   ${x.snippet}` : ""}`);
  const only = r.provider === "Wikipedia" ? `\nThese are Wikipedia's alone (no other search answered with results). ${KEYS_NOTE}` : "";
  return `${r.provider}'s results for ${JSON.stringify(q)}:\n${lines.join("\n")}${tried}${only}`;
}

// ---- research ----

/// research: a question answered with numbered sources. With
/// PERPLEXITY_API_KEY, one call of Perplexity's sonar, which searches and
/// cites itself; else a search, the best few results read (while the turn
/// has room), and one cheap call that answers from them alone.
export async function research(io, question) {
  const notes = [];
  if (!io.passed.has("PERPLEXITY_API_KEY") && io.room(2)) {
    try {
      const res = await io.fetch(
        "https://api.perplexity.ai/chat/completions",
        authed(json({ model: "sonar", messages: [{ role: "system", content: SONAR }, { role: "user", content: question }] }), { authorization: "Bearer {{PERPLEXITY_API_KEY}}" }),
      );
      const j = res.status === 200 ? res.json() : null;
      const answer = String(j?.choices?.[0]?.message?.content ?? "").trim();
      if (answer) {
        const cited = Array.isArray(j.search_results) && j.search_results.length ? j.search_results : (Array.isArray(j.citations) ? j.citations : []).map((url) => ({ url }));
        const sources = results(cited, (x) => ({ title: x.title ?? x.url, url: x.url }));
        return `${answer}${sources.length ? `\n\nSources:\n${sources.map((s, k) => `[${k + 1}] ${s.title}: ${s.url}`).join("\n")}` : ""}\n(researched by Perplexity's sonar)`;
      }
      notes.push(`Perplexity: it answered ${res.status}${res.status === 200 ? " with no answer" : ""}`);
    } catch (e) {
      const why = describe(e);
      if (why.includes("no secret named PERPLEXITY_API_KEY")) io.passed.add("PERPLEXITY_API_KEY");
      else notes.push(`Perplexity: ${why}`);
    }
  }
  const found = await search(io, question, RESEARCH_RESULTS);
  found.notes.unshift(...notes);
  if (!found.results.length) return `Could not research ${JSON.stringify(question)}: ${searchText(question, found)}`;
  const pages = [];
  for (const r of found.results) {
    // a page's step, and the answer's call after
    if (pages.length >= RESEARCH_PAGES || !io.room(2)) break;
    const p = await read(io, r.url);
    if (!p.error && p.text.trim()) pages.push({ title: p.title || r.title, url: p.url, text: p.text });
  }
  if (!pages.length) return `${searchText(question, found)}\n\n(None of these pages could be read: answer from the snippets, or web_fetch one.)`;
  const sources = pages.map((p, k) => `[${k + 1}] ${p.title}\n${p.url}\n\n${cutChars(p.text, RESEARCH_PAGE_CHARS)}`).join("\n\n---\n\n");
  let answer = "";
  try {
    const a = await io.text({
      model: "cheap",
      messages: [
        { role: "system", content: RESEARCH },
        { role: "user", content: `Question: ${question}\n\nSources:\n\n${sources}` },
      ],
      max_tokens: RESEARCH_TOKENS,
    });
    answer = String(a?.text ?? "").trim();
  } catch (e) {
    found.notes.push(`the answer's call failed: ${describe(e)}`);
  }
  const list = pages.map((p, k) => `[${k + 1}] ${p.title}: ${p.url}`).join("\n");
  if (!answer) return `${searchText(question, found)}\n\n(The pages were read but no answer was written: web_fetch one.)`;
  return `${answer}\n\nSources:\n${list}\n(researched through ${found.provider}'s search, ${pages.length} pages read${found.notes.length ? `; ${found.notes.join("; ")}` : ""})`;
}
