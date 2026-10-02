/*
 * Word Count plugin.
 *
 * This file is the *entry script* declared by manifest.json. The framework
 * reads it at runtime and evaluates it inside a dedicated Web Worker, with the
 * shim from `src/plugin-runtime/worker-shim.js` prepended. Everything the
 * plugin can do goes through the injected `ctx` object:
 *
 *   ctx.expose({ ... })          methods the framework may call
 *   ctx.onLoad(fn) / onUnload    lifecycle
 *   ctx.log/info/warn/error      framework log (needs "log")
 *   ctx.kv.get/set/delete/keys   shared key-value store (needs "kv")
 *   ctx.emit(event, payload)     broadcast to the frontend (needs "emit")
 *   ctx.callPlugin(id, method)   call another plugin (needs "call")
 *
 * There is no DOM, no `window`, no Tauri `invoke` and no filesystem access in
 * here - only `ctx`.
 */

const HISTORY_KEY = "demo.wordcount.history";

ctx.onLoad(async () => {
  await ctx.info("wordcount worker is up", { scope: self.constructor.name });
});

ctx.expose({
  /** The framework calls this automatically to show what the plugin is. */
  describe() {
    return {
      id: ctx.manifest.id,
      kind: "js",
      implementedIn: "index.js",
      isolated: true,
      runsIn: "dedicated Web Worker",
      hasDom: typeof document !== "undefined",
      hasTauriIpc: typeof window !== "undefined" && typeof window.__TAURI_INTERNALS__ !== "undefined",
      methods: ["count", "history", "reset"],
    };
  },

  async count({ text = "" } = {}) {
    const trimmed = String(text).trim();
    const words = trimmed.length === 0 ? [] : trimmed.split(/\s+/);
    const result = {
      characters: String(text).length,
      words: words.length,
      longest: words.reduce((a, b) => (b.length > a.length ? b : a), ""),
    };
    await ctx.info(`counted ${words.length} word(s)`);
    const history = (await ctx.kv.get(HISTORY_KEY)) ?? [];
    history.push({ at: ctx.now(), ...result });
    await ctx.kv.set(HISTORY_KEY, history.slice(-10));
    await ctx.emit("demo.wordcount.counted", result);
    return result;
  },

  async history() {
    return { entries: (await ctx.kv.get(HISTORY_KEY)) ?? [] };
  },

  async reset() {
    await ctx.kv.set(HISTORY_KEY, []);
    return { ok: true };
  },

  /** Deliberately broken, to show how errors cross the Worker boundary. */
  async explode() {
    throw new Error("intentional failure inside the wordcount worker");
  },
});
