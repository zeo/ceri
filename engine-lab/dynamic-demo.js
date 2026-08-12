const mount = document.getElementById("mount");
const created = document.createElement("section");
created.className = "created-card";
created.innerHTML = '<p class="created">created fragment is in the document</p>';
mount.appendChild(created);
console.log(created.innerHTML);
