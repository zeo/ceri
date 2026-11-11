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

export function invoke(command, args) {
  let host;
  try {
    host = bridge();
  } catch (error) {
    return Promise.reject(error);
  }
  return host.invoke(command, args).catch((error) => {
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

export function appVersion() {
  return invoke("app:version");
}
