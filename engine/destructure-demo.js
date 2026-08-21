const payload = {
  title: "ready",
  counts: { complete: 2 },
  extra: "kept",
};
const { title, counts: { complete }, ...rest } = payload;
const [first, , third, ...tail] = ["a", "b", "c", "d"];
const label = ({ title }, [first]) => `${title}|${first}`;
const iterator = new Map([["module", 1], ["storage", 2]]).keys();
const firstKey = iterator.next().value;

document.getElementById("status").textContent =
  `${label(payload, [first])}|${complete}|${rest.extra}|${third}|${tail.length}|${firstKey}`;
