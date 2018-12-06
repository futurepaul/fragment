// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
import * as fragment from "frag_native";
import app from "apprun";
import { note } from "./demo_note";

const state = {
  list: []
};
const view = state => (
  <div className="wrapper">
    <div className="title-bar" />
    <div className="search-box">
      <input
        type="text"
        oninput={e => app.run("update-query", e)}
        onkeypress={e => app.run("keypress", e)}
      />
    </div>
    <div className="list">
      {state.list.map((item, key) => (
        <div className="list-item" key={key}>
          {item}
        </div>
      ))}
    </div>
    <div className="note">
      <p>{note}</p>
    </div>
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
    return { list: response };
  }
};

app.start("app", state, view, update);
