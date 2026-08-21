const status = document.getElementById("status");
const frame = document.getElementById("frame");
requestAnimationFrame((timestamp) => {
  frame.textContent = "frame callback at " + timestamp + "ms";
});
setTimeout(() => {
  status.textContent = "timeout callback at 100ms";
  status.style.color = "#6ee7c5";
}, 100);
console.log("task clock armed");
