<script>
  import { list } from "./stores.js";

  function submitQuery(e) {
    list.search(e.target.value);
  }
</script>

<style>
  input {
    width: 95vw;
    height: 2em;
    border: none;
    outline: none;
    font-size: 1em;
    -webkit-app-region: no-drag;
    background-color: gainsboro;
  }
  .search-box {
    background-color: white;
    grid-area: box;
    display: flex;
    justify-content: center;
    padding: 0 0.5em 0 0.5em;
    border-bottom: 2px solid black;
  }
</style>

<div class="search-box">
  <input on:input={submitQuery} type="text" />
</div>
