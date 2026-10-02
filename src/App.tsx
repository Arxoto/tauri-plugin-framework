import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { getPluginRuntime } from "./plugin-runtime/runtime";
import {
  describeError,
  type FrameworkInfo,
  type LogEntry,
  type PluginEvent,
  type PluginInfo,
} from "./plugin-runtime/types";
import "./App.css";

type ConsoleTab = "logs" | "events" | "runtime";

interface Invocation {
  pluginId: string;
  method: string;
  ok: boolean;
  output: string;
  at: number;
}

/** Handy starting points so every demo method can be called in one click. */
const PARAM_PRESETS: Record<string, string> = {
  count: '{ "text": "the quick brown fox jumps over the lazy dog" }',
  fanout: '{ "text": "each plugin owns its own worker" }',
  incCounter: '{ "by": 1 }',
  inc: '{ "by": 1 }',
  get: "{}",
  reset: "{}",
  history: "{}",
  echo: '{ "hello": "world", "n": 42 }',
  reverse: '{ "text": "plugin framework" }',
  sum: '{ "numbers": [1, 2, 3, 4] }',
  boom: "{}",
  explode: "{}",
  ping: '{ "message": "hello from the playground" }',
  describe: "{}",
};

function defaultParams(method: string): string {
  return PARAM_PRESETS[method] ?? "{}";
}

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function clock(timestamp: number): string {
  return new Date(timestamp).toLocaleTimeString();
}

export default function App() {
  const runtime = useMemo(() => getPluginRuntime(), []);

  const [info, setInfo] = useState<FrameworkInfo | null>(null);
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [workers, setWorkers] = useState<string[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [method, setMethod] = useState("describe");
  const [paramsText, setParamsText] = useState("{}");
  const [result, setResult] = useState<Invocation | null>(null);
  const [busy, setBusy] = useState(false);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [events, setEvents] = useState<PluginEvent[]>([]);
  const [runtimeLog, setRuntimeLog] = useState<string[]>([]);
  const [tab, setTab] = useState<ConsoleTab>("logs");
  const [fatal, setFatal] = useState<string | null>(null);

  const autoRan = useRef(false);

  const pushRuntime = useCallback((message: string) => {
    setRuntimeLog((prev) => [...prev.slice(-199), `${clock(Date.now())}  ${message}`]);
  }, []);

  const refreshPlugins = useCallback(async () => {
    const list = await invoke<PluginInfo[]>("plugin_list");
    setPlugins(list);
    setWorkers(runtime.liveWorkers());
    return list;
  }, [runtime]);

  // ------------------------------------------------------------------ boot
  useEffect(() => {
    let disposed = false;
    const unlisten: (() => void)[] = [];

    runtime.setCallbacks({
      onStatus: (message) => {
        if (!disposed) pushRuntime(message);
      },
      onPluginsChanged: () => {
        if (!disposed) void refreshPlugins();
      },
    });

    const boot = async () => {
      setInfo(await invoke<FrameworkInfo>("framework_info"));
      await refreshPlugins();
      setLogs(await invoke<LogEntry[]>("plugin_logs"));
      setEvents(await invoke<PluginEvent[]>("plugin_events"));

      unlisten.push(
        await listen<LogEntry>("framework://log", (event) => {
          setLogs((prev) => [...prev.slice(-199), event.payload]);
        }),
      );
      unlisten.push(
        await listen<PluginEvent>("framework://event", (event) => {
          setEvents((prev) => [...prev.slice(-199), event.payload]);
        }),
      );
      unlisten.push(
        await listen<{ plugins: PluginInfo[] }>("framework://plugins", (event) => {
          setPlugins(event.payload.plugins);
          setWorkers(runtime.liveWorkers());
        }),
      );

      // Last step: tell Rust the webview can create Workers. Rust answers by
      // shipping every JS plugin's manifest + source.
      await runtime.start();
      pushRuntime("webview runtime ready, asking Rust for the plugin set");
    };

    void boot().catch((error) => {
      setFatal(describeError(error));
    });

    return () => {
      disposed = true;
      for (const off of unlisten) off();
    };
  }, [pushRuntime, refreshPlugins, runtime]);

  // ------------------------------------------------------- select a plugin
  useEffect(() => {
    if (plugins.length === 0) return;
    if (plugins.some((plugin) => plugin.id === selectedId)) return;
    const first = plugins[0];
    setSelectedId(first.id);
    const dynamic = runtime.jsMethodsFor(first.id) ?? [];
    const list = Array.from(new Set([...first.methods, ...dynamic]));
    const next = list[0] ?? "describe";
    setMethod(next);
    setParamsText(defaultParams(next));
  }, [plugins, runtime, selectedId]);

  const selected = useMemo(
    () => plugins.find((plugin) => plugin.id === selectedId) ?? null,
    [plugins, selectedId],
  );

  const methods = useMemo(() => {
    if (!selected) return [];
    const dynamic = runtime.jsMethodsFor(selected.id) ?? [];
    return Array.from(new Set([...selected.methods, ...dynamic]));
    // `workers` is a version token: it changes whenever the runtime spawns or
    // kills a Worker, which is when dynamic methods may have changed.
  }, [selected, runtime, workers]);

  // ---------------------------------------------------------- call a method
  const callMethod = useCallback(
    async (pluginId: string, methodName: string, rawParams: string) => {
      setBusy(true);
      try {
        let params: unknown = null;
        const trimmed = rawParams.trim();
        if (trimmed.length > 0) {
          try {
            params = JSON.parse(trimmed);
          } catch (error) {
            throw new Error(`参数不是合法 JSON: ${describeError(error)}`);
          }
        }
        const value = await invoke("plugin_invoke", {
          pluginId,
          method: methodName,
          params,
        });
        setResult({ pluginId, method: methodName, ok: true, output: pretty(value), at: Date.now() });
      } catch (error) {
        setResult({
          pluginId,
          method: methodName,
          ok: false,
          output: describeError(error),
          at: Date.now(),
        });
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  /** Let the framework actively interrogate every plugin. */
  const describeAll = useCallback(async () => {
    const list = await invoke<PluginInfo[]>("plugin_list");
    pushRuntime(`--- describe all (${list.length} plugins) ---`);
    const outcomes = await Promise.all(
      list.map(async (plugin) => {
        try {
          const value = await invoke("plugin_invoke", {
            pluginId: plugin.id,
            method: "describe",
            params: null,
          });
          return `${plugin.id} -> ${pretty(value).replace(/\s+/g, " ")}`;
        } catch (error) {
          return `${plugin.id} -> ERROR ${describeError(error)}`;
        }
      }),
    );
    outcomes.forEach((line) => pushRuntime(line));
    setTab("runtime");
  }, [pushRuntime]);

  /**
   * End-to-end self check: exercises the cross-plugin paths that a single
   * `describe` cannot reach.
   *
   * `demo.caller.fanout` is a JS Worker asking the framework to call
   * `builtin.counter` (Rust, in-process) and `demo.wordcount` (JS, another
   * Worker) in one go, so a single line exercises
   * Worker -> framework -> {Rust plugin, other Worker} -> back.
   */
  const selfCheck = useCallback(async () => {
    pushRuntime("--- end-to-end self check ---");
    const report = async (label: string, run: () => Promise<unknown>) => {
      try {
        const value = await run();
        pushRuntime(`${label} -> ${pretty(value).replace(/\s+/g, " ")}`);
      } catch (error) {
        pushRuntime(`${label} -> ERROR ${describeError(error)}`);
      }
    };

    await report("builtin.counter.inc (Rust, in-process)", () =>
      invoke("plugin_invoke", { pluginId: "builtin.counter", method: "inc", params: { by: 1 } }),
    );
    await report("demo.caller.incCounter (Worker -> framework -> Rust plugin)", () =>
      invoke("plugin_invoke", { pluginId: "demo.caller", method: "incCounter", params: { by: 2 } }),
    );
    await report("demo.caller.fanout (Worker -> framework -> Worker + Rust)", () =>
      invoke("plugin_invoke", {
        pluginId: "demo.caller",
        method: "fanout",
        params: { text: "one worker per plugin" },
      }),
    );
    await report("builtin.counter.fanout (Rust plugin -> framework -> Worker)", () =>
      invoke("plugin_invoke", {
        pluginId: "builtin.counter",
        method: "fanout",
        params: { text: "rust calling a worker" },
      }),
    );
    setTab("runtime");
  }, [pushRuntime]);

  // Runs once, as soon as every JS plugin has a live Worker.
  //
  // Deliberately no cleanup: `plugins` keeps changing while Workers report in,
  // and cancelling the pending run on those updates would silently skip the
  // self check (and the effect would not be re-armed, since the guard is set).
  useEffect(() => {
    if (autoRan.current || plugins.length === 0) return;
    const jsPlugins = plugins.filter((plugin) => plugin.kind === "js");
    const live = runtime.liveWorkers();
    if (!jsPlugins.every((plugin) => live.includes(plugin.id))) return;
    autoRan.current = true;
    void describeAll();
    void selfCheck();
  }, [plugins, runtime, describeAll, selfCheck]);

  // ---------------------------------------------------------------- actions
  const rescan = useCallback(async () => {
    try {
      const list = await invoke<PluginInfo[]>("plugin_rescan");
      setPlugins(list);
      setWorkers(runtime.liveWorkers());
      pushRuntime(`rescan finished, ${list.length} plugin(s) registered`);
    } catch (error) {
      pushRuntime(`rescan failed: ${describeError(error)}`);
    }
  }, [pushRuntime, runtime]);

  const pickPlugin = (plugin: PluginInfo) => {
    setSelectedId(plugin.id);
    const dynamic = runtime.jsMethodsFor(plugin.id) ?? [];
    const list = Array.from(new Set([...plugin.methods, ...dynamic]));
    const next = list[0] ?? "describe";
    setMethod(next);
    setParamsText(defaultParams(next));
    setResult(null);
  };

  return (
    <div className="shell">
      <header className="topbar">
        <div>
          <h1>Plugin Framework Playground</h1>
          <p className="subtitle">
            Runtime plugin loading — Rust plugins compiled in, JS plugins loaded from disk and
            executed in isolated Web Workers.
          </p>
        </div>
        <div className="topbar-actions">
          <button type="button" onClick={() => void rescan()}>
            重新扫描插件目录
          </button>
          <button type="button" onClick={() => void describeAll()}>
            对全部插件调用 describe
          </button>
          <button type="button" onClick={() => void selfCheck()}>
            端到端自检
          </button>
        </div>
      </header>

      {fatal && <div className="fatal">启动失败: {fatal}</div>}

      <section className="meta">
        <Chip label="framework" value={info ? `v${info.version}` : "…"} />
        <Chip label="plugin dir" value={info?.pluginDir ?? "…"} wide />
        <Chip
          label="js runtime"
          value={info ? (workers.length > 0 ? "ready" : "starting") : "…"}
        />
        <Chip label="workers" value={`${workers.length}`} />
        <Chip label="invoke timeout" value={info ? `${info.invokeTimeoutMs} ms` : "…"} />
      </section>

      <main className="grid">
        <section className="panel">
          <h2>插件 ({plugins.length})</h2>
          <ul className="plugin-list">
            {plugins.map((plugin) => (
              <li
                key={plugin.id}
                className={plugin.id === selectedId ? "plugin-card active" : "plugin-card"}
                onClick={() => pickPlugin(plugin)}
              >
                <div className="plugin-head">
                  <span className={`badge ${plugin.kind}`}>{plugin.kind}</span>
                  <strong>{plugin.name}</strong>
                  <span className={`state ${plugin.state}`}>{plugin.state}</span>
                </div>
                <code className="plugin-id">{plugin.id}</code>
                <div className="plugin-meta">
                  <span>{plugin.runtime}</span>
                  <span>·</span>
                  <span>{plugin.version}</span>
                  {workers.includes(plugin.id) && (
                    <>
                      <span>·</span>
                      <span className="worker">worker live</span>
                    </>
                  )}
                </div>
                <div className="chips">
                  {plugin.methods.map((name) => (
                    <span className="chip" key={name}>
                      {name}
                    </span>
                  ))}
                </div>
                <div className="origin">{plugin.origin}</div>
                {plugin.error && <div className="plugin-error">{plugin.error}</div>}
              </li>
            ))}
            {plugins.length === 0 && <li className="empty">还没有插件</li>}
          </ul>
        </section>

        <section className="panel">
          {selected ? (
            <>
              <h2>调用 {selected.id}</h2>
              <p className="hint">
                框架侧主动调用插件实现的方法。Rust 插件在进程内执行, JS 插件在独立 Worker
                中执行, 结果都经同一条路径返回。
              </p>
              <label className="field">
                <span>方法</span>
                <select
                  value={method}
                  onChange={(event) => {
                    const next = event.target.value;
                    setMethod(next);
                    setParamsText(defaultParams(next));
                  }}
                >
                  {methods.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="field">
                <span>参数 (JSON)</span>
                <textarea
                  spellCheck={false}
                  rows={6}
                  value={paramsText}
                  onChange={(event) => setParamsText(event.target.value)}
                />
              </label>
              <div className="row">
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  onClick={() => void callMethod(selected.id, method, paramsText)}
                >
                  {busy ? "调用中…" : "调用"}
                </button>
                <button type="button" onClick={() => setParamsText("{}")}>
                  重置参数
                </button>
              </div>

              <div className={result ? (result.ok ? "result ok" : "result error") : "result"}>
                <div className="result-head">
                  <span>结果</span>
                  {result && (
                    <span className="result-meta">
                      {result.pluginId}.{result.method} · {clock(result.at)}
                    </span>
                  )}
                </div>
                <pre>{result ? result.output : "还没有调用结果"}</pre>
              </div>

              <div className="detail">
                <div>
                  <span className="detail-label">permissions</span>
                  <span>{selected.permissions.join(", ") || "log"}</span>
                </div>
                <div>
                  <span className="detail-label">description</span>
                  <span>{selected.description || "—"}</span>
                </div>
                <div>
                  <span className="detail-label">capabilities</span>
                  <span>{(info?.capabilities ?? []).map((cap) => cap.name).join(", ")}</span>
                </div>
              </div>
            </>
          ) : (
            <h2>选择一个插件</h2>
          )}
        </section>
      </main>

      <section className="console">
        <div className="tabs">
          <button
            type="button"
            className={tab === "logs" ? "active" : ""}
            onClick={() => setTab("logs")}
          >
            日志 ({logs.length})
          </button>
          <button
            type="button"
            className={tab === "events" ? "active" : ""}
            onClick={() => setTab("events")}
          >
            插件事件 ({events.length})
          </button>
          <button
            type="button"
            className={tab === "runtime" ? "active" : ""}
            onClick={() => setTab("runtime")}
          >
            运行时 ({runtimeLog.length})
          </button>
          <div className="spacer" />
          <button
            type="button"
            onClick={() => {
              setLogs([]);
              setEvents([]);
              setRuntimeLog([]);
            }}
          >
            清空
          </button>
        </div>
        <div className="console-body">
          {tab === "logs" &&
            logs.map((entry, index) => (
              <div className="line" key={`${entry.timestamp}-${index}`}>
                <span className="time">{clock(entry.timestamp)}</span>
                <span className={`level ${entry.level}`}>{entry.level}</span>
                <span className="source">{entry.source}</span>
                <span className="text">{entry.message}</span>
              </div>
            ))}
          {tab === "events" &&
            events.map((entry, index) => (
              <div className="line" key={`${entry.timestamp}-${index}`}>
                <span className="time">{clock(entry.timestamp)}</span>
                <span className="level event">event</span>
                <span className="source">{entry.source}</span>
                <span className="text">
                  {entry.event} {JSON.stringify(entry.payload)}
                </span>
              </div>
            ))}
          {tab === "runtime" &&
            runtimeLog.map((line, index) => (
              <div className="line" key={`${line}-${index}`}>
                <span className="text mono">{line}</span>
              </div>
            ))}
        </div>
      </section>
    </div>
  );
}

function Chip({ label, value, wide }: { label: string; value: string; wide?: boolean }) {
  return (
    <div className={wide ? "meta-chip wide" : "meta-chip"}>
      <span className="meta-label">{label}</span>
      <span className="meta-value" title={value}>
        {value}
      </span>
    </div>
  );
}
