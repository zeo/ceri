const status = document.getElementById("status");
status.textContent = "at " + location.pathname;

document.getElementById("replace-link").addEventListener("click", () => {
  window.location.replace("/navigation-about.html?source=replace#top");
});

console.log("packaged navigation controls ready");
