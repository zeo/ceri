<script>
  let count = $state(0);
  let items = $state(["alpha", "beta"]);
  let draft = $state("");

  function add() {
    if (draft) {
      items.push(draft);
      draft = "";
    }
  }
</script>

<section>
  <p id="count">count: {count}</p>
  <button id="increment" onclick={() => count++}>add one</button>
</section>
<section>
  <input id="draft" bind:value={draft} />
  <button id="add" onclick={add}>add</button>
  <button id="drop" onclick={() => items.shift()}>drop first</button>
  <ul id="items">{#each items as item (item)}<li>{item}</li>{/each}</ul>
</section>
