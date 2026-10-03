// Small action-name tooltips for the chat's icon buttons (Skyler's, shared
// with the shell's design): shown after a short hover or on keyboard
// focus, above the button when it fits, never on touch. The button keeps
// its name for assistive technology (`aria-label`); its `title` moves into
// the tooltip so the browser shows no second one.
const selector = "button.icon-button, button.send";
const tooltip = document.createElement("div");
tooltip.id = "action-tooltip";
tooltip.className = "action-tooltip";
tooltip.setAttribute("role", "tooltip");
tooltip.hidden = true;
document.body.append(tooltip);
let active = null;
let timer = 0;

function hide() {
  clearTimeout(timer);
  tooltip.hidden = true;
  if (active) {
    const ids = (active.getAttribute("aria-describedby") || "").split(/\s+/).filter((id) => id && id !== tooltip.id);
    if (ids.length) active.setAttribute("aria-describedby", ids.join(" "));
    else active.removeAttribute("aria-describedby");
  }
  active = null;
}

function show(button) {
  if (active === button) return;
  hide();
  if (button.title) {
    button.dataset.actionTitle = button.title;
    if (!button.hasAttribute("aria-label")) button.setAttribute("aria-label", button.title);
    button.removeAttribute("title");
  }
  const label = button.getAttribute("aria-label") || button.dataset.actionTitle;
  if (!label) return;
  active = button;
  timer = setTimeout(() => {
    if (!button.isConnected) return hide();
    if (!tooltip.isConnected) document.body.append(tooltip);
    tooltip.textContent = label.replace(/…$/, "");
    tooltip.hidden = false;
    const rect = button.getBoundingClientRect();
    const size = tooltip.getBoundingClientRect();
    const left = Math.max(6, Math.min(innerWidth - size.width - 6, rect.left + (rect.width - size.width) / 2));
    // above when it fits; a control at the top edge gets it below
    const top = rect.top >= size.height + 8 ? rect.top - size.height - 6 : rect.bottom + 6;
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${top}px`;
    const ids = (button.getAttribute("aria-describedby") || "").split(/\s+/).filter(Boolean);
    button.setAttribute("aria-describedby", [...ids, tooltip.id].join(" "));
  }, 180);
}

document.addEventListener("pointerover", (event) => {
  if (event.pointerType === "touch") return;
  const button = event.target.closest(selector);
  if (button) show(button);
});
document.addEventListener("pointerout", (event) => {
  if (active && active.contains(event.target) && !active.contains(event.relatedTarget)) hide();
});
document.addEventListener("focusin", (event) => {
  const button = event.target.closest(selector);
  if (button) show(button);
});
document.addEventListener("focusout", hide);
document.addEventListener("pointerdown", hide, true);
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") hide();
});
addEventListener("blur", hide);
addEventListener("resize", hide);
document.addEventListener("scroll", hide, true);
