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

// an ArrayBuffer or typed array arrives in Rust as the call's raw body
export interface InvokeOptions {
  headers?: Record<string, string>;
}

export function invoke<T = unknown>(command: string, args?: unknown, options?: InvokeOptions): Promise<T>;

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

export interface ProgressOptions {
  // normal when a progress is given and none otherwise; linux launchers show no indeterminate
  readonly state?: "none" | "normal" | "indeterminate" | "paused" | "error";
  readonly progress?: number;
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
  // logical pixels, as the window's size and position are given everywhere
  setSize(width: number, height: number): Promise<void>;
  setPosition(x: number, y: number): Promise<void>;
  // the least the user can shrink the window to
  setMinimumSize(width: number, height: number): Promise<void>;
  setSkipTaskbar(on: boolean): Promise<void>;
  // pointer input goes to whatever is beneath the window
  setClickThrough(on: boolean): Promise<void>;
  // screen capture shows the window blank
  setContentProtected(on: boolean): Promise<void>;
  setTitle(title: string): Promise<void>;
  // the taskbar button's bar; a number is the fraction done from 0 to 1, and none clears it
  setProgress(progress?: number | ProgressOptions | null): Promise<void>;
  state(): Promise<WindowState>;
  isMaximized(): Promise<boolean>;
  onResized(handler: (size: WindowResized) => void): Promise<Unlisten>;
  // files from the desktop are held over the window, or no longer are
  onFileHover(handler: (over: boolean) => void): Promise<Unlisten>;
  // the paths of the files let go over the window
  onFileDrop(handler: (paths: readonly string[]) => void): Promise<Unlisten>;
  // the labels of the app's open windows
  labels(): Promise<readonly string[]>;
  monitors(): Promise<readonly Monitor[]>;
  // another of the app's windows has closed
  onClosed(handler: (label: string) => void): Promise<Unlisten>;
}

// a display, in physical pixels
export interface Monitor {
  readonly name: string | null;
  readonly primary: boolean;
  readonly scale: number;
  readonly position: { readonly x: number; readonly y: number };
  readonly size: { readonly width: number; readonly height: number };
  // what a taskbar or dock leaves free
  readonly workArea: {
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
  };
}

export const appWindow: AppWindow;

export interface DialogFilter {
  readonly name: string;
  // without the dot; "*" takes any file
  readonly extensions: readonly string[];
}

export interface OpenDialogOptions {
  readonly title?: string;
  // a folder to start in, or a file whose folder to start in and whose name to suggest
  readonly defaultPath?: string;
  readonly directory?: boolean;
  readonly multiple?: boolean;
  readonly filters?: readonly DialogFilter[];
}

export interface SaveDialogOptions {
  readonly title?: string;
  readonly defaultPath?: string;
  readonly filters?: readonly DialogFilter[];
}

export interface MessageDialogOptions {
  readonly title?: string;
  readonly kind?: "info" | "warning" | "error";
  // words for the buttons in place of OK and Cancel, or Yes and No
  readonly okLabel?: string;
  readonly cancelLabel?: string;
}

export interface Dialog {
  open(options: OpenDialogOptions & { readonly multiple: true }): Promise<string[] | null>;
  open(options?: OpenDialogOptions): Promise<string | null>;
  save(options?: SaveDialogOptions): Promise<string | null>;
  message(message: string, options?: string | MessageDialogOptions): Promise<boolean>;
  // yes or no
  ask(message: string, options?: string | MessageDialogOptions): Promise<boolean>;
  // OK or cancel
  confirm(message: string, options?: string | MessageDialogOptions): Promise<boolean>;
}

export const dialog: Dialog;

export interface Opener {
  openUrl(url: string): Promise<void>;
  // needs the app to grant opener:open-path
  openPath(path: string): Promise<void>;
  // the file manager on the item's folder, with the item selected
  revealItemInDir(path: string): Promise<void>;
}

export const opener: Opener;

export interface NotificationOptions {
  readonly title: string;
  readonly body?: string;
}

export type NotificationPermission = "granted" | "denied" | "default";

export interface Notifications {
  send(options: string | NotificationOptions): Promise<void>;
  permission(): Promise<NotificationPermission>;
  isPermissionGranted(): Promise<boolean>;
  requestPermission(): Promise<NotificationPermission>;
}

export const notification: Notifications;

export interface Autostart {
  enable(): Promise<void>;
  disable(): Promise<void>;
  isEnabled(): Promise<boolean>;
}

export const autostart: Autostart;

export interface ShortcutEvent {
  readonly shortcut: string;
  readonly state: "Pressed" | "Released";
}

export interface GlobalShortcut {
  // electron's or tauri's accelerator grammar, such as "CmdOrCtrl+Shift+K" or "Alt+F9"
  register(shortcuts: string | readonly string[], handler: (event: ShortcutEvent) => void): Promise<void>;
  unregister(shortcuts: string | readonly string[]): Promise<void>;
  unregisterAll(): Promise<void>;
  isRegistered(shortcut: string): Promise<boolean>;
}

export const globalShortcut: GlobalShortcut;

export interface DeepLink {
  // null until a link opens the app
  getCurrent(): Promise<string[] | null>;
  onOpenUrl(handler: (urls: string[]) => void): Promise<Unlisten>;
}

export const deepLink: DeepLink;

// a count over the app's icon; null or zero takes it away
export function setBadgeCount(count: number | null): Promise<void>;

export function appVersion(): Promise<string>;

export function fileSrc(path: string): string;

export interface CeriHost {
  readonly windowLabel: string;
  invoke(command: string, args?: unknown, options?: InvokeOptions): Promise<unknown>;
  listen(event: string, handler: (event: HostEvent) => void): Promise<Unlisten>;
  emit(event: string, payload?: unknown): Promise<void>;
}

declare global {
  interface Window {
    ceriQuery?: (request: string) => Promise<string>;
    ceri?: CeriHost;
  }
}
