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

const state = {
  list: ["result1", "result2", "result3"]
};
const view = state => (
  <div>
    <h1>type something</h1>
    <input
      type="text"
      oninput={_e => app.run("update-query", _e)}
      onkeypress={e => app.run("keypress", e)}
    />
    <ul>
      {state.list.map((item, key) => (
        <li key={key}>{item}</li>
      ))}
    </ul>
  </div>
);

const update = {
  keypress: (_, e) => {
    e.keyCode === 13 && app.run("update-query", e);
  },
  "update-query": (state, e) => {
    const input = e.target.value;
    let response = [];
    try {
      response = fragment.query(input) || [];
    } catch (e) {
      console.log(e);
    }
    console.log(response);
    return { list: response };
  }
};
};
app.start("my-app", state, view, update);
