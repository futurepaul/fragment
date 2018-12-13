// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
import * as fragment from "frag_native";
import app from "apprun";
import { note } from "./demo_note";

const state = {
  // list items are objects with path, file_name, line, and line_num
  list: [],
  current_note: {
    path: "",
    content: note
  }
};
const view = state => (
  <div className="wrapper">
    <div className="title-bar" />
    <div className="search-box">
      <input
        type="text"
        autofocus="true"
        oninput={e => app.run("update-query", e)}
        onkeypress={e => app.run("keypress", e)}
      />
    </div>
    <div className="list">
      {state.list.map((item, key) => (
        <div
          className="list-item"
          onclick={_e => app.run("get-note", _e, item.path)}
          key={key}
        >
          <strong>{item.file_name}</strong>
          <p>
            {item.line_num} - {item.line}
          </p>
        </div>
      ))}
    </div>
    <div className="note">{state.current_note.content}</div>
  </div>
);

function query_async(query: string): Promise<Array<string>> {
  return new Promise<Array<string>>(resolve => {
    resolve(fragment.query(query));
  });
}

const update = {
  keypress: (_, e) => {
    e.keyCode === 13 && app.run("update-query", e);
  },
  "update-query": (state, e, path) => {
    const input = e.target.value;
    let response = [];
    // query_async(input)
    //   .then(result => (response = result))
    //   .catch(() => {
    //     console.log("async went wrong");
    //   });
    try {
      response = fragment.query(input) || [];
    } catch (e) {
      console.log(e);
    }
    return { list: response, current_note: { ...state.current_note } };
  },
  "update-query-async": (state, e, path) => {
    const input = e.target.value;

    let response = fragment.query_async((err, value) => {
      if (err) throw err;
      return value;
    });

    return { list: response, current_note: { ...state.current_note } };
  },
  "get-note": (state, e, path) => {
    let note = "";
    try {
      note = fragment.get_note(path);
    } catch (e) {
      console.log(e);
    }
    return { ...state, current_note: { path: path, content: note } };
  }
};

app.start("app", state, view, update);
