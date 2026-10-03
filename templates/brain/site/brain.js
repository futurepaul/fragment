// The brain's search, beside the notes viewer (assets/: the notes
// template's prebuilt bundle, which this file leaves as it is; see the
// debt ledger's "The notes viewer is a prebuilt bundle"). The box asks the
// brain's search operation and lists its ranked sections in the reader; a
// result opens its page through the viewer's own hash route. Text from the
// brain becomes DOM through textContent only.
import { call } from "./__fragment.js";

const RESULTS = 20;
const TYPING_MS = 250;

const $ = (id) => document.getElementById(id);
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// the brain's own title, from its face (the page's Open Graph tags)
const title = document.querySelector('meta[property="og:title"]')?.getAttribute("content");
if (title) {
  $("brand").textContent = title;
  document.title = title;
}

// the viewer draws the page the hash names again (a result for the page
// already open changes no hash)
const reroute = () => window.dispatchEvent(new HashChangeEvent("hashchange"));

function show(answer, error) {
  const note = el("div", "note brain-results");
  note.append(el("h1", null, "Search"));
  const results = answer?.results ?? [];
  const said = error
    ? error
    : !answer.words.length
      ? "Type a word or two."
      : results.length
        ? `${results.length} section${results.length === 1 ? "" : "s"} with ${answer.words.join(", ")}, best first.`
        : `Nothing in this brain has ${answer.words.join(", ")}.`;
  note.append(el("p", error ? "hint brain-error" : "hint", said));
  if (answer?.pending > 0) note.append(el("p", "hint", "The brain is still taking in a sync: search again in a moment for the rest."));
  const list = el("ol", "brain-list");
  for (const r of results) {
    const item = el("li", "brain-result");
    const link = el("a", "brain-title", r.title);
    link.href = "#/" + encodeURIComponent(r.path);
    link.dataset.path = r.path;
    link.addEventListener("click", () => {
      if (location.hash === link.getAttribute("href")) reroute();
    });
    const where = [r.wiki, ...(r.ancestry ?? []), r.heading].filter(Boolean).join(" › ");
    item.append(link);
    if (where) item.append(el("div", "brain-where", where));
    if (r.snippet) item.append(el("div", "brain-snippet", r.snippet));
    item.append(el("div", "brain-path", r.path));
    list.append(item);
  }
  note.append(list);
  $("content").replaceChildren(note);
  $("main").scrollTop = 0;
}

let asked = 0;
async function search(q) {
  const mine = ++asked;
  try {
    const answer = await call("search", { q, limit: RESULTS });
    if (mine === asked) show(answer);
  } catch (e) {
    if (mine === asked) show(null, e.message || String(e));
  }
}

let typing = null;
const box = $("q");
box.addEventListener("input", () => {
  clearTimeout(typing);
  const q = box.value.trim();
  if (!q) {
    asked++;
    reroute();
    return;
  }
  typing = setTimeout(() => search(q), TYPING_MS);
});
$("search").addEventListener("submit", (e) => {
  e.preventDefault();
  clearTimeout(typing);
  const q = box.value.trim();
  if (q) search(q);
});
