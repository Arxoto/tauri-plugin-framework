/**
 * The JS side of the plugin framework.
 *
 * Rust owns the registry; this module owns the *isolation boundary*. For every
 * JS plugin Rust announces (`framework://js-load`) we build a dedicated Web
 * Worker out of `worker-shim.js` + the plugin source, and we relay messages in
 * both directions:
 *
 *   Rust ──framework://js-invoke──▶ runtime ──postMessage──▶ Worker
 *   Rust ◀─plugin_runtime_reply─── runtime ◀─postMessage─── Worker
 *   Rust ◀─plugin_ctx_call──────── runtime ◀─postMessage─── Worker (ctx.*)
 *
 * The plugin code itself never runs on the main thread.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import shimSource from "./worker-shim.js?raw";
import {
  describeError,
  type InvokeRequest,
  type JsLoadPayload,
  type PluginInfo,
} from "./types";

interface WorkerEntry {
  worker: Worker;
  url: string;
  methods: string[];
}

interface WorkerMessage {
  kind: string;
  callId?: number;
  capability?: string;
  args?: unknown;
  ok?: boolean;
  value?: unknown;
  error?: string;
  message?: string;
  methods?: string[];
}

export interface RuntimeCallbacks {
  onStatus?: (message: string) => void;
  onPluginsChanged?: (plugins: PluginInfo[]) => void;
}

export class PluginRuntime {
  private workers = new Map<string, WorkerEntry>();
  private unlisten: UnlistenFn[] = [];
  private callbacks: RuntimeCallbacks = {};
  private started = false;
  private stopping = false;

  setCallbacks(callbacks: RuntimeCallbacks): void {
    this.callbacks = { ...this.callbacks, ...callbacks };
  }

  /** Methods a Worker reported (JS plugins are dynamic, Rust ones are static). */
  jsMethodsFor(pluginId: string): string[] | undefined {
    return this.workers.get(pluginId)?.methods;
  }

  liveWorkers(): string[] {
    return [...this.workers.keys()];
  }

  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;
    this.unlisten.push(
      await listen<JsLoadPayload>("framework://js-load", (event) => {
        this.loadPlugin(event.payload);
      }),
    );
    this.unlisten.push(
      await listen<{ id: string }>("framework://js-unload", (event) => {
        this.unloadPlugin(event.payload.id);
      }),
    );
    this.unlisten.push(
      await listen<InvokeRequest>("framework://js-invoke", (event) => {
        void this.dispatch(event.payload);
      }),
    );
    // Only now can Rust hand plugins over: the listeners above must exist and
    // Worker creation must be possible.
    await invoke("plugin_runtime_ready");
  }

  async stop(): Promise<void> {
    this.stopping = true;
    for (const id of [...this.workers.keys()]) {
      this.unloadPlugin(id);
    }
    for (const unlisten of this.unlisten.splice(0)) {
      unlisten();
    }
  }

  // ------------------------------------------------------------- lifecycle

  private loadPlugin(payload: JsLoadPayload): void {
    try {
      this.unloadPlugin(payload.id, true);

      const prelude = `globalThis.__PLUGIN_MANIFEST__ = ${JSON.stringify(payload.manifest)};\n`;
      const bootstrap =
        "\n;globalThis.__PLUGIN_RUNTIME__ && globalThis.__PLUGIN_RUNTIME__.finish();\n";
      const header = `\n//# sourceURL=plugin://${payload.id}/${payload.entry_path.replace(/\\/g, "/")}\n`;
      const source = `${prelude}${shimSource}\n${header}${payload.source}${bootstrap}`;

      const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
      const worker = new Worker(url, { name: `plugin:${payload.id}` });
      this.workers.set(payload.id, { worker, url, methods: [] });

      // Assigning `onmessage` synchronously keeps us from racing the Worker's
      // first messages: nothing can be delivered on the main thread until the
      // current task yields.
      worker.onmessage = (event: MessageEvent<WorkerMessage>) => {
        this.handleWorkerMessage(payload.id, event.data);
      };
      worker.onerror = (event: ErrorEvent) => {
        const message = [
          event.message || "worker error",
          event.filename ? `at ${event.filename}:${event.lineno}:${event.colno}` : "",
        ]
          .filter(Boolean)
          .join(" ");
        this.status(`worker error [${payload.id}]: ${message}`);
        this.reportFailed(payload.id, message);
      };

      this.status(
        `worker spawned: ${payload.id} (${payload.entry_path}) permissions=${
          payload.manifest.permissions.join(",") || "log"
        } shim=${shimSource.length}B total=${source.length}B`,
      );
    } catch (error) {
      const message = describeError(error);
      this.status(`failed to spawn worker [${payload.id}]: ${message}`);
      this.reportFailed(payload.id, message);
    }
  }

  private reportFailed(pluginId: string, message: string): void {
    if (this.stopping) return;
    void invoke("plugin_runtime_status", { pluginId, ok: false, message }).catch(() => undefined);
  }

  private unloadPlugin(pluginId: string, silent = false): void {
    const entry = this.workers.get(pluginId);
    if (!entry) return;
    this.workers.delete(pluginId);
    try {
      entry.worker.postMessage({ kind: "unload" });
    } catch {
      // Worker already gone.
    }
    entry.worker.terminate();
    URL.revokeObjectURL(entry.url);
    if (!silent) this.status(`worker terminated: ${pluginId}`);
  }

  // ------------------------------------------------------------ messaging

  private handleWorkerMessage(pluginId: string, message: WorkerMessage): void {
    switch (message.kind) {
      case "ready": {
        const entry = this.workers.get(pluginId);
        if (entry) entry.methods = message.methods ?? [];
        this.status(
          `worker ready: ${pluginId} [${(message.methods ?? []).join(", ") || "no methods"}]`,
        );
        void invoke("plugin_runtime_status", { pluginId, ok: true, message: null }).catch(
          () => undefined,
        );
        break;
      }
      case "status": {
        const text = message.message ?? "worker status";
        this.status(`${pluginId}: ${text}`);
        void invoke("plugin_runtime_status", {
          pluginId,
          ok: message.ok !== false,
          message: text,
        }).catch(() => undefined);
        break;
      }
      case "ctx":
        void this.handleCapabilityCall(pluginId, message);
        break;
      case "invokeResult":
        void this.reply(
          message.callId ?? 0,
          message.ok !== false,
          message.value ?? null,
          message.error ?? null,
        );
        break;
      case "unloaded":
        this.status(`worker unloaded: ${pluginId}`);
        break;
      default:
        this.status(`unknown worker message from ${pluginId}: ${message.kind}`);
    }
  }

  /** A plugin asked for a capability; the framework decides if it is allowed. */
  private async handleCapabilityCall(pluginId: string, message: WorkerMessage): Promise<void> {
    const callId = message.callId ?? 0;
    try {
      const value = await invoke("plugin_ctx_call", {
        pluginId,
        capability: message.capability,
        args: message.args ?? {},
      });
      this.postToWorker(pluginId, { kind: "ctxResult", callId, ok: true, value: value ?? null });
    } catch (error) {
      const text = describeError(error);
      this.status(`capability denied/failed [${pluginId}]: ${message.capability} -> ${text}`);
      this.postToWorker(pluginId, { kind: "ctxResult", callId, ok: false, error: text });
    }
  }

  /** The framework calls a plugin method. */
  private async dispatch(request: InvokeRequest): Promise<void> {
    const entry = this.workers.get(request.pluginId);
    if (!entry) {
      await this.reply(
        request.callId,
        false,
        null,
        `no live Worker for plugin \`${request.pluginId}\``,
      );
      return;
    }
    entry.worker.postMessage({
      kind: "invoke",
      callId: request.callId,
      method: request.method,
      params: request.params,
    });
  }

  private async reply(
    callId: number,
    ok: boolean,
    value: unknown,
    error: string | null,
  ): Promise<void> {
    try {
      await invoke("plugin_runtime_reply", { callId, ok, value, error });
    } catch (caught) {
      this.status(`failed to deliver reply for call #${callId}: ${describeError(caught)}`);
    }
  }

  private postToWorker(pluginId: string, message: unknown): void {
    this.workers.get(pluginId)?.worker.postMessage(message);
  }

  private status(message: string): void {
    console.debug("[plugin-runtime]", message);
    this.callbacks.onStatus?.(message);
    // Mirror runtime diagnostics into the framework log so a Worker that never
    // boots is visible from Rust too, not just in the webview devtools.
    void invoke("plugin_runtime_log", { level: "debug", message }).catch(() => undefined);
  }
}

let runtime: PluginRuntime | null = null;

/** The runtime is a singleton so React StrictMode cannot double-load Workers. */
export function getPluginRuntime(): PluginRuntime {
  if (!runtime) runtime = new PluginRuntime();
  return runtime;
}
