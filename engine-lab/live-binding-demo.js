import { count, increment } from "./live-binding-counter.js";
import * as counter from "./live-binding-counter.js";

const status = document.getElementById("status");
status.textContent = `${count} | ${counter.count}`;
increment();
status.textContent = `${status.textContent} | ${count} | ${counter.count}`;
