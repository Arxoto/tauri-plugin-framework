# 架构说明

## 分层

```
+---------------------------- Tauri 宿主 (src-tauri) ----------------------------+
|  plugin_ipc.rs        IPC 命令：plugin_list / plugin_invoke / plugin_ctx_call … |
|  tauri_bridge.rs      UiBridge -> AppHandle.emit、TaskSpawner -> async_runtime  |
|  builtin/             内置 Rust 插件                                            |
+-------------------------------^-----------------------------------------------+
                                | 依赖
+-------------------------------|-----------------------------------------------+
|  plugin-framework crate（不依赖 Tauri）                                        |
|   manifest.rs    PluginManifest（磁盘 manifest.json / 代码内构造）             |
|   ctx.rs         PluginCtx：框架暴露给插件的能力                               |
|   rust_plugin.rs RustPlugin trait（BoxFuture，无 async-trait 依赖）            |
|   host.rs        注册表 + 能力分发 + 调用路由 + 日志/事件缓冲 + 看门狗         |
|   oneshot.rs     无依赖 oneshot：Worker 回包 -> 等待中的 async 任务            |
|   bridge.rs      UiBridge / TaskSpawner 两个接缝，核心与 UI 解耦               |
+-------------------------------^-----------------------------------------------+
                                | Tauri 事件 + IPC
+-------------------------------|-----------------------------------------------+
|  Webview（React UI + src/plugin-runtime）                                      |
|   runtime.ts        监听 framework://js-load / js-invoke，中继消息             |
|   worker-shim.js    注入到每个 Worker 的 ctx 实现（?raw 文本）                 |
+-------------------------------^-----------------------------------------------+
                                | postMessage
                    +-----------+-----------+
                    |  Worker: 插件 A       |
                    |  Worker: 插件 B       |  每个插件一个独占 Worker
                    +-----------------------+
```

核心不依赖 Tauri 是刻意设计的：`UiBridge` / `TaskSpawner` 两个 trait 把“事件送到哪里”和“后台任务由谁驱动”交给宿主决定。好处是注册表、能力校验、Rust 插件路径、目录加载全部可以在没有 GUI 的情况下测试（见 `src-plugin-framework/tests/framework.rs`）。

## 数据结构

`PluginHost` 是一个 `Arc` 句柄，内部 `Inner`：

| 字段 | 作用 |
| --- | --- |
| `plugins: RwLock<BTreeMap<String, Arc<LoadedPlugin>>>` | 注册表，key 是插件 id |
| `kv: Mutex<HashMap<String, Value>>` | 所有插件共享的键值存储 |
| `pending: Mutex<HashMap<u64, oneshot::Sender<..>>>` | 已发出、等待 Worker 回包的调用 |
| `logs` / `events: Mutex<VecDeque<..>>` | 环形缓冲，UI 打开时能补看启动阶段的日志 |
| `call_seq: AtomicU64` | 调用 id 生成器 |
| `runtime_ready: AtomicBool` | 前端 Worker 运行时是否就绪 |
| `bridge` / `spawner: OnceLock<Arc<dyn ..>>` | 宿主注入的两个接缝 |

`LoadedPlugin.imp` 是 `PluginImpl::Rust(Arc<dyn RustPlugin>)` 或 `PluginImpl::Js { entry_path, source }`。调用方只看 id 和 `invoke()`，两种实现的差异全部被 `host.rs` 吸收。

## 事件协议（Rust -> Webview）

| 事件 | 时机 | 载荷 |
| --- | --- | --- |
| `framework://plugins` | 注册表变化 | `{ plugins: PluginInfo[] }` |
| `framework://js-load` | 把 JS 插件交给前端执行 | `{ id, manifest, source, entry_path }` |
| `framework://js-unload` | 卸载 Worker | `{ id }` |
| `framework://js-invoke` | 框架要调用插件方法 | `{ pluginId, callId, method, params, caller }` |
| `framework://log` | `ctx.log` / 框架日志 | `LogEntry` |
| `framework://event` | `ctx.emit` | `FrameworkEvent` |
| `framework://runtime-status` | 前端 Worker 运行时就绪 | `{ ready: true }` |
| `framework://kv` | 共享 kv 变化 | `{ key, changed }` |

## 命令协议（Webview -> Rust）

应用自定义命令不受 capability / ACL 限制（只有插件命令需要），因此 `capabilities/default.json` 保持原样即可。

| 命令 | 作用 |
| --- | --- |
| `framework_info` | 版本、插件目录、超时、能力清单 |
| `plugin_list` / `plugin_logs` / `plugin_events` | 注册表与缓冲区快照 |
| `plugin_invoke` | 框架 -> 插件调用（唯一的正向调用入口） |
| `plugin_rescan` | 重新扫描插件目录 |
| `plugin_runtime_ready` | 前端声明可以创建 Worker；Rust 随即推送所有 JS 插件 |
| `plugin_ctx_call` | Worker 请求框架能力（权限校验在这里） |
| `plugin_runtime_reply` | Worker 的调用结果回到 `pending` 表 |
| `plugin_runtime_status` | Worker 就绪 / onLoad 完成 / 失败 |

## 为什么 JS 插件要放进 Worker

直接在主线程 `eval` 插件源码最省事，但插件会拿到 `window`、`__TAURI_INTERNALS__`、`localStorage`，也能卡死 UI。Worker 方案换来三件事：

1. **隔离**：独立全局作用域与事件循环，插件崩溃不拖垮窗口；插件看不到 DOM 与 Tauri IPC。
2. **可控的能力面**：`ctx` 是唯一出口，所有能力调用都要经 `plugin_ctx_call` 回到 Rust 校验 `manifest.permissions`。
3. **可终止**：`worker.terminate()` 能强制结束失控插件（`runtime.ts` 的 `unloadPlugin` 就是这么做的）。

代价是插件入口必须是 Worker 能直接执行的经典脚本，所有能力调用都变成异步 RPC。

## 时序：框架调用 JS 插件

```
UI --invoke("plugin_invoke")--> Rust
                                | host.invoke()
                                |-- 建 oneshot，登记 callId
                                |-- emit framework://js-invoke
                                |-- 起看门狗线程（默认 10s）
                                        |
          Webview runtime <-------------+
                | worker.postMessage({kind:"invoke", callId, method, params})
                v
             Worker: methods[method](params, ctx)
                | postMessage({kind:"invokeResult", callId, ok, value})
                v
          runtime --invoke("plugin_runtime_reply")--> Rust
                                                       | resolve_pending(callId)
                                                       v
                                                  oneshot 唤醒 invoke()，返回结果
```

看门狗到点仍未回包时，会从 `pending` 表取走 sender 并 `send(Err(timeout))`，等待方拿到错误而不是永久挂起；之后迟到的回包被丢弃（`plugin_runtime_reply` 的 `false` 分支）。

## 时序：插件使用框架能力

```
Worker: await ctx.kv.set("history", [...])
   | postMessage({kind:"ctx", callId, capability:"kv.set", args})
   v
runtime --invoke("plugin_ctx_call", {pluginId, capability, args})--> Rust
                                                                     | check_capability
                                                                     |  -> required_permission
                                                                     |  -> manifest.permissions
                                                                     |-- 允许：执行并返回 Value
                                                                     |-- 拒绝：PluginError{capability_denied}
   <------------------------------------------------------------------
runtime postMessage({kind:"ctxResult", callId, ok, value|error})
   v
Worker: Promise resolve / reject
```

权限词表有两层：能力名（`kv.set`、`callPlugin`，JS 侧使用）映射到权限名（`kv`、`call`，manifest 中声明）。Rust 插件走 `PluginCtx` 时直接检查权限名，因为 `kind == rust` 视为可信、直接放行。

## 插件生命周期

1. `bootstrap()`：解析插件目录 -> 扫描子目录 -> 读 manifest 与入口脚本 -> 注册为 `pending`。
2. Rust 插件的 `on_load` 通过 `TaskSpawner` 异步执行（Tauri 下走 `async_runtime`），结束后状态变 `loaded`。
3. 前端 `runtime.start()` 注册监听后调用 `plugin_runtime_ready`，Rust 把每个 JS 插件的 manifest + 源码推给前端。
4. 前端用 `Blob(shim + 源码)` 建 Worker；Worker 内 `__PLUGIN_RUNTIME__.finish()` 上报方法表，`onLoad` 结果作为状态回报。
5. 卸载 / 重扫时 `worker.terminate()` 并回收 Blob URL；Rust 侧同名插件被新的 `LoadedPlugin` 替换。

## 可扩展点

* 加能力：`PluginCtx` 加方法 + `handle_capability` 加分支 + `required_permission` 加映射，JS 侧在 `worker-shim.js` 暴露对应函数。
* 换宿主：实现 `UiBridge` / `TaskSpawner` 即可脱离 Tauri 使用（例如接入 CLI、egui 或另一个 webview）。
* 持久化 kv、事件订阅过滤、插件签名、模块化 JS（`import` 需要自定义加载器）都是自然的下一步。
