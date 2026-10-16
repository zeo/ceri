import { h, render } from "preact";
import { useState } from "preact/hooks";

function Counter() {
  const [count, setCount] = useState(0);
  return h("section", null,
    h("p", { id: "count" }, `count: ${count}`),
    h("button", { id: "increment", onClick: () => setCount(count + 1) }, "add one"),
  );
}

function List() {
  const [items, setItems] = useState(["alpha", "beta"]);
  const [draft, setDraft] = useState("");
  return h("section", null,
    h("input", { id: "draft", value: draft, onInput: (event) => setDraft(event.target.value) }),
    h("button", {
      id: "add",
      onClick: () => {
        if (draft) {
          setItems([...items, draft]);
          setDraft("");
        }
      },
    }, "add"),
    h("button", { id: "drop", onClick: () => setItems(items.slice(1)) }, "drop first"),
    h("ul", { id: "items" }, items.map((item) => h("li", { key: item }, item))),
  );
}

render(h("div", null, h(Counter), h(List)), document.getElementById("app"));
