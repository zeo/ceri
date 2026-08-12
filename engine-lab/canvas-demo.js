const canvas = document.getElementById("art");
const button = document.getElementById("redraw");
const status = document.getElementById("status");
const context = canvas.getContext("2d");

function draw() {
  context.clearRect(0, 0, 560, 180);
  context.fillStyle = "#6ee7c5";
  context.fillRect(18, 18, 160, 64);
  context.fillStyle = "#ffd479";
  context.fillRect(200, 18, 160, 96);
  context.fillStyle = "#e06c75";
  context.fillRect(382, 18, 160, 132);
  status.textContent = "drawn with " + context.fillStyle;
}

button.addEventListener("click", draw);
draw();
console.log("canvas 2d demo ready");
