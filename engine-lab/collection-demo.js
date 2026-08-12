const checks = new Map(
  [...document.querySelectorAll("[data-check]")].map((check) => [
    check.dataset.check,
    check,
  ]),
);
const status = document.getElementById("status");
const keys = checks.keys();
checks.get("module").className = "pass";
status.textContent = `${keys.join(" + ")} | ${checks.size} checks`;
