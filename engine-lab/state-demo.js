const action = document.getElementById("action");
const switchControl = document.getElementById("switch");
const status = document.getElementById("status");

function report() {
  if (switchControl.checked) {
    status.textContent = "checked state active";
  } else {
    status.textContent = "unchecked state active";
  }
}

action.addEventListener("focus", () => {
  status.textContent = "focus state active";
});
switchControl.addEventListener("change", report);
report();
console.log("dynamic selector demo ready");
