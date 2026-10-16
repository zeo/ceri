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
