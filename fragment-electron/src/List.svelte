<script>
  import { note, list } from "./stores.js";

  let list_value;

  const unsubscribe = list.subscribe(value => {
    list_value = value;
  });

  function setNote(path) {
    note.setNote(path);
  }
</script>

<style>
  .list {
    padding-left: 10px;
    padding-top: 10px;
    padding-right: 10px;
    overflow-y: auto;
    -webkit-app-region: drag;
  }

  .list-item {
    border-bottom: 1px solid grey;
  }

  .list-item:hover {
    background-color: pink;
    cursor: default;
  }
</style>

<div class="list">
  {#each list_value as item}
    <div class="list-item" on:click={() => setNote(item.path)}>
       {item.file_name}
    </div>
  {/each}
</div>
