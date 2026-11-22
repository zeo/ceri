import assert from "node:assert/strict";
import { test } from "node:test";

// a host bridge that records what the page asks for and lets the test fire host events
const calls = [];
const listeners = new Map();
globalThis.window = {
  ceri: {
    invoke: (command, args) => {
      calls.push([command, args]);
      return Promise.resolve(null);
    },
    listen: (event, handler) => {
      listeners.set(event, handler);
      return Promise.resolve(() => listeners.delete(event));
    },
    emit: () => Promise.resolve(),
    windowLabel: "main",
  },
};
const { appWindow } = await import("./index.js");

test("close listeners decide whether the window goes", async () => {
  let refuse = true;
  const unlisten = await appWindow.onCloseRequested((event) => {
    if (refuse) event.preventDefault();
  });
  assert.deepEqual(calls.at(-1), ["window:intercept-close", { on: true }]);

  calls.length = 0;
  await listeners.get("window:close-requested")();
  assert.deepEqual(calls, [], "a refused close leaves the window open");

  refuse = false;
  await listeners.get("window:close-requested")();
  assert.deepEqual(calls, [["window:destroy", undefined]]);

  calls.length = 0;
  await unlisten();
  assert.deepEqual(calls, [["window:intercept-close", { on: false }]]);
});

test("resize dragging names the edge as Tauri does", async () => {
  calls.length = 0;
  await appWindow.startResizeDragging("SouthEast");
  assert.deepEqual(calls, [["window:start-resize-dragging", { direction: "SouthEast" }]]);
});
