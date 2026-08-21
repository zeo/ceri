const button = document.getElementById("run-check");
const status = document.getElementById("status");
button.addEventListener("click", () => {
  status.textContent = "custom script handled the click";
  status.style.color = "#6ee7c5";
});
console.log("custom external resources ready");
