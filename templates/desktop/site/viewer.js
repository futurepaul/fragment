// The viewer: apps and files stacked vertically in the right column, newest on
// top. Each pane has a header (icon, title, actions, maximize, close; drag it
// to reorder) and resizes against its neighbours through split-grid row
// gutters. Order is only grid-row placement: a pane's element never moves in
// the DOM once added, so app iframes keep their state through reordering,
// resizing, collapsing the viewer, maximizing, and other panes closing.
import Split from "./vendor/split-grid.js";
import { store } from "./layout.js";

const KEY = "finite.viewer.v1";
const MIN_ROW = 96;
const ICON = {
  max: '<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>',
  restore: '<path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name] || name}</svg>`;

function button(icon, title, onClick) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "icon-button pane-action";
  b.title = title;
  b.setAttribute("aria-label", title);
  b.innerHTML = svg(icon);
  b.onclick = (e) => { e.stopPropagation(); onClick(e); };
  return b;
}

export function createViewer({ stack, onChange = () => {} }) {
  const panes = []; // { key, el, gutter, size (fr), persist }
  let split = null;
  let solo = null; // key of the maximized pane
  const restored = new Map(store.get(KEY, []).map((p) => [p.key, p.size])); // sizes from last time, used once

  function relayout() {
    split?.destroy();
    split = null;
    const tracks = [];
    const gutters = [];
    panes.forEach((p, i) => {
      const shown = !solo || p.key === solo;
      p.gutter.hidden = i === 0 || !!solo;
      if (i > 0) {
        tracks.push(solo ? "0px" : "1px");
        p.gutter.style.gridRow = String(tracks.length);
        if (!solo) gutters.push({ track: tracks.length - 1, element: p.gutter });
      }
      tracks.push(solo ? (shown ? "1fr" : "0px") : `${p.size}fr`);
      p.el.style.gridRow = String(tracks.length);
      p.el.hidden = !shown;
      p.el.classList.toggle("solo", p.key === solo);
    });
    stack.style.gridTemplateRows = tracks.join(" ");
    if (gutters.length) {
      split = Split({
        rowGutters: gutters,
        rowMinSize: MIN_ROW,
        snapOffset: 0,
        onDragStart: () => document.body.classList.add("resizing", "resizing-row"),
        onDragEnd: () => {
          document.body.classList.remove("resizing", "resizing-row");
          const sizes = stack.style.gridTemplateRows.split(" ").filter((_, i) => i % 2 === 0).map(parseFloat);
          panes.forEach((p, i) => { if (sizes[i] > 0) p.size = sizes[i]; });
          save();
        },
      });
    }
    save();
    onChange(panes.map((p) => p.key));
  }

  function save() {
    store.set(KEY, panes.filter((p) => p.persist).map((p) => ({ key: p.key, size: p.size })));
  }

  function flash(p) {
    p.el.classList.remove("flash");
    void p.el.offsetWidth;
    p.el.classList.add("flash");
  }

  // open({ key, title, subtitle?, icon?: Element, body: Element, actions?: [{ icon, title, onClick }], size?, persist? })
  function open(spec) {
    const existing = panes.find((p) => p.key === spec.key);
    if (existing) { focus(spec.key); return existing; }
    const el = document.createElement("article");
    el.className = "pane";
    el.dataset.key = spec.key;
    const head = document.createElement("header");
    head.className = "pane-head";
    head.title = "Drag to reorder · double-click to maximize";
    if (spec.icon) head.append(spec.icon);
    const title = document.createElement("span");
    title.className = "pane-title";
    title.textContent = spec.title;
    head.append(title);
    if (spec.subtitle) {
      const sub = document.createElement("span");
      sub.className = "pane-sub";
      sub.textContent = spec.subtitle;
      head.append(sub);
    }
    const grow = document.createElement("span");
    grow.className = "grow";
    head.append(grow);
    for (const a of spec.actions || []) head.append(button(a.icon, a.title, a.onClick));
    const max = button("max", "Maximize", () => maximize(spec.key));
    head.append(max, button("x", "Close", () => close(spec.key)));
    head.ondblclick = (e) => { if (!e.target.closest("button")) maximize(spec.key); };
    const body = document.createElement("div");
    body.className = "pane-body";
    body.append(spec.body);
    el.append(head, body);
    const gutter = document.createElement("div");
    gutter.className = "gutter gutter-row";
    gutter.dataset.above = spec.key;
    gutter.setAttribute("role", "separator");
    gutter.setAttribute("aria-orientation", "horizontal");
    gutter.title = "Drag to resize · double-click to share evenly";
    gutter.addEventListener("mousedown", (e) => { if (e.detail === 2) { for (const p of panes) p.size = 1; relayout(); } }); // see layout.js on dblclick
    const avg = panes.length ? panes.reduce((n, p) => n + p.size, 0) / panes.length : 1;
    const size = spec.size || restored.get(spec.key) || avg;
    restored.delete(spec.key);
    const pane = { key: spec.key, el, gutter, max, size, persist: spec.persist !== false };
    head.addEventListener("pointerdown", (e) => reorder(pane, e));
    setSolo(null);
    panes.unshift(pane);
    stack.append(gutter, el);
    relayout();
    flash(pane);
    return pane;
  }

  // Drag a header to move its pane. A line shows where it will land; Escape
  // cancels. Iframes get no pointer events meanwhile (body.reordering).
  function reorder(pane, e) {
    const head = e.currentTarget;
    if (e.button !== 0 || e.target.closest("button") || solo || panes.length < 2) return;
    const startY = e.clientY;
    let moving = false;
    let slot = 0;
    const line = document.createElement("div");
    line.className = "drop-line";
    head.setPointerCapture(e.pointerId);
    const move = (ev) => {
      if (!moving && Math.abs(ev.clientY - startY) < 5) return;
      if (!moving) {
        moving = true;
        document.body.classList.add("reordering");
        pane.el.classList.add("dragging");
        stack.append(line);
      }
      // Land before the first pane whose middle is below the pointer.
      const rects = panes.map((p) => p.el.getBoundingClientRect());
      slot = rects.findIndex((r) => ev.clientY < r.top + r.height / 2);
      if (slot < 0) slot = panes.length;
      const y = slot < panes.length ? rects[slot].top : rects[panes.length - 1].bottom;
      const box = stack.getBoundingClientRect();
      line.style.top = `${Math.max(0, Math.min(box.height - 3, y - box.top - 1))}px`;
    };
    const end = (ev) => {
      head.removeEventListener("pointermove", move);
      head.removeEventListener("pointerup", end);
      head.removeEventListener("pointercancel", end);
      removeEventListener("keydown", esc, true);
      if (head.hasPointerCapture(e.pointerId)) head.releasePointerCapture(e.pointerId);
      if (!moving) return;
      document.body.classList.remove("reordering");
      pane.el.classList.remove("dragging");
      line.remove();
      if (ev.type !== "pointerup") return;
      const from = panes.indexOf(pane);
      const to = slot > from ? slot - 1 : slot; // taking it out shifts later slots up
      if (to === from) return;
      panes.splice(from, 1);
      panes.splice(to, 0, pane);
      relayout();
    };
    const esc = (ev) => { if (ev.key === "Escape") { ev.preventDefault(); end({ type: "cancel" }); } };
    head.addEventListener("pointermove", move);
    head.addEventListener("pointerup", end);
    head.addEventListener("pointercancel", end);
    addEventListener("keydown", esc, true);
  }

  function close(key) {
    const i = panes.findIndex((p) => p.key === key);
    if (i < 0) return;
    const [p] = panes.splice(i, 1);
    p.el.remove();
    p.gutter.remove();
    if (solo === key) solo = null;
    relayout();
  }

  function setSolo(key) {
    solo = key;
    for (const p of panes) {
      const on = p.key === solo;
      p.max.innerHTML = svg(on ? "restore" : "max");
      p.max.title = on ? "Restore" : "Maximize";
    }
  }

  function maximize(key) {
    setSolo(solo === key ? null : key);
    relayout();
  }

  function focus(key) {
    const p = panes.find((x) => x.key === key);
    if (!p) return;
    if (solo && solo !== key) maximize(solo);
    p.el.scrollIntoView({ block: "nearest" });
    flash(p);
  }

  return {
    open, close, focus, maximize,
    has: (key) => panes.some((p) => p.key === key),
    get keys() { return panes.map((p) => p.key); },
    saved: () => store.get(KEY, []),
  };
}
