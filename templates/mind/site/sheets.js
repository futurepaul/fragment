// Sheets over the page: a persona's (name, face, instructions, hands, the
// default), and the settings (about me, the apps, other agents). A sheet
// is a dialog in the middle of a wide screen and rises from the bottom of
// a phone's.

import { avatar } from "./pieces.js";
import { F, S, changed, problem } from "./store.js";
import { copyButton, h, icon, iconButton, plural } from "./ui.js";

const FACES = ["🌿", "🛠️", "🧭", "🌙", "✨", "🦉", "🐙", "🍄", "🔭", "📚", "🎨", "🧪", "🌊", "🔥", "🍋", "🐝", "🪴", "🎧", "🧘", "🗺️", "🦊", "🐢", "☕", "🪐"];
// fragment.json's limits: persona_set's name and instructions, settings_set's about
const NAME_MAX = 48;
const INSTRUCTIONS_MAX = 4000;
const ABOUT_MAX = 8000;

/// Opens a sheet; returns its close.
export function sheet(titleText, body, { foot = null, cls = "" } = {}) {
  const prev = document.activeElement;
  const panel = h(`div.sheet${cls ? "." + cls : ""}`, { role: "dialog", "aria-modal": "true", "aria-label": titleText });
  const scrim = h("div.sheet-scrim", null, panel);
  panel.append(h("div.sheet-grab", { "aria-hidden": "true" }), h("header.sheet-head", null, h("h2", { text: titleText }), iconButton("x", "Close", () => close())), h("div.sheet-body", null, body));
  if (foot) panel.append(h("footer.sheet-foot", null, foot));
  const esc = (e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
  };
  function close() {
    document.removeEventListener("keydown", esc, true);
    scrim.classList.add("leaving");
    setTimeout(() => scrim.remove(), 180);
    prev?.focus?.();
  }
  scrim.addEventListener("pointerdown", (e) => e.target === scrim && close());
  document.addEventListener("keydown", esc, true);
  document.body.append(scrim);
  requestAnimationFrame(() => panel.querySelector("input, textarea, button:not(.icon-btn)")?.focus());
  return close;
}

function toggle(label, sub, on) {
  const input = h("input", { type: "checkbox", checked: !!on, role: "switch" });
  return { input, el: h("label.switch-row", null, h("span.switch-text", null, h("span", { text: label }), h("small", { text: sub })), input, h("span.switch", { "aria-hidden": "true" }, h("i"))) };
}

/// A persona's sheet: `p` to edit, or null for a new one.
export function personaSheet(p) {
  let face = p?.emoji || FACES[Math.floor(Math.random() * 8) + 3];
  const faceBtn = h("button.face-btn", { type: "button", "aria-label": "Choose its face" }, avatar({ emoji: face }, "xl"));
  const faceInput = h("input.field.face-input", { value: face, maxlength: 8, "aria-label": "Any emoji" });
  const palette = h("div.faces", { hidden: true }, FACES.map((f) => h("button.face", { type: "button", text: f, onclick: () => setFace(f) })), faceInput);
  function setFace(f) {
    face = f;
    faceBtn.replaceChildren(avatar({ emoji: f }, "xl"));
    faceInput.value = f;
  }
  faceBtn.addEventListener("click", () => (palette.hidden = !palette.hidden));
  faceInput.addEventListener("input", () => faceInput.value.trim() && setFace(faceInput.value.trim()));
  const name = h("input.field.big", { value: p?.name ?? "", placeholder: "Name", maxlength: NAME_MAX, "aria-label": "Name" });
  const instructions = h("textarea.field", { rows: 7, value: p?.instructions ?? "", maxlength: INSTRUCTIONS_MAX, placeholder: "Who is it, and how does it talk? What does it care about, and what does it leave alone?", "aria-label": "Instructions" });
  const hands = toggle("Hands", "Can hand work to your computer: files, the shell, the web.", p?.hands);
  const isDefault = toggle("Default", "New chats start with this persona.", p ? p.id === S.defaultPersona : false);
  if (p && p.id === S.defaultPersona) isDefault.input.disabled = true;
  const note = h("p.sheet-note", { text: "Every persona shares the same memory. Only the voice and the skills change." });
  const body = h("div.persona-form", null, h("div.face-row", null, faceBtn, name), palette, h("label.label", { text: "Instructions" }), instructions, hands.el, isDefault.el, note);
  const save = h("button.primary", { type: "button" }, p ? "Save" : "Make persona");
  const remove = p && p.id !== S.defaultPersona ? h("button.ghost.danger", { type: "button" }, icon("trash"), "Remove") : null;
  const close = sheet(p ? `Edit ${p.name}` : "New persona", body, { foot: [remove, h("span.grow"), h("button.ghost", { type: "button", onclick: () => close() }, "Cancel"), save] });

  save.addEventListener("click", async () => {
    const n = name.value.trim();
    if (!n) return name.focus();
    save.disabled = true;
    const input = { name: n, emoji: face, instructions: instructions.value.trim(), hands: hands.input.checked };
    if (p) input.id = p.id;
    try {
      const r = await F.call("persona_set", input);
      const id = p?.id ?? r?.id ?? null;
      if (isDefault.input.checked && !isDefault.input.disabled) {
        const target = id ?? (await F.call("personas", {})).personas?.find((x) => x.name === n)?.id;
        if (target) await F.call("persona_default", { id: target });
      }
      if (!p && id) S.chosen = id;
      changed();
      close();
    } catch (e) {
      save.disabled = false;
      problem(`Could not save ${n}: ${e.message}`);
    }
  });
  remove?.addEventListener("click", async () => {
    if (remove.dataset.sure !== "1") {
      remove.dataset.sure = "1";
      remove.lastChild.textContent = "Remove it? Its chats stay.";
      return;
    }
    try {
      await F.call("persona_remove", { id: p.id });
      if (S.chosen === p.id) S.chosen = null;
      changed();
      close();
    } catch (e) {
      problem(`Could not remove ${p.name}: ${e.message}`);
    }
  });
}

/// The platform's origin (its shell lists every app at `/?apps`): the page
/// that frames this one, else read from this host. A fragment's host is
/// `<label>--<user>[--<branch>].<zone>` and the platform's `[<branch>.]<zone>`;
/// the dev stack's fragments are `*.fragment.localhost`, its platform
/// 127.0.0.1. (A deployment whose fragments have a zone of their own, as
/// fragment.boats beside fragment.club, is not read right: the page has
/// no way to ask.)
export function platformOrigin() {
  const framing = location.ancestorOrigins?.[location.ancestorOrigins.length - 1];
  if (window.parent !== window && framing) return framing;
  const [first, ...rest] = location.hostname.split(".");
  const labels = first.split("--");
  if (location.hostname.endsWith(".fragment.localhost")) return `${location.protocol}//127.0.0.1${location.port ? `:${location.port}` : ""}`;
  if (labels.length >= 3) return `${location.protocol}//${labels.slice(2).join("--")}.${rest.join(".")}`;
  return `${location.protocol}//${rest.join(".")}`;
}

/// The fragment's name, as the CLI names it (its host's first label).
export const fragmentName = () => (location.hostname.includes("--") ? location.hostname.split(".")[0].split("--")[0] : "mind");

export function settingsSheet() {
  const about = h("textarea.field", { rows: 6, maxlength: ABOUT_MAX, value: S.about ?? "", placeholder: "Your name, what you do, who's who in your life, how you like answers. Every persona reads this.", "aria-label": "About you" });
  const saved = h("span.saved", { "aria-live": "polite" });
  const saveAbout = h("button.ghost.small", { type: "button" }, "Save");
  saveAbout.addEventListener("click", async () => {
    saveAbout.disabled = true;
    try {
      await F.call("settings_set", { about: about.value.trim() });
      S.about = about.value.trim();
      saved.textContent = "Saved";
      setTimeout(() => (saved.textContent = ""), 1600);
    } catch (e) {
      problem(`Could not save: ${e.message}`);
    }
    saveAbout.disabled = false;
  });
  const name = fragmentName();
  const read = `claude mcp add ${name} -- fragment mcp ${name}`;
  const write = `${read} --write`;
  const cmd = (label, text) => h("div.cmd", null, h("div.cmd-label", { text: label }), h("div.cmd-line", null, h("code", { text: text }), copyButton(text)));
  const st = S.status;
  const body = h(
    "div.settings",
    null,
    h("section", null, h("h3", { text: "About you" }), about, h("div.row", null, saved, h("span.grow"), saveAbout)),
    h(
      "section",
      null,
      h("h3", { text: "Your mind in other agents" }),
      h("p.quiet", { text: "Give Claude Code (or any MCP client) the same memory: search, zoom and the view. Needs the fragment CLI, signed in." }),
      cmd("Read only", read),
      cmd("Read and write notes", write),
    ),
    h(
      "section",
      null,
      h("h3", { text: "Memory" }),
      h("p.quiet", { text: st ? `${plural(st.T ?? 0, "message")}${st.unbuilt ? `, ${st.unbuilt} still being summarized` : ", all summarized"}. ${st.hands ? "Your computer is connected." : "No computer is connected."}` : "…" }),
    ),
    h("a.apps-link", { href: `${platformOrigin()}/?apps`, target: "_top" }, icon("grid"), h("span", null, h("b", { text: "Your apps" }), h("small", { text: "Everything else on fragment" })), icon("open")),
  );
  sheet("Settings", body, { cls: "wide" });
}
