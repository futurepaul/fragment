// The extension's popup, its settings: whether this browser is connected
// for its owner's agents (or coming back on its own, with its link), what
// they may use of it (only the tabs they open, those and the tabs the
// person picks, or every tab and window: each said beside its choice),
// and the way to disconnect it, which forgets it. The worker keeps the
// choice (access.js).

const said = document.getElementById("said");
const button = document.getElementById("disconnect");
const error = document.getElementById("error");
const all = document.getElementById("all");
const list = document.getElementById("tabs");

// Only the answer to the newest ask is shown: an older one (the first
// view, answered after a click) would show what was chosen before.
let asked = 0;
function ask(m) {
  const n = ++asked;
  return chrome.runtime.sendMessage(m).then((v) => n === asked && show(v));
}

function show(v) {
  error.textContent = v.error ?? "";
  if (v.error) return;
  const s = v.status;
  said.textContent = s.connected
    ? "Connected. Your agents can use this browser."
    : s.link
      ? "Reconnecting…"
      : s.error
        ? `Not connected: ${s.error}.`
        : "Not connected. Connect it from Settings in Fragment.";
  // Disconnect forgets this browser: it does not come back on its own
  button.hidden = !s.connected && !s.link;
  for (const r of document.querySelectorAll("input[name=access]")) r.checked = r.value === v.access;
  all.className = v.access === "all" ? "hint warning" : "hint";
  list.replaceChildren();
  if (v.access !== "chosen") return;
  for (const t of v.tabs.filter((t) => !t.own)) {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = t.chosen;
    box.dataset.title = t.title;
    box.onchange = () => ask({ type: "choose", tabId: t.id, on: box.checked });
    const row = document.createElement("label");
    row.title = t.url;
    row.append(box, ` ${t.title || t.url}`);
    list.append(row);
  }
}

for (const r of document.querySelectorAll("input[name=access]")) {
  r.onchange = () => ask({ type: "choose", access: r.value });
}
button.onclick = () => chrome.runtime.sendMessage({ type: "disconnect" }).then(() => ask({ type: "view" }));
ask({ type: "view" });
