//! The platform's markdown renderer (cell/markdown.mjs, `./__markdown.js`
//! on every fragment) on the pinned Node, against a DOM of its own that
//! records everything the renderer makes and refuses any property it was
//! not built to set. The renderer's promise is that text can make only its
//! own elements, with only plain properties, and links and pictures only of
//! safe schemes, whatever the text and whatever its caller's hooks answer:
//! these tests hold it to that with hostile markdown (raw HTML, `<img
//! onerror>`, script URLs in every disguise, deep nesting, delimiter runs),
//! and check that ordinary markdown still renders as it should. The e2e
//! renders the same file in Chrome (the notes, brain and chat sections).

use std::io::Write;
use std::process::Stdio;

use fragment_devstack as devstack;
use serde_json::Value;

/// A DOM just large enough for the renderer, refusing what it should never
/// touch. Every element is a Proxy: setting a property outside SETTABLE
/// (markup, an event handler, anything) throws, so a renderer that grew a
/// sink fails here, not in a person's browser.
const DOM: &str = r##"
const SETTABLE = new Set(["className", "textContent", "href", "src", "alt", "title", "target", "rel", "loading", "width", "start", "type", "checked", "disabled", "tabIndex"]);
const ATTRIBUTES = new Set(["aria-label", "role"]);
const made = [];
class FakeNode {
  constructor(name) { this.nodeName = name; this.childNodes = []; }
  append(...kids) {
    for (const k of kids) {
      if (typeof k === "string") this.childNodes.push(new FakeText(k));
      else if (k instanceof FakeNode && k.nodeName === "#document-fragment") this.childNodes.push(...k.childNodes.splice(0));
      else if (k instanceof FakeNode) this.childNodes.push(k);
      else throw new Error("appended a " + typeof k);
    }
  }
  get textContent() { return this.childNodes.map((c) => c.textContent).join(""); }
}
class FakeText extends FakeNode {
  constructor(data) { super("#text"); this.data = data; }
  get textContent() { return this.data; }
}
class FakeElement extends FakeNode {
  constructor(tag) { super(tag.toUpperCase()); this.tag = tag; this.props = {}; this.attrs = {}; this.dataset = {}; this.style = {}; }
  setAttribute(name, value) {
    if (!ATTRIBUTES.has(name)) throw new Error("setAttribute " + name);
    this.attrs[name] = String(value);
  }
}
globalThis.Node = FakeNode;
globalThis.document = {
  createElement(tag) {
    const el = new FakeElement(tag);
    made.push(tag);
    return new Proxy(el, {
      set(target, prop, value) {
        if (prop === "textContent") { target.childNodes = [new FakeText(String(value))]; return true; }
        if (!SETTABLE.has(prop)) throw new Error("set " + String(prop) + " on <" + tag + ">");
        target.props[prop] = value;
        return true;
      },
    });
  },
  createTextNode: (data) => new FakeText(String(data)),
  createDocumentFragment: () => new FakeNode("#document-fragment"),
};
const walk = (node, out = []) => { out.push(node); for (const c of node.childNodes) walk(c, out); return out; };
const elements = (root) => walk(root).filter((n) => n instanceof FakeElement);
const depth = (node) => 1 + Math.max(0, ...node.childNodes.map(depth));
const SAFE_LINK = /^(?:https?:|mailto:)/i;
const SAFE_SRC = /^https?:/i;
// a URL's scheme as a browser reads it (controls and spaces dropped)
const schemeOf = (url) => url.replace(/[\x00-\x20\x7f]/g, "").match(/^([a-z][a-z\d+.-]*):/i)?.[1];
"##;

/// The cases, each pushing `{name, ok, detail}`: hostile input first, then
/// the ordinary markdown the viewers depend on.
const CASES: &str = r##"
const results = [];
const check = (name, ok, detail = "") => results.push({ name, ok: !!ok, detail: String(detail) });
const permissive = { resolve: (t) => ({ href: t }), image: (s) => ({ src: s, href: s }) };
const render = (text, options) => { const root = new FakeNode("root"); root.append(m.markdown(text, options)); return root; };
const unsafe = (root) => elements(root).filter((e) => {
  for (const [prop, ok] of [["href", SAFE_LINK], ["src", SAFE_SRC]]) {
    const v = e.props[prop];
    if (v === undefined) continue;
    const scheme = schemeOf(String(v));
    if (scheme !== undefined && !ok.test(scheme + ":")) return true;
  }
  return false;
});
const hostile = [
  "<img src=x onerror=\"globalThis.ran = true\">",
  "<script>globalThis.ran = true</script>",
  "<a href=\"javascript:alert(1)\">x</a> <iframe srcdoc=\"<script>\">",
  "[a](javascript:alert(1)) [b](JaVaScRiPt:alert(1)) [c](\u0001javascript:alert(1)) [d](vbscript:x) [e](data:text/html,<script>)",
  "![a](javascript:alert(1)) ![b](data:image/svg+xml,<svg onload=alert(1)>) ![c](\u0007javascript:x)",
  "[[javascript:alert(1)]] ![[javascript:alert(1)]] <javascript:alert(1)> <data:text/html,x>",
  "[x](https://ok.example/\"onmouseover=\"alert(1)) **<b onclick=x>**",
  "| a | b |\n| --- | --- |\n| <img src=x onerror=y> | [z](javascript:1) |",
  "> [!note] <img src=x onerror=y>\n> - [x] <script>1</script>\n\n```html\n<script>alert(1)</script>\n```",
];
for (const [i, text] of hostile.entries()) {
  for (const [label, options] of [["default hooks", {}], ["hooks that allow anything", permissive]]) {
    let root;
    try { root = render(text, options); } catch (e) { check(`hostile ${i} (${label}) renders`, false, e.stack); continue; }
    const tags = elements(root).map((e) => e.tag);
    check(`hostile ${i} (${label}): only the renderer's own elements`, tags.every((t) => m.ELEMENTS.includes(t)) && !tags.some((t) => ["script", "iframe", "object", "embed", "svg", "style", "link", "form"].includes(t)), tags.join(","));
    check(`hostile ${i} (${label}): no link or picture of an unsafe scheme`, unsafe(root).length === 0, JSON.stringify(unsafe(root).map((e) => e.props)));
    check(`hostile ${i} (${label}): markup in it is text`, !/<(img|script|iframe|a|b) /.test(text) || root.textContent.includes("<"), root.textContent);
  }
}
check("no script ran", globalThis.ran === undefined);
check("only the renderer's own elements are ever made", made.every((t) => m.ELEMENTS.includes(t)), [...new Set(made)].join(","));
{
  const root = render("<img src=x onerror=alert(1)>");
  check("an <img onerror> is a paragraph of its own text", elements(root).length === 1 && elements(root)[0].tag === "p" && root.textContent === "<img src=x onerror=alert(1)>", root.textContent);
}
{
  const root = render("[x](javascript:alert(1))", permissive);
  const e = elements(root);
  check("a script link is its words, marked missing, even when a hook allows it", e.length === 2 && e[1].tag === "span" && e[1].props.className === "md-missing" && root.textContent === "x", JSON.stringify(e.map((x) => [x.tag, x.props])));
}
{
  const t0 = Date.now();
  const deep = render("> ".repeat(5000) + "deep");
  check("5000 nested quotes stop at DEPTH_MAX, as text", depth(deep) <= m.DEPTH_MAX + 4 && deep.textContent.endsWith("deep"), depth(deep));
  const list = render(Array.from({ length: 2000 }, (_, i) => "  ".repeat(i) + "- item").join("\n"));
  check("2000 nested list items stop at DEPTH_MAX", depth(list) <= 2 * m.DEPTH_MAX + 4, depth(list));
  const emphasis = render("*".repeat(3000) + "x" + "*".repeat(3000));
  check("a run of emphasis marks stops at DEPTH_MAX", depth(emphasis) <= m.DEPTH_MAX + 4, depth(emphasis));
  render("*a ".repeat(40000));
  render("[".repeat(20000) + "](".repeat(20000));
  render("`".repeat(30000));
  render("> - ".repeat(3000) + "x\n".repeat(3000));
  check("hostile delimiter runs render in bounded time", Date.now() - t0 < 10000, `${Date.now() - t0} ms`);
}
{
  let threw = 0;
  for (const bad of [{ resolve: () => "javascript:alert(1)" }, { image: () => 7 }, { code: () => "<script>" }, { resolve: "no" }]) {
    try { render("[a](b) ![c](d)\n\n```\nx\n```", bad); } catch (e) { if (e instanceof TypeError) threw++; }
  }
  check("a hook's answer of the wrong shape is a TypeError, never markup", threw === 4, threw);
}
// ordinary markdown, as the viewers and the chat render it
{
  const root = render("# Hello *world*\n\nSome **bold**, `code`, ~~gone~~, ==marked== and a [link](https://example.com \"its title\").\n\n- [x] done\n- [ ] todo\n  1. nested\n\n| a | b |\n| :-- | --: |\n| `x\\|y` | a\\|b |\n\n> [!warning] Careful\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\npaul@example.com and https://example.com/x.");
  const e = elements(root);
  const one = (tag) => e.find((x) => x.tag === tag);
  check("a heading has its anchor", one("h1")?.dataset.anchor === "hello-world", JSON.stringify(one("h1")?.dataset));
  check("inline spans render", ["strong", "code", "del", "mark", "em"].every((t) => one(t)), e.map((x) => x.tag).join(","));
  const links = e.filter((x) => x.tag === "a");
  check("a web link opens in a tab, with no opener or referrer", links.some((a) => a.props.href === "https://example.com" && a.props.title === "its title" && a.props.target === "_blank" && a.props.rel === "noopener noreferrer"), JSON.stringify(links.map((a) => a.props)));
  check("bare emails and URLs are links", links.some((a) => a.props.href === "mailto:paul@example.com") && links.some((a) => a.props.href === "https://example.com/x"), JSON.stringify(links.map((a) => a.props.href)));
  const boxes = e.filter((x) => x.tag === "input");
  check("task boxes are disabled, labelled, one checked", boxes.length === 2 && boxes.every((b) => b.props.disabled && b.props.type === "checkbox" && b.attrs["aria-label"]) && boxes.filter((b) => b.props.checked).length === 1, JSON.stringify(boxes.map((b) => [b.props, b.attrs])));
  const tds = e.filter((x) => x.tag === "td");
  check("a table keeps escaped and code pipes, and its alignment", tds.length === 2 && tds[0].textContent === "x|y" && tds[1].textContent === "a|b" && tds[1].style.textAlign === "right" && one("div")?.attrs.role === "region", JSON.stringify(tds.map((t) => [t.textContent, t.style])));
  check("a callout is a quote of its kind", e.some((x) => x.tag === "blockquote" && x.props.className === "md-callout" && x.dataset.kind === "warning"));
  check("fenced code is literal, with its language", one("pre")?.dataset.lang === "rust" && one("pre")?.textContent === "fn main() {}" && one("hr"));
}
{
  const root = render("[[Note|its words]] ![[pic.png|120]] [rel](notes/b.md)", {
    resolve: (t, kind) => ({ href: "#/" + encodeURIComponent(t), path: kind + ":" + t }),
    image: (s, kind) => (kind === "wiki" ? { src: "__file?path=" + encodeURIComponent(s) } : null),
  });
  const e = elements(root);
  const wl = e.find((x) => x.props.className === "md-wikilink");
  check("a wikilink opens its file in the page", wl?.dataset.path === "wiki:Note" && wl?.textContent === "its words" && wl?.props.target === undefined, JSON.stringify(wl?.props));
  const img = e.find((x) => x.tag === "img");
  check("an embed is the page's picture, at its width", img?.props.src === "__file?path=pic.png" && img?.props.width === 120 && img?.props.loading === "lazy", JSON.stringify(img?.props));
  check("a relative link goes where the page says", e.some((x) => x.tag === "a" && x.dataset.path === "link:notes/b.md"));
}
{
  const p = new FakeNode("p");
  m.inline(p, "line one\nline two", { breaks: true });
  check("breaks: every newline breaks its line", p.childNodes.some((c) => c.nodeName === "BR"));
  const q = new FakeNode("p");
  m.inline(q, "line one\nline two");
  check("without breaks a newline is soft", !q.childNodes.some((c) => c.nodeName === "BR"));
  check("front matter is dropped", m.withoutFrontMatter("---\ntitle: x\n---\n# Body") === "# Body");
}
console.log(JSON.stringify(results));
"##;

/// Goal: the renderer turns any text into its own elements and safe URLs
/// only, within its limits, and still renders markdown. Method: the
/// renderer imported by Node from cell/markdown.mjs into the recording DOM
/// above, every case checked there, the results read back as JSON.
#[test]
fn the_markdown_renderer_makes_only_its_own_dom_from_hostile_text() {
    let root = devstack::repo_root();
    let node = devstack::node::locate(&root.join(devstack::TOOLS_DIR)).expect("the pinned Node");
    let url = format!("file://{}", root.join("cell/markdown.mjs").display());
    let script = format!("{DOM}\nconst m = await import({});\n{CASES}", serde_json::to_string(&url).unwrap());
    let mut child = node
        .command(&root.join(devstack::CACHE_DIR))
        .expect("a Node command")
        .args(["--input-type=module", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node runs");
    child.stdin.take().expect("stdin is piped").write_all(script.as_bytes()).expect("the script is written");
    let out = child.wait_with_output().expect("node finishes");
    assert!(out.status.success(), "node failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let results: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert!(results.len() >= 40, "the cases ran: {}", results.len());
    let failed: Vec<String> = results.iter().filter(|r| r["ok"] != true).map(|r| format!("{}: {}", r["name"], r["detail"])).collect();
    assert!(failed.is_empty(), "{} of {} failed:\n{}", failed.len(), results.len(), failed.join("\n"));
}
