// The shell's icons, each one's shapes on a 24-unit grid (shell.css strokes
// them), and `icon`, which builds one through the DOM, never from markup.
// Lucide's (lucide.dev v1.7.0, from the dashboard's lucide-react package;
// ISC / MIT: vendor/lucide-LICENSE) are marked; the rest are the shell's.
// An icon is named, never handed over as markup: a name with no icon here
// is a mistake, and throws.
const ICON = {
  // Lucide's
  cornerDownLeft: [["path", { d: "M20 4v7a4 4 0 0 1-4 4H4" }], ["path", { d: "m9 10-5 5 5 5" }]],
  share: [["path", { d: "M12 2v13" }], ["path", { d: "m16 6-4-4-4 4" }], ["path", { d: "M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8" }]],
  reload: [["path", { d: "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" }], ["path", { d: "M21 3v5h-5" }]],
  max: [["path", { d: "M8 3H5a2 2 0 0 0-2 2v3" }], ["path", { d: "M21 8V5a2 2 0 0 0-2-2h-3" }], ["path", { d: "M3 16v3a2 2 0 0 0 2 2h3" }], ["path", { d: "M16 21h3a2 2 0 0 0 2-2v-3" }]],
  restore: [["path", { d: "M8 3v3a2 2 0 0 1-2 2H3" }], ["path", { d: "M21 8h-3a2 2 0 0 1-2-2V3" }], ["path", { d: "M3 16h3a2 2 0 0 1 2 2v3" }], ["path", { d: "M16 21v-3a2 2 0 0 1 2-2h3" }]],
  x: [["path", { d: "M18 6 6 18" }], ["path", { d: "m6 6 12 12" }]],
  folder: [["path", { d: "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" }]],
  // Lucide's external-link, link and square-pen
  external: [["path", { d: "M15 3h6v6" }], ["path", { d: "M10 14 21 3" }], ["path", { d: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" }]],
  link: [["path", { d: "M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" }], ["path", { d: "M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" }]],
  newChat: [["path", { d: "M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" }], ["path", { d: "M18.4 2.6a1 1 0 0 1 3 3l-9 9a2 2 0 0 1-.85.5l-2.87.84a.5.5 0 0 1-.62-.62l.84-2.87a2 2 0 0 1 .5-.85z" }]],
  // the shell's own
  plus: [["path", { d: "M12 5v14M5 12h14" }]],
  rename: [["path", { d: "M12 20h9" }], ["path", { d: "M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" }]],
  invite: [["circle", { cx: 9, cy: 8, r: 3.2 }], ["path", { d: "M3 19a6 6 0 0 1 12 0M19 8v6M16 11h6" }]],
  agent: [["circle", { cx: 12, cy: 8, r: 4 }], ["path", { d: "M4 21a8 8 0 0 1 16 0" }]],
  grid: [["rect", { x: 4, y: 4, width: 7, height: 7, rx: 1.5 }], ["rect", { x: 13, y: 4, width: 7, height: 7, rx: 1.5 }], ["rect", { x: 4, y: 13, width: 7, height: 7, rx: 1.5 }], ["rect", { x: 13, y: 13, width: 7, height: 7, rx: 1.5 }]],
  more: [["circle", { cx: 5, cy: 12, r: 1.3 }], ["circle", { cx: 12, cy: 12, r: 1.3 }], ["circle", { cx: 19, cy: 12, r: 1.3 }]],
  people: [["circle", { cx: 9, cy: 8, r: 3.2 }], ["path", { d: "M3 19a6 6 0 0 1 12 0" }], ["path", { d: "M16 5.2a3.2 3.2 0 0 1 0 5.6M18 19a6 6 0 0 0-2.5-4.9" }]],
  globe: [["circle", { cx: 12, cy: 12, r: 9 }], ["path", { d: "M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" }]],
  lock: [["rect", { x: 5, y: 11, width: 14, height: 10, rx: 2 }], ["path", { d: "M8 11V7a4 4 0 0 1 8 0v4" }]],
  screen: [["rect", { x: 3, y: 4, width: 18, height: 12, rx: 2 }], ["path", { d: "M8 20h8M12 16v4" }]],
  archive: [["rect", { x: 2, y: 3, width: 20, height: 5, rx: 1 }], ["path", { d: "M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8M10 12h4" }]],
};
const SVG_NS = "http://www.w3.org/2000/svg";

function draw(shapes, attrs) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  for (const [key, value] of Object.entries(attrs)) svg.setAttribute(key, value);
  for (const [tag, shapeAttrs] of shapes) {
    const shape = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(shapeAttrs)) shape.setAttribute(key, String(value));
    svg.append(shape);
  }
  return svg;
}

/// An icon by its name, hidden from assistive technology.
export function icon(name) {
  const shapes = ICON[name];
  if (!shapes) throw new Error(`shell: no icon ${name}`);
  return draw(shapes, { "aria-hidden": "true" });
}

// An app's icon in the desktop's dock and its window: one glyph for every
// app (the person's list says nothing more of what an app is), in a colour
// chosen by its label (FNV-1a, as an agent's colour is chosen by its
// identity), so the same app is the same colour wherever it shows.
const COLORS = ["#b18a30", "#5d7bc3", "#c9625e", "#4f9a7a", "#8f88bc", "#c4743d"];
const GLYPH = [["rect", { x: 5, y: 5, width: 14, height: 14, rx: 3 }], ["path", { d: "M5 10h14" }]];
export function appIcon(label) {
  let h = 0x811c9dc5;
  for (let i = 0; i < label.length; i++) h = Math.imul(h ^ label.charCodeAt(i), 0x01000193) >>> 0;
  const mark = document.createElement("span");
  mark.className = "app-icon";
  mark.setAttribute("aria-hidden", "true");
  mark.style.setProperty("--app-color", COLORS[h % COLORS.length]);
  mark.append(draw(GLYPH, { fill: "none" }));
  return mark;
}
