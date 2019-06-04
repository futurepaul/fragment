<script>
  import Search from "./search.svelte";
  import List from "./list.svelte";
  import { note } from "./stores.js";
  const fragment = require("fragment-backend");

  let note_value;

  const unsubscribe = note.subscribe(value => {
    note_value = value;
  });

  function openNote() {
    console.log("opening note...");
    fragment.open_note(note_value.path);
  }

  function refreshNote() {
    note.setNote(note_value.path);
  }
</script>

<style>
  .note {
    background-color: white;
    padding: 10px;
    overflow-y: auto;
    white-space: pre-line;
    -webkit-app-region: no-drag;
  }

  .controls {
    position: fixed;
    bottom: 0;
    right: 0;
  }

  button {
    background-color: pink;
    border: none;
    padding: 0.5em;
    margin-left: 0;
    margin-right: 0.5em;
    margin-bottom: 0.5em;
  }
</style>

<div class="note"> {note_value.content} </div>
<div class="controls">
  <button on:click={openNote} title="Open note">
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="24"
      viewBox="0 0 24 24">
      <path d="M4.01 2L4 22h16V8l-6-6H4.01zM13 9V3.5L18.5 9H13z" />
    </svg>
  </button>
  <button on:click={refreshNote} title="Refresh note">
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="24"
      viewBox="0 0 24 24">
      <path fill="none" d="M0 0h24v24H0V0z" />
      <path
        d="M17.65 6.35C16.2 4.9 14.21 4 12 4c-4.42 0-7.99 3.58-7.99 8s3.57 8
        7.99 8c3.73 0 6.84-2.55 7.73-6h-2.08c-.82 2.33-3.04 4-5.65 4-3.31
        0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z" />
    </svg>
  </button>
</div>
