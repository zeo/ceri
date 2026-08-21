const form = document.getElementById("profile");
const status = document.getElementById("status");

form.addEventListener("submit", (event) => {
  event.preventDefault();
  const data = new FormData(form);
  status.textContent = data.get("name") + " / " + data.get("note") + " / " + data.get("remember");
  status.style.color = "#6ee7c5";
});

document.getElementById("submit").addEventListener("click", () => {
  form.requestSubmit();
});

console.log("FormData demo ready");
