// The skills fragment's page: its skills by category, read from the
// fragment's own files (`__files`, `__file`). The release's managed set
// (decision 17) is among them, and a file of the fragment's own at the same
// path wins over the release's.
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

// A skill is skills/<category>/<name>/SKILL.md, or skills/<name>/SKILL.md
// with no category.
export function skillsOf(paths) {
  const out = [];
  for (const path of paths) {
    const parts = path.split("/");
    if (parts[0] !== "skills" || parts[parts.length - 1] !== "SKILL.md") continue;
    if (parts.length === 4) out.push({ category: parts[1], name: parts[2], path });
    else if (parts.length === 3) out.push({ category: "", name: parts[1], path });
  }
  return out.sort((a, b) => a.category.localeCompare(b.category) || a.name.localeCompare(b.name));
}

// a SKILL.md's frontmatter description, its first line
function described(text) {
  const front = text.startsWith("---\n") ? text.slice(4, text.indexOf("\n---", 4)) : "";
  const line = front.split("\n").find((l) => l.startsWith("description:"));
  return line ? line.slice("description:".length).trim().replace(/^["'>]\s*|["']$/g, "") : "";
}

const read = async (path) => {
  const r = await fetch(`__file?path=${encodeURIComponent(path)}`, { credentials: "same-origin" });
  return r.ok ? r.text() : "";
};

async function main() {
  window.addEventListener("message", (e) => {
    if (e.data?.fragment === "theme" && (e.data.mode === "light" || e.data.mode === "dark")) document.documentElement.dataset.theme = e.data.mode;
  });
  const title = document.querySelector('meta[property="og:title"]')?.content;
  if (title) document.title = title;
  const listed = await fetch("__files", { headers: { accept: "application/json" }, credentials: "same-origin" }).then((r) => (r.ok ? r.json() : { files: [] })).catch(() => ({ files: [] }));
  const skills = skillsOf((listed.files ?? []).map((f) => f.path));
  const box = $("categories");
  if (!skills.length) {
    box.replaceChildren(el("p", "muted", "No skills yet."));
    return;
  }
  const groups = new Map();
  for (const s of skills) {
    if (!groups.has(s.category)) groups.set(s.category, []);
    groups.get(s.category).push(s);
  }
  box.replaceChildren(...[...groups].map(([category, list]) => {
    const section = el("section");
    const items = el("ul");
    items.append(...list.map((s) => {
      const li = el("li");
      const b = el("button");
      b.type = "button";
      const about = el("small");
      b.append(el("strong", null, s.name), about);
      b.onclick = async () => {
        $("skill-name").textContent = s.name;
        $("skill-text").textContent = "Loading…";
        $("skill").showModal();
        $("skill-text").textContent = await read(s.path);
      };
      li.append(b);
      // its description, read as the list shows (a page of skills, a few dozen reads)
      read(s.path).then((t) => { about.textContent = described(t); });
      return li;
    }));
    section.append(el("h2", null, category ? category.replace(/-/g, " ") : "general"), items);
    return section;
  }));
}

main();
