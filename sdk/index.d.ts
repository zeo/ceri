export type RuntimeRecoveryState = "ready" | "recovering" | "degraded";

export interface RuntimeInfo {
  readonly engine: string;
  readonly recovery: RuntimeRecoveryState;
  readonly session: string;
}

export type CeriBridgeErrorCode =
  | "bridge-unavailable"
  | "invalid-response"
  | "query-failed"
  | "denied"
  | "unknown-command"
  | "invalid-payload"
  | "failed"
  | (string & {});

export class CeriBridgeError extends Error {
  readonly code: CeriBridgeErrorCode;

  constructor(code: CeriBridgeErrorCode, message: string);
}

export function runtimeInfo(): Promise<RuntimeInfo>;

export interface HostEvent<T = unknown> {
  readonly event: string;
  readonly payload: T;
}

export type Unlisten = () => void;

export function invoke<T = unknown>(command: string, args?: unknown): Promise<T>;

export function listen<T = unknown>(event: string, handler: (event: HostEvent<T>) => void): Promise<Unlisten>;

export function emit(event: string, payload?: unknown): Promise<void>;

export function windowLabel(): string;

export interface CeriHost {
  readonly windowLabel: string;
  invoke(command: string, args?: unknown): Promise<unknown>;
  listen(event: string, handler: (event: HostEvent) => void): Promise<Unlisten>;
  emit(event: string, payload?: unknown): Promise<void>;
}

declare global {
  interface Window {
    ceriQuery?: (request: string) => Promise<string>;
    ceri?: CeriHost;
  }
}
