const status = document.getElementById("status");
fetch("./fetch-demo.json")
  .then((response) => {
    return response.text().then((body) => {
      status.textContent = response.status + " " + response.ok + " " + body;
      console.log(status.textContent);
    });
  })
  .catch((error) => {
    status.textContent = "fetch failed: " + error;
  });
