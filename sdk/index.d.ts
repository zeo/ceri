export type RuntimeRecoveryState = "ready" | "recovering" | "degraded";

export interface RuntimeInfo {
  readonly engine: string;
  readonly recovery: RuntimeRecoveryState;
  readonly session: string;
}

export type CeriBridgeErrorCode = "bridge-unavailable" | "invalid-response" | "query-failed";

export class CeriBridgeError extends Error {
  readonly code: CeriBridgeErrorCode;

  constructor(code: CeriBridgeErrorCode, message: string);
}

export function runtimeInfo(): Promise<RuntimeInfo>;

declare global {
  interface Window {
    ceriQuery?: (request: string) => Promise<string>;
  }
}
