const button = document.getElementById("run-storage");
const status = document.getElementById("status");

button.addEventListener("click", () => {
  localStorage.setItem("engine", "custom");
  sessionStorage.setItem("mode", "lab");
  status.textContent = localStorage.length + " / " + localStorage.getItem("engine") + " / " + sessionStorage.getItem("mode");
  status.style.color = "#6ee7c5";
});

console.log("storage API demo ready");
