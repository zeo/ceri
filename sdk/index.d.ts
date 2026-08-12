export type RuntimeRecoveryState = "ready" | "recovering" | "degraded";

export interface RuntimeInfo {
  readonly engine: string;
  readonly chromium: string;
  readonly recovery: RuntimeRecoveryState;
  readonly session: string;
}

export type CeriBridgeErrorCode =
  | "bridge-unavailable"
  | "invalid-response"
  | "query-failed"
  | `cef:${number}`;

export class CeriBridgeError extends Error {
  readonly code: CeriBridgeErrorCode;

  constructor(code: CeriBridgeErrorCode, message: string);
}

export function runtimeInfo(): Promise<RuntimeInfo>;

export interface CefQueryRequest {
  readonly request: string;
  readonly persistent?: boolean;
  readonly onSuccess: (response: string) => void;
  readonly onFailure: (errorCode: number, errorMessage: string) => void;
}

declare global {
  interface Window {
    ceriQuery?: (request: string) => Promise<string>;
    cefQuery?: (request: CefQueryRequest) => number;
    cefQueryCancel?: (queryId: number) => void;
  }
}
