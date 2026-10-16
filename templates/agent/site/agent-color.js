// An agent's colour, one of Skyler's (a plain coloured circle), chosen by
// its identity: the same on its own page, in the shell and on its chats'
// pages. One file: the chat template's agent-color.js and the shell's are
// links to this one (crates/templates/build.rs), so no copy can drift.
const COLORS = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];

/** FNV-1a over the identity's UTF-16 units; an agent with none yet, the first. */
export function colorOf(id) {
  if (!id) return COLORS[0];
  let h = 0x811c9dc5;
  for (let i = 0; i < id.length; i++) h = Math.imul(h ^ id.charCodeAt(i), 0x01000193) >>> 0;
  return COLORS[h % COLORS.length];
}
