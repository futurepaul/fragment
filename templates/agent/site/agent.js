// The agent's page: its name (the fragment's title), its colour and its
// model (`agent.json`: its tier, or a model of a provider its owner
// connected, `model: {provider, id}`), its job
// (`SOUL.md`) and the skills in its repo, read from the fragment's own
// files (`__file`, `__files`).
const COLORS = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];

const text = async (path) => {
  const r = await fetch(`__file?path=${encodeURIComponent(path)}`, { credentials: "same-origin" });
  return r.ok ? r.text() : null;
};

// a colour of the palette from the agent's name, until agent.json names one
function colorOf(name) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return COLORS[h % COLORS.length];
}

async function main() {
  window.addEventListener("message", (e) => {
    if (e.data?.fragment === "theme" && (e.data.mode === "light" || e.data.mode === "dark")) document.documentElement.dataset.theme = e.data.mode;
  });
  // the platform puts the fragment's own title on its page (Open Graph)
  const name = document.querySelector('meta[property="og:title"]')?.content || location.hostname.split("--")[0] || "Agent";
  document.title = name;
  document.getElementById("name").textContent = name;
  let settings = {};
  try {
    settings = JSON.parse((await text("agent.json")) || "{}");
  } catch {}
  document.getElementById("avatar").style.setProperty("--agent-color", settings.color || colorOf(name));
  document.getElementById("tier").textContent = settings.model?.id ? `${settings.model.id} (${settings.model.provider})` : settings.tier ? `${settings.tier} model` : "";
  const soul = await text("SOUL.md");
  const box = document.getElementById("soul");
  box.textContent = soul?.trim() || "No job written down yet.";
  if (!soul) box.classList.add("muted");
  const files = await fetch("__files", { headers: { accept: "application/json" }, credentials: "same-origin" }).then((r) => (r.ok ? r.json() : { files: [] })).catch(() => ({ files: [] }));
  const skills = [...new Set(files.files.map((f) => f.path).filter((p) => p.startsWith("skills/") && p.endsWith("/SKILL.md")).map((p) => p.split("/")[1]))];
  if (skills.length) {
    const list = document.getElementById("skills");
    list.replaceChildren(...skills.map((s) => Object.assign(document.createElement("li"), { textContent: s })));
  }
}

main();
