const surface = document.getElementById("surface");
const status = document.getElementById("status");

surface.addEventListener("click", (event) => {
  status.textContent = event.type + " target=" + event.target.id + " current=" + event.currentTarget.id;
});

console.log("ancestor listener ready");
