const response = fetch("./fetch-demo.json");
const status = document.getElementById("status");
status.textContent = response.status + " " + response.ok + " " + response.text();
console.log(status.textContent);
