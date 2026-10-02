/** Shared shapes between the Rust framework and this webview runtime. */

export type PluginKind = "rust" | "js";
export type PluginState = "pending" | "loaded" | "error";

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  kind: PluginKind;
  entry: string;
  runtime: string;
  methods: string[];
  permissions: string[];
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  kind: PluginKind;
  runtime: string;
  entry: string;
  methods: string[];
  permissions: string[];
  origin: string;
  state: PluginState;
  error: string | null;
}

export interface FrameworkInfo {
  version: string;
  pluginDir: string;
  pluginDirEnv: string;
  runtimeReady: boolean;
  invokeTimeoutMs: number;
  capabilities: { name: string; description: string }[];
}

export interface LogEntry {
  source: string;
  level: string;
  message: string;
  timestamp: number;
}

export interface PluginEvent {
  source: string;
  event: string;
  payload: unknown;
  timestamp: number;
}

/** Payload of `framework://js-load`. */
export interface JsLoadPayload {
  id: string;
  manifest: PluginManifest;
  source: string;
  entry_path: string;
}

/** Payload of `framework://js-invoke`. */
export interface InvokeRequest {
  pluginId: string;
  callId: number;
  method: string;
  params: unknown;
  caller: string | null;
}

export function describeError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object") {
    const maybe = error as { message?: unknown; code?: unknown };
    if (typeof maybe.message === "string") {
      return typeof maybe.code === "string" ? `${maybe.message} (${maybe.code})` : maybe.message;
    }
    try {
      return JSON.stringify(error);
    } catch {
      return String(error);
    }
  }
  return String(error);
}
