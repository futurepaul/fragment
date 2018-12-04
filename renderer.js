// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
const fragment = require("frag_native");

try {
  document.write(`<pre>${fragment.hello()}</pre>`);
} catch (e) {
  document.write(`<pre>${e.stack}</pre>`);
}
