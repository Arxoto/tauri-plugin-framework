/*
 * Cross-plugin Caller.
 *
 * Demonstrates the interesting direction of the bridge: a JS plugin running in
 * a Worker asking the framework to call *another* plugin. The framework routes
 * the call (Rust or JS) and returns the answer to this Worker.
 *
 * `demo-caller` declares the "call" permission; without it every
 * `ctx.callPlugin` would be rejected by Rust with a `capability_denied` error.
 */

const startedAt = new Date().toISOString();

ctx.expose({
  describe() {
    return {
      id: ctx.manifest.id,
      kind: "js",
      startedAt,
      permissions: ctx.manifest.permissions,
      note: "uses ctx.callPlugin to reach builtin.counter (Rust) and demo.wordcount (JS)",
    };
  },

  ping({ message = "ping" } = {}) {
    return { pong: message, workerTime: ctx.now() };
  },

  /** JS Worker -> framework -> built-in Rust plugin. */
  async incCounter({ by = 1 } = {}) {
    await ctx.info(`asking builtin.counter to increment by ${by}`);
    const counter = await ctx.callPlugin("builtin.counter", "inc", { by });
    await ctx.emit("demo.caller.counter", counter);
    return counter;
  },

  /** One call in, two plugins out - the framework does the routing. */
  async fanout({ text = "plugins are neat" } = {}) {
    const [counted, counter] = await Promise.all([
      ctx.callPlugin("demo.wordcount", "count", { text }),
      ctx.callPlugin("builtin.counter", "get", {}),
    ]);
    return { text, counted, counter };
  },
});
