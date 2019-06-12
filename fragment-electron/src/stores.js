import { writable } from "svelte/store";
const fragment = require("fragment-backend");
const settings = require("electron-settings");
const { dialog } = require("electron").remote;

let user_provided_path = "";

if (settings.has("notes_path.path")) {
  user_provided_path = settings.get("notes_path.path");
} else {
  let dialog_path = dialog.showOpenDialog({
    properties: ["openDirectory"]
  });

  user_provided_path = dialog_path[0];
}

function createList() {
  const notes_path = user_provided_path;
  const { subscribe, set, update } = writable(fragment.query("", notes_path));

  const asyncSearch = query => {
    return new Promise((resolve, reject) => {
      fragment.query_async(query, notes_path, (err, res) => {
        set(res);
      });
    });
  };

  return {
    subscribe,
    search: searchString => {
      asyncSearch(searchString);
    }
  };
}

function createNoteGetter() {
  const { subscribe, set, update } = writable({
    path: null,
    content: "Start typing to search for a note!"
  });

  return {
    subscribe,
    setNote: path => {
      set({ path: path, content: fragment.get_note(path) });
    }
  };
}

export const note = createNoteGetter();
export const list = createList();
