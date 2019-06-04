import { writable } from "svelte/store";
const fragment = require("fragment-backend");

function createList() {
  const { subscribe, set, update } = writable(fragment.query(""));

  const asyncSearch = query => {
    return new Promise((resolve, reject) => {
      fragment.query_async(query, (err, res) => {
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
