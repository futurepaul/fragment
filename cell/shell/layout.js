// The desktop frame: sidebar | chat | viewer as one CSS grid. split-grid drags
// the two column gutters; the side columns keep a pixel width and the chat
// takes the rest. Either side collapses to nothing and comes back at the width
// it had. Below 760 px the sides become overlays instead of columns.
import Split from "./vendor/split-grid.js";

const KEY = "finite.layout.v2";
const MIN = { left: 200, chat: 380, right: 300 };
const MAX_LEFT = 420;
const COLLAPSE_LEFT = 120;
const DEFAULTS = { left: 264, right: 480, leftOpen: true, rightOpen: true };

export const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
// A damped spring with one small overshoot, sampled for native animations.
export function springFrames(frame) {
  return Array.from({ length: 31 }, (_, i) => {
    const t = i / 30;
    const progress = i === 30 ? 1 : 1 - Math.exp(-8 * t) * (Math.cos(6 * t) + 4 / 3 * Math.sin(6 * t));
    return { ...frame(progress), offset: t };
  });
}

// Coalesce high-frequency drag events without putting easing behind the pointer.
export function frameWriter() {
  let frame = 0, pending;
  const flush = () => {
    cancelAnimationFrame(frame);
    frame = 0;
    if (pending) { const [el, prop, value] = pending; el.style[prop] = value; pending = null; }
  };
  return { flush, write(el, prop, value) { pending = [el, prop, value]; if (!frame) frame = requestAnimationFrame(flush); } };
}

// The sandboxed desktop has an opaque origin, where storage throws. Layout is
// a convenience, so it silently does not persist there.
export const store = {
  get(key, fallback) { try { return JSON.parse(localStorage.getItem(key)) ?? fallback; } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem(key, JSON.stringify(value)); } catch {} },
};

export function createLayout({ grid, gutters: [gutterLeft, gutterRight], onChange = () => {} }) {
  const s = { ...DEFAULTS, ...store.get(KEY, {}) };
  const narrow = matchMedia("(max-width: 760px)");
  let overlay = null; // "left" | "right" while narrow
  let motion = null;
  let sheetMotions = [];
  let resetRight = false;
  const dragStyle = frameWriter();
  function stopMotion() {
    motion?.cancel();
    motion = null;
    for (const animation of sheetMotions) animation.cancel();
    sheetMotions = [];
    grid.classList.remove("moving-left", "moving-right");
  }
  // Stop at the visible width before split-grid measures a new gesture.
  for (const gutter of [gutterLeft, gutterRight]) {
    const interrupt = () => {
      if (!motion) return;
      const current = getComputedStyle(grid).gridTemplateColumns;
      const widths = current.split(" ").map(parseFloat);
      stopMotion();
      if (widths[0] > 1) { s.leftOpen = true; grid.classList.add("left-open"); }
      if (widths[4] > 1) { s.rightOpen = true; grid.classList.add("right-open"); }
      grid.style.gridTemplateColumns = current;
    };
    gutter.addEventListener("mousedown", interrupt, true);
    gutter.addEventListener("touchstart", interrupt, { capture: true, passive: true });
  }

  // Fit the saved widths into the window without persisting the squeeze.
  function fitted() {
    let left = s.leftOpen ? s.left : 0;
    let right = s.rightOpen ? s.right : 0;
    const room = grid.clientWidth - MIN.chat - (s.leftOpen ? 1 : 0) - (s.rightOpen ? 1 : 0);
    if (left + right > room && right) right = Math.max(MIN.right, room - left);
    if (left + right > room && left) left = Math.max(MIN.left, room - right);
    return { left, right, hideLeft: s.leftOpen && left + right > room };
  }

  function write(animate = false) {
    const before = getComputedStyle(grid).gridTemplateColumns;
    const sheets = [grid.querySelector(".sidebar-shell"), grid.querySelector(".viewer")];
    const sheetStyles = sheets.map(el => ({ transform: getComputedStyle(el).transform, opacity: getComputedStyle(el).opacity }));
    const wasNarrow = grid.classList.contains("narrow");
    const wasLeft = grid.classList.contains("left-open") || grid.classList.contains("moving-left");
    const wasRight = grid.classList.contains("right-open") || grid.classList.contains("moving-right");
    stopMotion();
    const wide = !narrow.matches;
    const f = fitted();
    const leftShown = wide && s.leftOpen && !f.hideLeft;
    const rightShown = wide && s.rightOpen;
    grid.style.gridTemplateColumns = [
      leftShown ? `${f.left}px` : "0px", leftShown ? "1px" : "0px", "1fr",
      rightShown ? "1px" : "0px", rightShown ? `${f.right}px` : "0px",
    ].join(" ");
    const shown = { left: wide ? leftShown : overlay === "left", right: wide ? rightShown : overlay === "right" };
    grid.classList.toggle("narrow", !wide);
    grid.classList.toggle("left-open", shown.left);
    grid.classList.toggle("right-open", shown.right);
    onChange(shown);
    if (animate && wide && !reducedMotion.matches && grid.clientWidth) {
      const after = getComputedStyle(grid).gridTemplateColumns;
      if (before === after) return;
      const from = before.split(" ").map(parseFloat), to = after.split(" ").map(parseFloat);
      if (from.length !== 5 || to.length !== 5 || !from.every(Number.isFinite) || !to.every(Number.isFinite)) return;
      grid.classList.toggle("moving-left", wasLeft || shown.left);
      grid.classList.toggle("moving-right", wasRight || shown.right);
      motion = grid.animate(springFrames(p => ({ gridTemplateColumns: to.map((n, i) => `${Math.max(0, from[i] + (n - from[i]) * p)}px`).join(" ") })), { duration: 280, easing: "linear" });
      motion.onfinish = stopMotion;
    } else if (animate && !wide && wasNarrow && !reducedMotion.matches) {
      sheets.forEach((el, i) => {
        const side = i ? "right" : "left", was = i ? wasRight : wasLeft;
        if (was === shown[side]) return;
        grid.classList.add(`moving-${side}`);
        const off = i ? "translateY(32px)" : "translateX(-24px)";
        const from = was ? sheetStyles[i] : { transform: off, opacity: 0 };
        sheetMotions.push(el.animate([from, { transform: shown[side] ? "none" : off, opacity: shown[side] ? 1 : 0 }], { duration: 240, easing: "cubic-bezier(.22, 1, .36, 1)", fill: "both" }));
      });
      if (sheetMotions.length) {
        const current = sheetMotions;
        Promise.all(current.map(a => a.finished)).then(() => { if (sheetMotions === current) stopMotion(); }).catch(() => {});
      }
    }
  }

  function toggle(side, open) {
    if (narrow.matches) {
      const want = open ?? overlay !== side;
      overlay = want ? side : overlay === side ? null : overlay;
    } else {
      s[`${side}Open`] = open ?? !s[`${side}Open`];
      store.set(KEY, s);
    }
    write(true);
  }

  // split-grid suppresses click events during a press. Track mouse movement
  // separately so releasing a stationary press collapses instead of resizing.
  let sidebarPress = null;
  gutterLeft.addEventListener("mousedown", (event) => {
    if (event.button === 0) sidebarPress = { x: event.clientX, y: event.clientY, moved: false };
  }, true);
  addEventListener("mousemove", (event) => {
    if (sidebarPress && Math.hypot(event.clientX - sidebarPress.x, event.clientY - sidebarPress.y) > 4) {
      sidebarPress.moved = true;
    }
  });

  Split({
    columnGutters: [{ track: 1, element: gutterLeft }, { track: 3, element: gutterRight }],
    columnMinSizes: { 0: 0, 2: MIN.chat, 4: MIN.right },
    snapOffset: 0,
    writeStyle(el, prop, style) {
      const tracks = style.split(" ");
      if (parseFloat(tracks[0]) > MAX_LEFT) tracks[0] = `${MAX_LEFT}px`;
      dragStyle.write(el, prop, tracks.join(" "));
    },
    onDragStart: () => document.body.classList.add("resizing", "resizing-col"),
    onDragEnd: (_, track) => {
      dragStyle.flush();
      document.body.classList.remove("resizing", "resizing-col");
      // Keep only the dragged side: the other may be squeezed to fit the window.
      const px = parseFloat(grid.style.gridTemplateColumns.split(" ")[track === 1 ? 0 : 4]);
      if (track === 1) {
        // Keep the previous usable width so reopening restores the sidebar.
        if ((sidebarPress && !sidebarPress.moved) || px < COLLAPSE_LEFT) s.leftOpen = false;
        else s.left = Math.max(MIN.left, Math.round(px));
      } else if (resetRight) { s.right = DEFAULTS.right; resetRight = false; }
      else if (px) s.right = Math.round(px);
      sidebarPress = null;
      store.set(KEY, s);
      write(true);
    },
  });
  // Double-click resets the viewer width. split-grid turns off pointer events on the
  // grid while a press is held, so dblclick never fires; count clicks instead.
  const onDouble = (gutter, fn) => gutter.addEventListener("mousedown", (e) => { if (e.detail === 2) fn(); });
  onDouble(gutterRight, () => { resetRight = true; });

  // The grid's own size, not the window's: it is hidden until the person is
  // signed in, and a fit made while it measured nothing hid the sidebar.
  let frame = 0;
  new ResizeObserver(() => { cancelAnimationFrame(frame); frame = requestAnimationFrame(() => write()); }).observe(grid);
  narrow.addEventListener("change", () => { overlay = null; write(); });
  reducedMotion.addEventListener("change", () => { if (reducedMotion.matches) stopMotion(); });
  write();

  return {
    toggle,
    show: (side) => toggle(side, true),
    hide: (side) => toggle(side, false),
    get narrow() { return narrow.matches; },
  };
}
