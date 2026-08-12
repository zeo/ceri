const status = document.getElementById("status");

async function loadConfig() {
  const { default: config } = await import("./json-import-config.json", {
    with: { type: "json" },
  });
  status.textContent = `${config.label} | ${config.count}`;
}

loadConfig();
