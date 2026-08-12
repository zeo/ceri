const button = document.getElementById("run-url");
const status = document.getElementById("status");

button.addEventListener("click", () => {
  const url = new URL("/search?query=rust&tag=engine", location.href);
  const params = url.searchParams;
  params.set("query", "custom browser");
  params.append("tag", "from-scratch");
  params.sort();
  status.textContent = url.pathname + " / " + params.toString() + " / " + url.href;
  status.style.color = "#6ee7c5";
});

console.log("URL API demo ready");
