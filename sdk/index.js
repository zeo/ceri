const RUNTIME_INFO_REQUEST = JSON.stringify({
  method: "runtimeInfo",
  version: 1,
});

const RECOVERY_STATES = new Set(["ready", "recovering", "degraded"]);

export class CeriBridgeError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "CeriBridgeError";
    this.code = code;
  }
}

function decodeRuntimeInfo(response) {
  let payload;

  try {
    payload = JSON.parse(response);
  } catch {
    throw new CeriBridgeError(
      "invalid-response",
      "The Ceri host returned malformed runtime information",
    );
  }

  if (
    typeof payload !== "object" ||
    payload === null ||
    typeof payload.engine !== "string" ||
    typeof payload.session !== "string" ||
    !RECOVERY_STATES.has(payload.recovery)
  ) {
    throw new CeriBridgeError(
      "invalid-response",
      "The Ceri host returned an unsupported runtime information shape",
    );
  }

  return Object.freeze({
    engine: payload.engine,
    recovery: payload.recovery,
    session: payload.session,
  });
}

export function runtimeInfo() {
  if (typeof window === "undefined" || typeof window.ceriQuery !== "function") {
    return Promise.reject(
      new CeriBridgeError(
        "bridge-unavailable",
        "The Ceri host bridge is unavailable in this document",
      ),
    );
  }

  return window.ceriQuery(RUNTIME_INFO_REQUEST).then(decodeRuntimeInfo, () => {
    throw new CeriBridgeError(
      "query-failed",
      "The Ceri host rejected the runtime information request",
    );
  });
}

function bridge() {
  if (typeof window === "undefined" || typeof window.ceri !== "object" || window.ceri === null) {
    throw new CeriBridgeError("bridge-unavailable", "The Ceri host bridge is unavailable in this document");
  }
  return window.ceri;
}

// a host error arrives as an Error carrying a code; callers see one CeriBridgeError shape
function asBridgeError(error) {
  if (error instanceof CeriBridgeError) {
    return error;
  }
  const code = typeof error?.code === "string" ? error.code : "failed";
  const message = typeof error?.message === "string" ? error.message : String(error);
  return new CeriBridgeError(code, message);
}

export function invoke(command, args, options) {
  let host;
  try {
    host = bridge();
  } catch (error) {
    return Promise.reject(error);
  }
  return host.invoke(command, args, options).catch((error) => {
    throw asBridgeError(error);
  });
}

export function listen(event, handler) {
  let host;
  try {
    host = bridge();
  } catch (error) {
    return Promise.reject(error);
  }
  return host.listen(event, handler);
}

export function emit(event, payload) {
  let host;
  try {
    host = bridge();
  } catch (error) {
    return Promise.reject(error);
  }
  return host.emit(event, payload);
}

export function windowLabel() {
  return bridge().windowLabel;
}

function command(name) {
  return (args) => invoke(name, args);
}

// the page's own window; the app origin may always drive it
export const appWindow = Object.freeze({
  show: command("window:show"),
  hide: command("window:hide"),
  focus: command("window:focus"),
  minimize: command("window:minimize"),
  unminimize: command("window:unminimize"),
  maximize: command("window:maximize"),
  unmaximize: command("window:unmaximize"),
  toggleMaximize: command("window:toggle-maximize"),
  center: command("window:center"),
  requestAttention: command("window:request-attention"),
  close: command("window:close"),
  setFullscreen: (on) => invoke("window:set-fullscreen", { on }),
  setAlwaysOnTop: (on) => invoke("window:set-always-on-top", { on }),
  setZoom: (zoom) => invoke("window:set-zoom", { zoom }),
  setTitle: (title) => invoke("window:set-title", { title }),
  state: command("window:state"),
  isMaximized: () => invoke("window:state").then((state) => state.maximized),
  onResized: (handler) => listen("window:resized", (event) => handler(event.payload)),
});

function asking(name) {
  return (message, options = {}) =>
    invoke(name, { message, ...(typeof options === "string" ? { title: options } : options) });
}

// the system's own dialogs, owned by the page's window; a cancelled pick resolves to null
export const dialog = Object.freeze({
  open: (options) => invoke("dialog:open", options ?? {}),
  save: (options) => invoke("dialog:save", options ?? {}),
  message: asking("dialog:message"),
  ask: asking("dialog:ask"),
  confirm: asking("dialog:confirm"),
});

// links open in the system's browser, mail or phone app; a page may open http, https, mailto and
// tel links and reveal an item, and the app grants opener:open-path for anything else
export const opener = Object.freeze({
  openUrl: (url) => invoke("opener:open-url", { url }),
  openPath: (path) => invoke("opener:open-path", { path }),
  revealItemInDir: (path) => invoke("opener:reveal", { path }),
});

// a desktop notification under the app's name; a string is the title alone
export const notification = Object.freeze({
  send: (options) =>
    invoke("notification:show", typeof options === "string" ? { title: options } : options),
  permission: () => invoke("notification:permission"),
  isPermissionGranted: () => invoke("notification:permission").then((state) => state === "granted"),
  requestPermission: () => invoke("notification:permission"),
});

// starting the app when the user logs in, with the arguments the app names for it
export const autostart = Object.freeze({
  enable: () => invoke("autostart:enable"),
  disable: () => invoke("autostart:disable"),
  isEnabled: () => invoke("autostart:is-enabled"),
});

// shortcuts that reach the app while another app has the keyboard, once the app grants
// global-shortcut:*; a handler hears each press and release under the name it registered
const shortcutHandlers = new Map();
let shortcutEvents;

export const globalShortcut = Object.freeze({
  async register(shortcuts, handler) {
    shortcutEvents ??= listen("global-shortcut", ({ payload }) => {
      const registered = shortcutHandlers.get(payload.shortcut);
      registered?.handler({ shortcut: registered.name, state: payload.state });
    });
    await shortcutEvents;
    for (const name of [shortcuts].flat()) {
      const canonical = await invoke("global-shortcut:register", { shortcut: name });
      shortcutHandlers.set(canonical, { name, handler });
    }
  },
  async unregister(shortcuts) {
    for (const name of [shortcuts].flat()) {
      await invoke("global-shortcut:unregister", { shortcut: name });
      for (const [canonical, registered] of shortcutHandlers) {
        if (registered.name === name) shortcutHandlers.delete(canonical);
      }
    }
  },
  async unregisterAll() {
    await invoke("global-shortcut:unregister-all");
    shortcutHandlers.clear();
  },
  isRegistered: (shortcut) => invoke("global-shortcut:is-registered", { shortcut }),
});

// links in the app's own schemes: the latest it was opened with, and each that reaches it after
export const deepLink = Object.freeze({
  getCurrent: () => invoke("deep-link:get-current"),
  onOpenUrl: (handler) => listen("deep-link", ({ payload }) => handler(payload.urls)),
});

export function appVersion() {
  return invoke("app:version");
}

const FILES_ORIGIN = "https://files.ceri.invalid";

// the URL a page loads an absolute file path from, once the app allows a folder holding it
export function fileSrc(path) {
  const drive = /^([A-Za-z]):[\\/]+(.*)$/s.exec(path);
  const share = /^\\\\(.*)$/s.exec(path);
  const parts = drive
    ? [drive[1], ...drive[2].split(/[\\/]/)]
    : share
      ? ["UNC", ...share[1].split(/[\\/]/)]
      : path.split("/");
  return `${FILES_ORIGIN}/${parts.filter(Boolean).map(encodeURIComponent).join("/")}`;
}
