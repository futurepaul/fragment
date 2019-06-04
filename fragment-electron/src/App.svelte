<script>
  import Search from "./Search.svelte";
  import List from "./List.svelte";
  import Note from "./Note.svelte";
  import SplitPane from "./SplitPane.svelte";

  export let name;
</script>

<style>
  :global(body) {
    margin: auto;
    line-height: 1.6;
    color: #333;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto,
      Oxygen-Sans, Ubuntu, Cantarell, "Helvetica Neue", sans-serif;
  }

  .wrapper {
    display: grid;
    width: 100vw;
    height: 100vh;
    grid-template-columns: 1fr;
    grid-template-rows: 35px 3em 1fr;
    grid-template-areas: "title" "box" "splitpane";
  }
  header {
    z-index: 20;
    grid-area: title;
    -webkit-app-region: drag;
    display: flex;
    justify-content: center;
  }
</style>

<div class="wrapper">
  <header />
  <Search />
  <SplitPane type={'vertical'}>
    <section slot="a">
      <List />
    </section>
    <section slot="b">
      <Note />
    </section>
  </SplitPane>
</div>
