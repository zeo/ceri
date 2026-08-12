const { label = "fallback", nested: { count = 2 } = {} } = {};
const [first = "first", second = first + " second"] = ["one"];

document.getElementById("status").textContent =
  `${label} | ${count} | ${second}`;
