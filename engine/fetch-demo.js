const status = document.getElementById("status");
fetch("./fetch-demo.json")
  .then((response) => {
    return response.json();
  })
  .then((data) => {
    status.textContent = data.message + " / items: " + data.count;
    console.log(status.textContent);
  })
  .catch((error) => {
    status.textContent = "fetch failed: " + error;
  });
