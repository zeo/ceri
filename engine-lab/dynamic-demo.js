const mount = document.getElementById("mount");
const created = document.createElement("p");
created.className = "created";
created.textContent = "created element is in the document";
mount.appendChild(created);
console.log("dynamic element appended");
