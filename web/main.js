import { CeriBridgeError, runtimeInfo } from "../sdk/index.js";

const checks = new Map(
  [...document.querySelectorAll("[data-check]")].map((check) => [
    check.dataset.check,
    check,
  ]),
);

const runButton = document.querySelector("#run-check");
const runtimeLine = document.querySelector("#runtime-line");
const score = document.querySelector("#score");

function updateCheck(name, state, detail) {
  const check = checks.get(name);
  check.dataset.state = state;
  check.querySelector(".check__state").textContent = state;
  check.querySelector(".check__detail").textContent = detail;
}

function checkModuleGraph() {
  return `module loaded from ${new URL(import.meta.url).pathname}`;
}

function checkStorage() {
  const key = "ceri.continuity.launches";
  const stored = Number.parseInt(localStorage.getItem(key) || "0", 10);
  const launches = Number.isFinite(stored) ? stored + 1 : 1;

  localStorage.setItem(key, String(launches));

  if (localStorage.getItem(key) !== String(launches)) {
    throw new Error("local storage did not confirm the write");
  }

  return `durable launch count ${launches}`;
}

async function checkRecoveryAsset() {
  const response = await fetch(new URL("./recovery.json", import.meta.url), {
    cache: "no-store",
  });

  if (!response.ok) {
    throw new Error(`recovery asset returned HTTP ${response.status}`);
  }

  const recovery = await response.json();

  if (
    recovery.status !== "armed" ||
    typeof recovery.strategy !== "string" ||
    !Array.isArray(recovery.preserves)
  ) {
    throw new Error("recovery asset has an unsupported shape");
  }

  return recovery;
}

function faultMessage(fault) {
  if (fault instanceof CeriBridgeError) {
    return `${fault.code}: ${fault.message}`;
  }

  return fault instanceof Error ? fault.message : "unknown failure";
}

async function runContinuityCheck() {
  runButton.disabled = true;
  runButton.textContent = "Checking";
  runtimeLine.textContent = "waiting for cefQuery";

  for (const name of checks.keys()) {
    updateCheck(name, "running", "check in progress");
  }

  updateCheck("module", "pass", checkModuleGraph());

  try {
    updateCheck("storage", "pass", checkStorage());
  } catch (fault) {
    updateCheck("storage", "fail", faultMessage(fault));
  }

  try {
    const recovery = await checkRecoveryAsset();
    updateCheck(
      "asset",
      "pass",
      `${recovery.strategy}; preserves ${recovery.preserves.join(" + ")}`,
    );
  } catch (fault) {
    updateCheck("asset", "fail", faultMessage(fault));
  }

  try {
    const info = await runtimeInfo();
    const session = info.session.slice(0, 8) || "unnamed";
    const state = info.recovery === "degraded" ? "fail" : "pass";

    runtimeLine.textContent = `${info.engine} / Chromium ${info.chromium}`;
    updateCheck(
      "runtime",
      state,
      `${info.recovery}; session ${session}`,
    );
  } catch (fault) {
    runtimeLine.textContent = "host bridge unavailable";
    updateCheck("runtime", "fail", faultMessage(fault));
  }

  const passed = document.querySelectorAll('[data-state="pass"]').length;
  score.textContent = `${passed} / ${checks.size}`;
  document.documentElement.dataset.outcome =
    passed === checks.size ? "ready" : "attention";
  runButton.disabled = false;
  runButton.textContent = "Run again";
}

runButton.addEventListener("click", runContinuityCheck);
runContinuityCheck();
