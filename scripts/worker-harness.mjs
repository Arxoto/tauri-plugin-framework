/**
 * Worker contract harness.
 *
 * The webview is the only place where a real `Worker` can exist, so this script
 * emulates the *other* side of the bridge with Node's `worker_threads`:
 *
 *   - it plays the role of the Rust framework (capability checks, shared kv,
 *     cross-plugin routing, framework -> plugin invocation), and
 *   - it loads the very same `worker-shim.js` plus the real `plugins/*\/index.js`
 *     sources, exactly like `src/plugin-runtime/runtime.ts` does.
 *
 * It therefore verifies the shim/plugin protocol and the plugin sources, which
 * is the part that cannot be covered by `cargo test`. Browser level isolation
 * (`typeof document === "undefined"`, no Tauri IPC) is asserted as well, but
 * note the harness runs on Node, so Node globals exist in this context.
 *
 *   node scripts/worker-harness.mjs
 */

import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..");
const shimPath = path.join(repoRoot, "src/plugin-runtime/worker-shim.js");
const pluginsDir = path.join(repoRoot, "plugins");

const shim = await readFile(shimPath, "utf8");

// ------------------------------- assertions -------------------------------

let failures = 0;
let checks = 0;

function check(label, condition, detail = "") {
  checks += 1;
  if (condition) {
    console.log(`  PASS  ${label}`);
  } else {
    failures += 1;
    console.log(`  FAIL  ${label}${detail ? ` -> ${detail}` : ""}`);
  }
}

function section(title) {
  console.log(`\n${title}`);
}

// --------------------------- framework simulation --------------------------

const REQUIRED_PERMISSION = {
  log: "log",
  "kv.get": "kv",
  "kv.set": "kv",
  "kv.delete": "kv",
  "kv.keys": "kv",
  emit: "emit",
  callPlugin: "call",
};

const kv = new Map();
const emitted = [];
const logLines = [];
const hosts = new Map();

/** The worker bootstrap: turns worker_threads into a Web Worker shaped global. */
const bootstrap = `
const { parentPort, workerData } = require("node:worker_threads");
globalThis.self = globalThis;
globalThis.postMessage = (message) => parentPort.postMessage(message);
globalThis.close = () => process.exit(0);
parentPort.on("message", (data) => {
  if (typeof globalThis.onmessage === "function") globalThis.onmessage({ data });
});
const source =
  workerData.prelude +
  workerData.shim +
  "\\n" + workerData.plugin +
  "\\n;globalThis.__PLUGIN_RUNTIME__ && globalThis.__PLUGIN_RUNTIME__.finish();\\n";
(0, eval)(source);
`;

/** Wraps one plugin: this is `runtime.ts` + the Rust command handlers. */
function loadPlugin({ id, manifest, source }) {
  const worker = new Worker(bootstrap, {
    eval: true,
    workerData: {
      prelude: `globalThis.__PLUGIN_MANIFEST__ = ${JSON.stringify(manifest)};\n`,
      shim,
      plugin: source,
    },
  });

  const pending = new Map();
  let callSeq = 0;
  let readyResolve;
  let readyReject;
  const ready = new Promise((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });

  const host = {
    id,
    manifest,
    methods: [],
    ready,
    invoke(method, params, timeoutMs = 5000) {
      const callId = ++callSeq;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(callId);
          reject(new Error(`${id}.${method} timed out after ${timeoutMs}ms`));
        }, timeoutMs);
        pending.set(callId, {
          resolve: (value) => {
            clearTimeout(timer);
            resolve(value);
          },
          reject: (error) => {
            clearTimeout(timer);
            reject(error);
          },
        });
        worker.postMessage({ kind: "invoke", callId, method, params });
      });
    },
    terminate: () => worker.terminate(),
  };

  worker.on("message", (message) => {
    switch (message.kind) {
      case "ready":
        host.methods = message.methods ?? [];
        readyResolve(host);
        break;
      case "status":
        logLines.push({ source: id, level: message.ok === false ? "error" : "info", message: message.message });
        if (message.ok === false && !host.methods.length) {
          readyReject(new Error(message.message));
        }
        break;
      case "ctx": {
        void handleCapability(id, message).then(
          (value) => worker.postMessage({ kind: "ctxResult", callId: message.callId, ok: true, value }),
          (error) =>
            worker.postMessage({
              kind: "ctxResult",
              callId: message.callId,
              ok: false,
              error: String(error.message ?? error),
            }),
        );
        break;
      }
      case "invokeResult": {
        const entry = pending.get(message.callId);
        pending.delete(message.callId);
        if (!entry) return;
        if (message.ok) entry.resolve(message.value);
        else entry.reject(new Error(message.error));
        break;
      }
      default:
        break;
    }
  });

  worker.on("error", (error) => {
    readyReject(error);
  });

  hosts.set(id, host);
  return host;
}

/** The Rust side of `plugin_ctx_call`, including the permission check. */
async function handleCapability(pluginId, message) {
  const host = hosts.get(pluginId);
  const required = REQUIRED_PERMISSION[message.capability];
  if (!required) throw new Error(`unknown capability ${message.capability}`);
  const permissions = host.manifest.permissions ?? ["log"];
  if (!permissions.includes("*") && !permissions.includes(required)) {
    throw new Error(`capability_denied: ${pluginId} may not use ${required}`);
  }

  const args = message.args ?? {};
  switch (message.capability) {
    case "log":
      logLines.push({ source: pluginId, level: args.level ?? "info", message: args.message });
      return null;
    case "kv.get":
      return kv.has(args.key) ? kv.get(args.key) : null;
    case "kv.set":
      kv.set(args.key, args.value);
      return null;
    case "kv.delete":
      kv.delete(args.key);
      return null;
    case "kv.keys":
      return [...kv.keys()];
    case "emit":
      emitted.push({ source: pluginId, event: args.event, payload: args.payload });
      return null;
    case "callPlugin": {
      const target = hosts.get(args.pluginId);
      if (!target) throw new Error(`plugin_not_found: ${args.pluginId}`);
      return target.invoke(args.method, args.params);
    }
    default:
      throw new Error(`unknown capability ${message.capability}`);
  }
}

async function loadPluginFromDisk(directoryName) {
  const directory = path.join(pluginsDir, directoryName);
  const manifest = JSON.parse(await readFile(path.join(directory, "manifest.json"), "utf8"));
  const source = await readFile(path.join(directory, manifest.entry), "utf8");
  return loadPlugin({ id: manifest.id, manifest, source });
}

// ---------------------------------- checks --------------------------------

section("1. plugin loading from disk + worker handshake");
const wordcount = await loadPluginFromDisk("wordcount");
await wordcount.ready;
check("wordcount worker reports ready", true);
check(
  "exposed methods are visible to the framework",
  ["describe", "count", "history", "reset"].every((m) => wordcount.methods.includes(m)),
  wordcount.methods.join(","),
);

section("2. framework -> plugin invocation");
const described = await wordcount.invoke("describe", {});
check("describe reports kind js", described.kind === "js", JSON.stringify(described));
check("worker has no DOM", described.hasDom === false);
check("worker has no Tauri IPC", described.hasTauriIpc === false);

const countResult = await wordcount.invoke("count", {
  text: "the quick brown fox jumps over the lazy dog",
});
check("count returns 9 words", countResult.words === 9, JSON.stringify(countResult));
check("count reports the longest word", countResult.longest === "quick");

const history = await wordcount.invoke("history", {});
check("history was persisted in the shared kv store", history.entries.length === 1);

await wordcount.invoke("reset", {});
const clearedHistory = await wordcount.invoke("history", {});
check("reset clears the history", clearedHistory.entries.length === 0);

section("3. capability traffic (ctx.*) reaches the framework");
check(
  "ctx.emit reached the framework",
  emitted.some((entry) => entry.event === "demo.wordcount.counted" && entry.payload.words === 9),
);
check(
  "ctx.log reached the framework",
  logLines.some((line) => line.source === "demo.wordcount" && line.message.includes("counted 9 word")),
);
check("kv writes are visible to the framework", kv.has("demo.wordcount.history"));

section("4. errors cross the worker boundary");
const explosion = await wordcount.invoke("explode", {}).then(
  () => null,
  (error) => error,
);
check(
  "a thrown error is reported back to the framework",
  explosion !== null && explosion.message.includes("intentional failure"),
  explosion?.message,
);

const unknownMethod = await wordcount.invoke("nope", {}).then(
  () => null,
  (error) => error,
);
check(
  "an unknown method is reported back to the framework",
  unknownMethod !== null && unknownMethod.message.includes("does not expose a method"),
  unknownMethod?.message,
);

section("5. cross-plugin calls");
const caller = await loadPluginFromDisk("demo-caller");
await caller.ready.catch(() => undefined);
check("demo.caller worker reports ready", caller.methods.includes("fanout"), caller.methods.join(","));

// A stand-in for the built-in Rust plugin, so the harness stays self contained.
hosts.set("builtin.counter", {
  manifest: { id: "builtin.counter", permissions: ["*"] },
  invoke: async (method, params) => {
    if (method === "inc") return { value: (params?.by ?? 1) };
    if (method === "get") return { value: 1 };
    throw new Error(`unknown_method: builtin.counter.${method}`);
  },
});

const incremented = await caller.invoke("incCounter", { by: 3 });
check("JS worker -> framework -> Rust plugin", incremented.value === 3, JSON.stringify(incremented));
check(
  "the JS plugin's own emit made it back",
  emitted.some((entry) => entry.event === "demo.caller.counter"),
);

section("6. permission enforcement");
const restricted = loadPlugin({
  id: "harness.restricted",
  manifest: {
    id: "harness.restricted",
    kind: "js",
    runtime: "worker",
    entry: "index.js",
    permissions: ["log"],
  },
  source: `
    ctx.expose({
      async tryCall() {
        try {
          await ctx.callPlugin("builtin.counter", "get", {});
          return { allowed: true };
        } catch (error) {
          return { allowed: false, error: String(error.message || error) };
        }
      },
      async tryEmit() {
        try {
          await ctx.emit("harness.should-not-happen", {});
          return { allowed: true };
        } catch (error) {
          return { allowed: false, error: String(error.message || error) };
        }
      }
    });
  `,
});
await restricted.ready;

const callAttempt = await restricted.invoke("tryCall", {});
check(
  "a plugin without the `call` permission is blocked",
  callAttempt.allowed === false && callAttempt.error.includes("capability_denied"),
  JSON.stringify(callAttempt),
);
const emitAttempt = await restricted.invoke("tryEmit", {});
check(
  "a plugin without the `emit` permission is blocked",
  emitAttempt.allowed === false && emitAttempt.error.includes("capability_denied"),
  JSON.stringify(emitAttempt),
);

section("7. worker teardown");
await wordcount.terminate();
await caller.terminate();
await restricted.terminate();
check("workers terminated cleanly", true);

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) {
  console.log(`${failures} check(s) FAILED`);
  process.exit(1);
}
