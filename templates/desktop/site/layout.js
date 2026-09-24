// The desktop frame: sidebar | chat | viewer as one CSS grid. split-grid drags
// the two column gutters; the side columns keep a pixel width and the chat
// takes the rest. Either side collapses to nothing and comes back at the width
// it had. Below 760 px the sides become overlays instead of columns.
import Split from "./vendor/split-grid.js";

const KEY = "finite.layout.v1";
const MIN = { left: 200, chat: 380, right: 300 };
const MAX_LEFT = 420;
const DEFAULTS = { left: 264, right: 480, leftOpen: true, rightOpen: false };

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

  // Fit the saved widths into the window without persisting the squeeze.
  function fitted() {
    let left = s.leftOpen ? s.left : 0;
    let right = s.rightOpen ? s.right : 0;
    const room = grid.clientWidth - MIN.chat - (s.leftOpen ? 1 : 0) - (s.rightOpen ? 1 : 0);
    if (left + right > room && right) right = Math.max(MIN.right, room - left);
    if (left + right > room && left) left = Math.max(MIN.left, room - right);
    return { left, right, hideLeft: s.leftOpen && left + right > room };
  }

  function write() {
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
  }

  function toggle(side, open) {
    if (narrow.matches) {
      const want = open ?? overlay !== side;
      overlay = want ? side : overlay === side ? null : overlay;
    } else {
      s[`${side}Open`] = open ?? !s[`${side}Open`];
      store.set(KEY, s);
    }
    write();
  }

  Split({
    columnGutters: [{ track: 1, element: gutterLeft }, { track: 3, element: gutterRight }],
    columnMinSizes: { 0: MIN.left, 2: MIN.chat, 4: MIN.right },
    snapOffset: 0,
    writeStyle(el, prop, style) {
      const tracks = style.split(" ");
      if (parseFloat(tracks[0]) > MAX_LEFT) tracks[0] = `${MAX_LEFT}px`;
      el.style[prop] = tracks.join(" ");
    },
    onDragStart: () => document.body.classList.add("resizing", "resizing-col"),
    onDragEnd: (_, track) => {
      document.body.classList.remove("resizing", "resizing-col");
      // Keep only the dragged side: the other may be squeezed to fit the window.
      const px = parseFloat(grid.style.gridTemplateColumns.split(" ")[track === 1 ? 0 : 4]);
      if (px) s[track === 1 ? "left" : "right"] = Math.round(px);
      store.set(KEY, s);
    },
  });
  // Double-click resets a width. split-grid turns off pointer events on the
  // grid while a press is held, so dblclick never fires; count clicks instead.
  const onDouble = (gutter, fn) => gutter.addEventListener("mousedown", (e) => { if (e.detail === 2) fn(); });
  onDouble(gutterLeft, () => { s.left = DEFAULTS.left; store.set(KEY, s); write(); });
  onDouble(gutterRight, () => { s.right = DEFAULTS.right; store.set(KEY, s); write(); });

  let frame = 0;
  addEventListener("resize", () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(write); });
  narrow.addEventListener("change", () => { overlay = null; write(); });
  write();

  return {
    toggle,
    show: (side) => toggle(side, true),
    hide: (side) => toggle(side, false),
    isOpen: (side) => grid.classList.contains(`${side}-open`),
    get narrow() { return narrow.matches; },
  };
}
