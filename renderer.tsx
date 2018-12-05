// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
const fragment = require("frag_native");

import app, { Component } from "apprun";
try {
  document.write(`<pre>${fragment.hello()}</pre>`);
} catch (e) {
  document.write(`<pre>${e.stack}</pre>`);
}

document.write(`<h1>test</h1>`);

const state = "search query";
const view = state => (
  <div>
    <h1>{state}</h1>
    <input type="text" onkeypress={e => app.run("keypress", e)} />
  </div>
);

const update = {
  keypress: (_, e) => {
    e.keyCode === 13 && app.run("update-query");
  },
  "update-query": state => {
    const input = document.querySelector("input");
    let response = "";
    try {
      response = fragment.query(input.value) || "";
      console.log(response);
    } catch (e) {
      console.log(e);
    }
    return "";
  }
};
app.start("my-app", state, view, update);
