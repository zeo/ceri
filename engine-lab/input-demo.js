const input = document.getElementById("name");
const status = document.getElementById("status");

input.addEventListener("focus", () => {
  status.textContent = "focused " + document.activeElement.id + ": " + input.value;
});

input.addEventListener("input", (event) => {
  status.textContent = "input " + event.data + ": " + input.value;
});

input.addEventListener("blur", () => {
  status.textContent = "blurred: " + input.value;
});

console.log("input controls ready");
