// The agent's page: its name (the fragment's title), its picture
// (`avatar.png`, else its image in its colour, by its identity as the shell
// and its chats choose it), its model (`agent.json`: its tier, or a model
// of a provider its owner connected, `model: {provider, id}`), its job
// (`SOUL.md`) and the skills in its repo, read from the fragment's own
// files (`__file`, `__files`). Its name and picture are changed from
// Settings, or by the agent (`fragment agent`: docs/api.md, "An agent's
// name and picture").
import { colorOf } from "./agent-color.js";

// This fragment's name, at the start of its host (docs/api.md, Names).
const NAME_AT = /^[a-z0-9](?:-?[a-z0-9])*--[a-km-np-z2-9]{4}/;
// Identities one `__people` call names (the platform's limit).
const PEOPLE_MAX = 64;

const text = async (path) => {
  const r = await fetch(`__file?path=${encodeURIComponent(path)}`, { credentials: "same-origin" });
  return r.ok ? r.text() : null;
};
const json = (path) => fetch(path, { headers: { accept: "application/json" }, credentials: "same-origin" }).then((r) => (r.ok ? r.json() : null)).catch(() => null);

// Its identity: of its agent members (`__members`; once its computer took
// it, it is an editor of its own fragment), the one made from this
// fragment (`__people`'s `fragment`); none before a computer takes it.
async function identity() {
  const here = location.hostname.match(NAME_AT)?.[0];
  const agents = ((await json("__members"))?.members ?? []).filter((m) => m.kind === "agent").map((m) => m.principal).slice(0, PEOPLE_MAX);
  if (!here || !agents.length) return null;
  const people = await json(`__people?${agents.map((id) => `id=${encodeURIComponent(id)}`).join("&")}`);
  return agents.find((id) => people?.profiles?.[id]?.fragment === here) ?? null;
}

async function main() {
  window.addEventListener("message", (e) => {
    if (e.data?.fragment === "theme" && (e.data.mode === "light" || e.data.mode === "dark")) document.documentElement.dataset.theme = e.data.mode;
  });
  const who = identity();
  // the platform puts the fragment's own title on its page (Open Graph)
  const name = document.querySelector('meta[property="og:title"]')?.content || location.hostname.split("--")[0] || "Agent";
  document.title = name;
  document.getElementById("name").textContent = name;
  let settings = {};
  try {
    settings = JSON.parse((await text("agent.json")) || "{}");
  } catch {}
  const avatar = document.getElementById("avatar");
  avatar.style.setProperty("--agent-color", colorOf(await who));
  const picture = new Image();
  picture.alt = "";
  picture.onload = () => {
    avatar.classList.add("picture");
    avatar.replaceChildren(picture);
  };
  picture.src = "__file?path=avatar.png";
  document.getElementById("tier").textContent = settings.model?.id ? `${settings.model.id} (${settings.model.provider})` : settings.tier ? `${settings.tier} model` : "";
  const soul = await text("SOUL.md");
  const box = document.getElementById("soul");
  box.textContent = soul?.trim() || "No job written down yet.";
  if (!soul) box.classList.add("muted");
  const files = (await json("__files")) ?? { files: [] };
  const skills = [...new Set(files.files.map((f) => f.path).filter((p) => p.startsWith("skills/") && p.endsWith("/SKILL.md")).map((p) => p.split("/")[1]))];
  if (skills.length) {
    const list = document.getElementById("skills");
    list.replaceChildren(...skills.map((s) => Object.assign(document.createElement("li"), { textContent: s })));
  }
}

main();
