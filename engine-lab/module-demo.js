import { message, suffix as addSuffix } from "./module-message.js";
import * as messages from "./module-message.js";

const status = document.getElementById("status");
status.textContent = `${addSuffix(messages.message)}|${message}|${new URL(import.meta.url).pathname}`;
