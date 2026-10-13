// An app's icon in the desktop's dock and its window: one glyph for every
// app (the person's list says nothing more of what an app is), in a colour
// chosen by its label (FNV-1a, as an agent's colour is chosen by its
// identity), so the same app is the same colour wherever it shows.
const COLORS = ["#b18a30", "#5d7bc3", "#c9625e", "#4f9a7a", "#8f88bc", "#c4743d"];
const GLYPH = '<rect x="5" y="5" width="14" height="14" rx="3"/><path d="M5 10h14"/>';
export function appIcon(label) {
  let h = 0x811c9dc5;
  for (let i = 0; i < label.length; i++) h = Math.imul(h ^ label.charCodeAt(i), 0x01000193) >>> 0;
  const icon = document.createElement('span');
  icon.className = 'app-icon';
  icon.setAttribute('aria-hidden', 'true');
  icon.style.setProperty('--app-color', COLORS[h % COLORS.length]);
  icon.innerHTML = `<svg viewBox="0 0 24 24" fill="none">${GLYPH}</svg>`;
  return icon;
}
