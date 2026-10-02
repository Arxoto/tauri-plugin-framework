/*
 * worker-shim.js
 * -----------------------------------------------------------------------------
 * This file is never imported as a module: the plugin runtime reads it as text
 * (`?raw`) and prepends it to the source of every JS plugin. The result is the
 * body of a *dedicated Web Worker*:
 *
 *     globalThis.__PLUGIN_MANIFEST__ = {...};   <- injected by the runtime
 *     <this shim>                               <- defines `ctx`
 *     <plugin index.js>                         <- calls ctx.expose({...})
 *     __PLUGIN_RUNTIME__.finish();              <- appended by the runtime
 *
 * Inside the Worker the plugin only has `ctx`. There is no DOM, no Tauri IPC,
 * no filesystem and no way to reach the framework window: every capability is
 * an async RPC answered by Rust (which checks the manifest permissions first).
 */
(function () {
  "use strict";

  var callSeq = 0;
  var pendingCalls = Object.create(null);
  var methods = Object.create(null);
  var onLoadFn = null;
  var onUnloadFn = null;

  function post(message) {
    self.postMessage(message);
  }

  function normalizeError(error) {
    if (error instanceof Error) {
      return error.stack ? error.message + "\n" + error.stack : error.message;
    }
    try {
      return JSON.stringify(error);
    } catch (_) {
      return String(error);
    }
  }

  /** Ask the framework to run one of its capabilities on our behalf. */
  function rpc(capability, args) {
    var callId = ++callSeq;
    return new Promise(function (resolve, reject) {
      pendingCalls[callId] = { resolve: resolve, reject: reject };
      post({ kind: "ctx", callId: callId, capability: capability, args: args || {} });
    });
  }

  var ctx = {
    manifest: globalThis.__PLUGIN_MANIFEST__ || {},
    framework: { name: "plugin-framework", version: "0.1.0" },
    now: function () {
      return Date.now();
    },
    log: function (level, message, data) {
      return rpc("log", { level: level, message: message, data: data });
    },
    debug: function (message, data) {
      return rpc("log", { level: "debug", message: message, data: data });
    },
    info: function (message, data) {
      return rpc("log", { level: "info", message: message, data: data });
    },
    warn: function (message, data) {
      return rpc("log", { level: "warn", message: message, data: data });
    },
    error: function (message, data) {
      return rpc("log", { level: "error", message: message, data: data });
    },
    kv: {
      get: function (key) {
        return rpc("kv.get", { key: key });
      },
      set: function (key, value) {
        return rpc("kv.set", { key: key, value: value });
      },
      delete: function (key) {
        return rpc("kv.delete", { key: key });
      },
      keys: function () {
        return rpc("kv.keys", {});
      },
    },
    emit: function (event, payload) {
      return rpc("emit", { event: event, payload: payload });
    },
    callPlugin: function (pluginId, method, params) {
      return rpc("callPlugin", { pluginId: pluginId, method: method, params: params });
    },
    /** Register the methods the framework is allowed to call. */
    expose: function (impl) {
      if (impl && typeof impl === "object") {
        Object.keys(impl).forEach(function (name) {
          methods[name] = impl[name];
        });
      }
      return ctx;
    },
    onLoad: function (fn) {
      onLoadFn = fn;
    },
    onUnload: function (fn) {
      onUnloadFn = fn;
    },
  };

  globalThis.ctx = ctx;
  globalThis.exposePlugin = function (impl) {
    return ctx.expose(impl);
  };
  globalThis.frameworkCtx = ctx;

  function handleInvoke(message) {
    var callId = message.callId;
    Promise.resolve()
      .then(function () {
        var fn = methods[message.method];
        if (typeof fn !== "function") {
          throw new Error("plugin does not expose a method named `" + message.method + "`");
        }
        return fn(message.params === undefined ? {} : message.params, ctx);
      })
      .then(function (value) {
        post({
          kind: "invokeResult",
          callId: callId,
          ok: true,
          value: value === undefined ? null : value,
        });
      })
      .catch(function (error) {
        post({ kind: "invokeResult", callId: callId, ok: false, error: normalizeError(error) });
      });
  }

  function handleUnload() {
    Promise.resolve()
      .then(function () {
        return onUnloadFn ? onUnloadFn(ctx) : null;
      })
      .catch(function (error) {
        post({ kind: "status", ok: false, message: "onUnload failed: " + normalizeError(error) });
      })
      .then(function () {
        post({ kind: "unloaded" });
        self.close();
      });
  }

  self.onmessage = function (event) {
    var message = event.data || {};
    if (message.kind === "ctxResult") {
      var pending = pendingCalls[message.callId];
      delete pendingCalls[message.callId];
      if (!pending) return;
      if (message.ok) pending.resolve(message.value);
      else pending.reject(new Error(message.error || "framework capability failed"));
      return;
    }
    if (message.kind === "invoke") {
      handleInvoke(message);
      return;
    }
    if (message.kind === "unload") {
      handleUnload();
    }
  };

  /** Uncaught errors inside the Worker are reported to the framework. */
  self.onerror = function (message, _source, _lineno, _colno, error) {
    post({
      kind: "status",
      ok: false,
      message: "uncaught error: " + (error ? normalizeError(error) : message),
    });
  };

  globalThis.__PLUGIN_RUNTIME__ = {
    expose: ctx.expose,
    methods: function () {
      return Object.keys(methods);
    },
    ctx: ctx,
    /** Called after the plugin entry script has been evaluated. */
    finish: function () {
      post({ kind: "ready", methods: Object.keys(methods) });
      if (!onLoadFn) return;
      Promise.resolve()
        .then(function () {
          return onLoadFn(ctx);
        })
        .then(function () {
          post({ kind: "status", ok: true, message: "onLoad finished" });
        })
        .catch(function (error) {
          post({ kind: "status", ok: false, message: "onLoad failed: " + normalizeError(error) });
        });
    },
  };
})();
