const surface = document.getElementById("surface");
const status = surface.querySelector("#status");

surface.addEventListener("click", (event) => {
  surface.classList.add("clicked");
  status.textContent = event.type + " target=" + event.target.id + " current=" + event.currentTarget.id;
});

console.log("ancestor listener ready");
