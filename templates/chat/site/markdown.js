// An agent's markdown, through the platform's renderer (./__markdown.js:
// DOM only, never markup, raw HTML shown as text, nesting bounded) with the
// chat's own choices: every newline breaks its line, as it was typed; a
// picture only of this chat's own files or blobs, opening in a tab; links
// to the web (http(s) and mailto) only; and a code block with its language
// and a Copy button.
import { inline as renderInline, markdown } from "./__markdown.js";
import { copy, feedback } from "./response-actions.js";

// `./__file?path=…` or `./__blob/<sha256>`: the chat's own bytes, never a
// picture from elsewhere on the web (a tracking pixel).
const OWN_PICTURE = /^(?:\.\/)?(?:__file\?path=[^\s]+|__blob\/[0-9a-f]{64})$/;

const OPTIONS = {
  breaks: true,
  image: (src, kind) => (kind === "link" && OWN_PICTURE.test(src) ? { src, href: src } : null),
  code: codeBlock,
};

export const renderMarkdown = (text) => markdown(text, OPTIONS);
export const inline = (parent, text) => renderInline(parent, text, OPTIONS);

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function codeBlock(text, language) {
  const wrap = el("div", "md-code");
  const header = el("div", "md-code-head");
  const button = el("button", "md-code-copy", "Copy");
  button.type = "button";
  button.setAttribute("aria-label", "Copy code");
  button.onclick = () => feedback(button, () => copy(text));
  header.append(el("span", "md-language", language || "Code"), button);
  const pre = el("pre");
  const code = el("code", null, text);
  if (language) code.dataset.language = language;
  pre.append(code);
  wrap.append(header, pre);
  return wrap;
}
