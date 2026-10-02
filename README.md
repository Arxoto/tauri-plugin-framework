# 插件框架可行性验证（Tauri 2 + React + Rust）

这个仓库用来验证一件事：**桌面应用能否在运行时加载插件，插件既可以用 Rust 写在程序里，也可以是磁盘目录上的一个 JS 目录；并且框架侧能主动调用插件实现的方法。**

结论：可以。JS 插件运行在各自独立的 Web Worker 中，与框架窗口隔离；Rust 插件与 JS 插件对调用方来说没有区别，都通过同一个 PluginHost 按 id 调用。

| 需求 | 实现 |
| --- | --- |
| 运行时从目录加载插件 | `PluginHost::bootstrap()` 扫描插件目录，读取每个子目录的 `manifest.json` + 入口脚本，无需重启、无需重新编译 |
| 插件类型支持 JS | 每个 JS 插件 = 一个 `manifest.json` + 一个 `index.js` |
| 插件可在 Rust 代码中定义 | 内置插件 `builtin.echo` / `builtin.counter` 在编译期注册，通过 `RustPlugin` trait 实现 |
| JS 插件用 manifest 声明入口 | `manifest.json` 的 `entry` 字段，默认 `index.js` |
| Rust 插件通过 PluginCtx 使用框架能力 | `log` / `kv` / `emit` / `callPlugin` 四个能力 |
| JS 侧同等能力 | Worker 内注入 `ctx`，同样的四个能力，经 RPC 回到 Rust 校验后执行 |
| 框架主动调用插件方法 | `plugin_invoke` 命令 + `PluginHost::invoke()`；启动后会自动对每个插件调用 `describe` |
| JS 在独立 Worker 中运行，与框架隔离 | 每个插件一个专属 Worker，只能看到 `ctx`，没有 DOM / Tauri IPC / 文件系统 |

## 快速开始

```bash
pnpm install
pnpm tauri dev        # 启动应用；UI 会列出所有插件并可直接调用
```

验证脚本（不需要 GUI）：

```bash
pnpm test:rust        # 13 个 Rust 测试：注册表、Rust 插件、能力校验、目录加载、错误分支、桥接回归
pnpm test:worker      # 20 个 Worker 契约检查：shim + 真实插件源码 + 跨插件调用
pnpm check:rust       # cargo check --workspace --all-targets
pnpm build            # tsc + vite build
```

## 项目结构

```
src-plugin-framework/         框架本体（不依赖 Tauri，可独立测试）
  src/bridge.rs                 UiBridge / TaskSpawner 两个接缝
  src/manifest.rs               manifest.json 解析与校验
  src/ctx.rs                    插件上下文（框架提供给插件的全部能力）
  src/rust_plugin.rs            RustPlugin trait
  src/host.rs                   注册表、能力分发、调用路由、日志与事件环形缓冲
  src/oneshot.rs                无依赖 oneshot（Worker 回包 → 等待中的异步任务）
  tests/framework.rs            集成测试

src-tauri/                     Tauri 宿主
  src/plugin_ipc.rs             IPC 命令（框架的对外接口）
  src/tauri_bridge.rs           UiBridge → AppHandle、TaskSpawner → async_runtime
  src/builtin/                  内置 Rust 插件：echo、counter

src/plugin-runtime/            Webview 侧的 JS 运行时
  runtime.ts                    管理 Worker 生命周期与消息中继
  worker-shim.js                注入到每个 Worker 里的 ctx 实现（以 ?raw 文本注入）

plugins/                       运行时插件目录（可直接放新插件）
  wordcount/                    manifest.json + index.js，用 kv/emit/log
  demo-caller/                  manifest.json + index.js，用 callPlugin 调用别的插件

scripts/worker-harness.mjs     Node 端 Worker 契约验证
docs/architecture.md           设计说明：分层、协议、调用链、边界
```

## 插件目录

默认按以下顺序寻找插件目录，取第一个存在的：

1. 环境变量 `PLUGIN_FRAMEWORK_DIR`
2. `<cwd>/plugins`
3. `<cwd>/../plugins`（`tauri dev` 时的实际命中项）
4. `<cwd>/src-tauri/plugins`

UI 上会显示最终解析到的目录，并提供“重新扫描插件目录”按钮，可以在应用运行时把新插件目录丢进去再扫描。

## 写一个 JS 插件

目录结构：

```
plugins/my-plugin/
  manifest.json
  index.js
```

`manifest.json`：

```json
{
  "id": "demo.wordcount",
  "name": "Word Count",
  "version": "0.1.0",
  "kind": "js",
  "runtime": "worker",
  "entry": "index.js",
  "methods": ["describe", "count"],
  "permissions": ["log", "kv", "emit"]
}
```

`index.js`（普通脚本，不是 ES module；框架会把 `worker-shim.js` 拼在它前面再放进 Worker）：

```js
ctx.onLoad(async () => {
  await ctx.info("ready");
});

ctx.expose({
  describe: () => ({ id: ctx.manifest.id, runsIn: "WebWorker" }),

  async count({ text = "" }) {
    const words = text.trim() ? text.trim().split(/\s+/).length : 0;
    await ctx.kv.set("last", { words });
    await ctx.emit("my-plugin.counted", { words });
    return { words };
  },
});
```

`ctx` API：

| 调用 | 说明 | 需要的权限 |
| --- | --- | --- |
| `ctx.expose({...})` | 注册框架可调用的方法 | — |
| `ctx.onLoad(fn)` / `ctx.onUnload(fn)` | 生命周期 | — |
| `ctx.manifest` / `ctx.now()` | 清单信息、时间戳 | — |
| `ctx.log(level, msg)` / `ctx.info/debug/warn/error` | 写入框架日志并推到 UI | `log` |
| `ctx.kv.get/set/delete/keys` | 与所有插件共享的键值存储 | `kv` |
| `ctx.emit(event, payload)` | 广播事件到框架前端 | `emit` |
| `ctx.callPlugin(id, method, params)` | 调用任意插件（Rust 或 JS）并等待结果 | `call` |

未声明的能力会在 Rust 侧被拒绝，Worker 里收到 `capability_denied` 错误。`permissions` 为空时只给 `log`。

## 写一个 Rust 插件

```rust
struct MyPlugin { manifest: PluginManifest }

impl RustPlugin for MyPlugin {
    fn manifest(&self) -> &PluginManifest { &self.manifest }

    fn on_load<'a>(&'a self, ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async move { ctx.info("loaded") })
    }

    fn invoke<'a>(&'a self, ctx: &'a PluginCtx, method: &'a str, params: Value)
        -> PluginFuture<'a, PluginResult<Value>>
    {
        Box::pin(async move {
            match method {
                "describe" => Ok(json!({ "id": ctx.plugin_id(), "kind": "rust" })),
                // 使用框架能力，或调用别的插件（可与 JS 插件互相调用）
                "fanout" => ctx.call_plugin("demo.wordcount", "count", json!({ "text": "a b" })).await,
                other => Err(PluginError::unknown_method(&self.manifest.id, other)),
            }
        })
    }
}
```

注册：`host.register_rust_plugin(Arc::new(MyPlugin::new()))?;`

## 一次调用的完整链路

框架调用 JS 插件：

```
plugin_invoke(pluginId, method, params)          # UI / 其它插件
   └─ PluginHost::invoke
        ├─ Rust 插件 → 进程内直接 await
        └─ JS 插件   → emit("framework://js-invoke", {callId, method, params})
                          └─ Webview 运行时 → 该插件的 Worker
                               └─ Worker 执行 index.js 里注册的方法
                                    └─ postMessage(结果) → 运行时
                                         └─ invoke("plugin_runtime_reply", {callId, ...})
                                              └─ oneshot 唤醒等待中的 invoke()（带 10s 看门狗）
```

插件使用框架能力（反方向）：

```
Worker: await ctx.callPlugin("builtin.counter", "inc", {by: 1})
   └─ postMessage({kind:"ctx", capability:"callPlugin", args})
        └─ 运行时 → invoke("plugin_ctx_call", {pluginId, capability, args})
             └─ Rust 校验 manifest.permissions → 路由到目标插件
                  └─ 结果沿原路返回，Worker 里的 Promise resolve
```

## 隔离性

每个 JS 插件一个专属 Worker：独立全局作用域、独立事件循环；插件崩溃或死循环不会卡住框架窗口。

Worker 里没有 `window`、`document`，也没有 Tauri 的 `invoke`——`ctx` 是唯一的出口，所有能力调用都要回到 Rust 过一遍权限表。

插件源码由 Rust 从磁盘读取后投递，前端不接触文件系统。

调用带超时看门狗（默认 10s，`PluginHost::set_invoke_timeout_ms` 可调），插件不响应时调用方会拿到 `timeout` 错误而不是永久挂起。

## 已知边界

这是可行性验证，以下是有意留白的部分：

* 插件目录只在启动和手动“重新扫描”时读取，没有文件监听热更新；重扫会替换同名插件。
* `index.js` 是经典脚本，不支持 `import`/`export`；也没有 Worker 内的模块加载器。
* `permissions` 是简单白名单（`log` / `kv` / `emit` / `call` / `*`），没有签名与版本约束。
* 事件是广播式的，没有订阅过滤；跨插件调用没有深度限制（可能递归）。
* 共享 kv 只在内存中，没有持久化。

详见 [docs/architecture.md](docs/architecture.md)。
