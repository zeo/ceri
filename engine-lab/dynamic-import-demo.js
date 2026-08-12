const status = document.getElementById("status");

async function loadModule() {
  const { label, values } = await import("./dynamic-import-module.js");
  status.textContent = `${label} | ${values.join(" + ")}`;
}

loadModule();
