// The viewer: apps and files stacked vertically in the right column, newest on
// top. Each pane has a header (icon, title, status, actions, maximize,
// close; drag it to reorder) and resizes against its neighbours
// through split-grid row gutters. Order is only grid-row placement: a pane's
// element never moves in the DOM once added, so app iframes keep their state
// through reordering, resizing, collapsing the viewer, maximizing, and other
// panes closing.
import Split from "./vendor/split-grid.js";
import { store, reducedMotion, springFrames, frameWriter } from "./layout.js";
import { icon } from "./icons.js";

const KEY = "finite.viewer.v1";
const MIN_ROW = 96;

// An action of a pane's header: `glyph` names one of the shell's icons.
function button(glyph, title, onClick) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "icon-button pane-action";
  b.title = title;
  b.setAttribute("aria-label", title);
  b.replaceChildren(icon(glyph));
  b.onclick = (e) => { e.stopPropagation(); onClick(e); };
  return b;
}

// App iframe events do not bubble into the desktop. Listen inside each local
// frame too, translating its pointer coordinates to the enclosing window.
function windowGlow(pane) {
  const hover = matchMedia("(hover: hover) and (pointer: fine)");
  const events = new AbortController();
  const frameEvents = new Map();
  let pending = 0;
  let pointer;
  const clear = () => {
    cancelAnimationFrame(pending);
    pending = 0;
    pane.style.setProperty("--window-light", "0");
  };
  const move = (event, frame) => {
    if (!hover.matches || event.pointerType === "touch") return;
    const offset = frame?.getBoundingClientRect();
    pointer = { x: event.clientX + (offset?.left || 0), y: event.clientY + (offset?.top || 0) };
    if (pending) return;
    pending = requestAnimationFrame(() => {
      pending = 0;
      const rect = pane.getBoundingClientRect();
      const x = pointer.x - rect.left, y = pointer.y - rect.top;
      const distance = Math.hypot(Math.max(0, -x, x - rect.width), Math.max(0, -y, y - rect.height));
      const proximity = Math.max(0, 1 - distance / 64);
      pane.style.setProperty("--window-x", `${x}px`);
      pane.style.setProperty("--window-y", `${y}px`);
      pane.style.setProperty("--window-radius", `${Math.max(180, Math.min(rect.width, rect.height))}px`);
      pane.style.setProperty("--window-light", String(proximity * proximity));
    });
  };
  pane.addEventListener("pointerleave", clear, { signal: events.signal });
  document.addEventListener("pointermove", event => move(event), { signal: events.signal });
  document.documentElement.addEventListener("pointerleave", clear, { signal: events.signal });
  addEventListener("blur", () => { if (!document.hasFocus()) clear(); }, { signal: events.signal });
  for (const frame of pane.querySelectorAll("iframe")) {
    const bind = () => {
      frameEvents.get(frame)?.abort();
      try {
        const doc = frame.contentDocument;
        if (!doc) return;
        const controller = new AbortController();
        frameEvents.set(frame, controller);
        doc.addEventListener("pointermove", event => move(event, frame), { signal: controller.signal });
        doc.documentElement.addEventListener("pointerleave", clear, { signal: controller.signal });
      } catch { /* Cross-origin apps retain the toolbar glow. */ }
    };
    frame.addEventListener("load", bind, { signal: events.signal });
    bind();
  }
  return () => { clear(); events.abort(); for (const controller of frameEvents.values()) controller.abort(); };
}

export function createViewer({ stack, onChange = () => {} }) {
  const panes = []; // { key, el, gutter, size (fr), persist }
  let split = null;
  let solo = null; // key of the maximized pane
  let equalize = false;
  const restored = new Map(store.get(KEY, []).map((p) => [p.key, p.size])); // sizes from last time, used once
  const dragStyle = frameWriter();
  const bounds = () => new Map(panes.filter(p => !p.el.hidden && !p.closing).map(p => [p.key, p.el.getBoundingClientRect()]));
  const cancelMotion = (p) => { p.motion?.cancel(); p.motion = null; };
  function animatePane(p, frames, options) {
    cancelMotion(p);
    if (reducedMotion.matches || !p.el.getBoundingClientRect().height) return null;
    const motion = p.el.animate(frames, options);
    p.motion = motion;
    motion.onfinish = () => { if (p.motion === motion) { p.motion = null; motion.cancel(); } };
    return motion;
  }
  function settle(previous) {
    for (const p of panes) {
      if (p.el.hidden || p.closing) continue;
      const before = previous.get(p.key), after = p.el.getBoundingClientRect();
      if (!after.width || !after.height) continue;
      if (!before?.width || !before.height) {
        animatePane(p, springFrames(t => ({ opacity: Math.min(1, t), transform: `scale(${.97 + .03 * t})` })), { duration: 280, easing: "linear" });
      } else {
        const x = before.left + before.width / 2 - after.left - after.width / 2;
        const y = before.top + before.height / 2 - after.top - after.height / 2;
        const sx = before.width / after.width, sy = before.height / after.height;
        if (Math.abs(x) + Math.abs(y) + Math.abs(before.height - after.height) + Math.abs(before.width - after.width) < 1) continue;
        animatePane(p, springFrames(t => ({ transform: `translate(${x * (1 - t)}px, ${y * (1 - t)}px) scale(${1 + (sx - 1) * (1 - t)}, ${1 + (sy - 1) * (1 - t)})` })), { duration: 280, easing: "linear" });
      }
    }
  }
  reducedMotion.addEventListener("change", () => {
    if (!reducedMotion.matches) return;
    for (const p of [...panes]) { if (p.closing) finishClose(p); else cancelMotion(p); }
  });

  function relayout(previous = bounds()) {
    dragStyle.flush();
    for (const p of panes) if (!p.closing) cancelMotion(p);
    split?.destroy();
    split = null;
    const tracks = [];
    const gutters = [];
    panes.forEach((p, i) => {
      const shown = !solo || p.key === solo;
      p.gutter.hidden = i === 0 || !!solo;
      if (i > 0) {
        tracks.push(solo ? "0px" : "8px");
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
        writeStyle: dragStyle.write,
        onDragStart: () => {
          for (const p of panes) if (!p.closing) cancelMotion(p);
          document.body.classList.add("resizing", "resizing-row");
        },
        onDragEnd: () => {
          dragStyle.flush();
          document.body.classList.remove("resizing", "resizing-row");
          const sizes = stack.style.gridTemplateRows.split(" ").filter((_, i) => i % 2 === 0).map(parseFloat);
          panes.forEach((p, i) => { if (sizes[i] > 0) p.size = sizes[i]; });
          save();
          if (equalize) { equalize = false; for (const p of panes) p.size = 1; relayout(); }
        },
      });
    }
    save();
    onChange(panes.map((p) => p.key));
    settle(previous);
  }

  function save() {
    store.set(KEY, panes.filter((p) => p.persist).map((p) => ({ key: p.key, size: p.size })));
  }

  function flash(p) {
    p.el.classList.remove("flash");
    void p.el.offsetWidth;
    p.el.classList.add("flash");
  }

  // open({ key, title, subtitle?, icon?: Element, status?: Element, body: Element, actions?: [{ icon, title, onClick }], persist? })
  // `status` follows the title.
  function open(spec) {
    const existing = panes.find((p) => p.key === spec.key);
    if (existing) {
      if (existing.closing) { existing.closing = false; existing.el.inert = false; cancelMotion(existing); }
      focus(spec.key); return existing;
    }
    const previous = bounds();
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
    if (spec.status) head.append(spec.status);
    const grow = document.createElement("span");
    grow.className = "grow";
    head.append(grow);
    for (const a of spec.actions || []) head.append(button(a.icon, a.title, a.onClick));
    const max = button("max", "Maximize", () => maximize(spec.key));
    max.classList.add("pane-max");
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
    gutter.addEventListener("mousedown", (e) => { if (e.detail === 2) equalize = true; }); // see layout.js on dblclick
    const avg = panes.length ? panes.reduce((n, p) => n + p.size, 0) / panes.length : 1;
    const size = restored.get(spec.key) || avg;
    restored.delete(spec.key);
    const pane = { key: spec.key, el, gutter, max, size, persist: spec.persist !== false, disposeGlow: windowGlow(el) };
    head.addEventListener("pointerdown", (e) => reorder(pane, e));
    setSolo(null);
    panes.unshift(pane);
    stack.append(gutter, el);
    relayout(previous);
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
    const p = panes.find(p => p.key === key);
    if (!p || p.closing) return;
    p.closing = true;
    p.el.inert = true;
    const style = getComputedStyle(p.el);
    const motion = animatePane(p, [
      { opacity: style.opacity, transform: style.transform },
      { opacity: 0, transform: "scale(.97)" },
    ], { duration: 160, easing: "cubic-bezier(.4, 0, 1, 1)", fill: "forwards" });
    if (motion) motion.onfinish = () => finishClose(p);
    else finishClose(p);
  }

  function finishClose(p) {
    const i = panes.indexOf(p);
    if (i < 0 || !p.closing) return;
    const previous = bounds();
    cancelMotion(p);
    panes.splice(i, 1);
    p.disposeGlow();
    p.el.remove();
    p.gutter.remove();
    if (solo === p.key) setSolo(null);
    relayout(previous);
  }

  function setSolo(key) {
    solo = key;
    for (const p of panes) {
      const on = p.key === solo;
      p.max.replaceChildren(icon(on ? "restore" : "max"));
      p.max.title = on ? "Restore" : "Maximize";
      p.max.setAttribute("aria-label", p.max.title);
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
    open, close,
    get keys() { return panes.map((p) => p.key); },
  };
}
