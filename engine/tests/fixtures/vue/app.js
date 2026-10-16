const { createApp, ref } = Vue;

createApp({
  setup() {
    const count = ref(0);
    const items = ref(["alpha", "beta"]);
    const draft = ref("");
    const add = () => {
      if (draft.value) {
        items.value.push(draft.value);
        draft.value = "";
      }
    };
    return { count, items, draft, add };
  },
}).mount("#app");
