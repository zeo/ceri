const status = document.getElementById("status");
status.textContent = "at " + location.pathname;

document.getElementById("replace-link").addEventListener("click", () => {
  window.location.replace("/navigation-about.html?source=replace#top");
});

document.getElementById("push-link").addEventListener("click", () => {
  window.history.pushState({ view: "lab-route" }, "", "/navigation-demo-route");
  status.textContent = "SPA route at " + location.pathname + " / " + window.history.state.view + " / entries: " + window.history.length;
});

window.addEventListener("popstate", (event) => {
  if (event.state === null) {
    status.textContent = "SPA history returned to " + location.pathname;
  } else {
    status.textContent = "SPA history at " + location.pathname + " / " + event.state.view;
  }
});

document.getElementById("back-link").addEventListener("click", () => {
  window.history.back();
});

console.log("packaged navigation controls ready");
