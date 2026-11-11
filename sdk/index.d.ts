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

export interface WindowState {
  readonly maximized: boolean;
  readonly minimized: boolean;
  readonly fullscreen: boolean;
  readonly focused: boolean;
  readonly visible: boolean;
  readonly zoom: number;
  readonly scale: number;
  // physical pixels, on the virtual desktop
  readonly position: { readonly x: number; readonly y: number };
  readonly size: { readonly width: number; readonly height: number };
}

export interface WindowResized {
  // CSS pixels
  readonly width: number;
  readonly height: number;
  readonly maximized: boolean;
  readonly minimized: boolean;
  readonly fullscreen: boolean;
}

export interface AppWindow {
  show(): Promise<void>;
  hide(): Promise<void>;
  focus(): Promise<void>;
  minimize(): Promise<void>;
  unminimize(): Promise<void>;
  maximize(): Promise<void>;
  unmaximize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  center(): Promise<void>;
  requestAttention(): Promise<void>;
  close(): Promise<void>;
  setFullscreen(on: boolean): Promise<void>;
  setAlwaysOnTop(on: boolean): Promise<void>;
  setZoom(zoom: number): Promise<void>;
  setTitle(title: string): Promise<void>;
  state(): Promise<WindowState>;
  isMaximized(): Promise<boolean>;
  onResized(handler: (size: WindowResized) => void): Promise<Unlisten>;
}

export const appWindow: AppWindow;

export function appVersion(): Promise<string>;

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
