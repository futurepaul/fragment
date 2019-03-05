// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
import * as fragment from "frag_native";
import app from "apprun";

// const filelist = new fragment.FileList("../notes_grep_test");
// filelist.first();

const state = {
  // list items are objects with path, file_name, line, and line_num

  list: [],
  current_note: {
    path: "",
    content: ""
  },
  current_query: ""
};
const view = state => (
  <div className="wrapper">
    <div className="title-bar" />
    <div className="search-box">
      <input
        type="text"
        autofocus="true"
        oninput={e => app.run("update-query", e)}
        // onkeypress={e => app.run("keypress", e)}
      />
      <button onclick={_e => app.run("new-note", _e, state.current_query)}>
        New note
      </button>
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
            {item.line_num} - {item.first_line}
          </p>
        </div>
      ))}
    </div>
    <div className="note">{state.current_note.content}</div>
    <button onclick={_e => app.run("open-note", _e, state.current_note.path)}>
      Open note
    </button>
  </div>
);

const update = {
  // keypress: (_, e) => {
  //   e.keyCode === 13 && app.run("update-query", e);
  // },
  "update-query": (state, e, path) => {
    const input = e.target.value;
    let response = [];

    try {
      response = fragment.query(input) || [];
    } catch (e) {
      console.log(e);
    }

    return {
      list: response,
      current_note: { ...state.current_note },
      current_query: input
    };
  },

  "get-note": (state, e, path) => {
    let note = "";
    try {
      note = fragment.get_note(path);
    } catch (e) {
      console.log(e);
    }
    return { ...state, current_note: { path: path, content: note } };
  },

  "open-note": (state, e, path) => {
    let success = false;
    try {
      success = fragment.open_note(path);
    } catch (e) {
      console.log(e);
    }
  },

  "new-note": (state, e, query) => {
    let success = false;
    try {
      //we use the query string as the filename
      success = fragment.create_file(query);
    } catch (e) {
      console.log(e);
    }
  }
};

app.start("app", state, view, update);
