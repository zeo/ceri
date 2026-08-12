const payload = { status: "ready", count: 2 };
const statusKey = "status";
const countKey = "count";
const { [statusKey]: status, [countKey]: count } = payload;

document.getElementById("status").textContent = `${status} | ${count}`;
