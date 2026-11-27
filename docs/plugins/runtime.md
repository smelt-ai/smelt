# 调用运行时

Smelt 的会话、项目、Git、自动化、智能体设置都在**运行时** `smeltd` 里。每台挂上的机器（本机和配对的远程机器）各跑一个。插件经宿主调运行时的 [Control API](../control-api.md)：宿主管连接、重连和版本，插件只给机器和方法名。

```ts
import { call } from "smelt/sdk";

const status = await call(null, "git.status", { path: "/Users/you/project" });
```

## 机器

`machine` 一律是 `null`（本机）或远程机器的 id 字符串。从 `machineMeta()`、`conversationFacts()`、`activeContext()` 这些地方拿到的 `machine` 原样传回来就行，不要自己拼。

路径是**那台机器上**的路径。远程机器可能是 Windows，处理路径用 `pathStyle(machine)` 和 SDK 的[路径函数](reference/sdk.md#路径)，不要自己按 `/` 切。

## `call`

```ts
call<T>(machine: string | null, method: ControlMethodName, params?: object, timeoutMs?: number): Promise<T>
```

- `method` 是 Control API 的方法名，如 `"session.list"`、`"git.status"`、`"automation.list"`。类型 `ControlMethodName` 列出当前版本有的全部方法，编辑器里能补全。
- `params` 不给是 `{}`。
- `timeoutMs` 不给是 60 秒。
- 成功给 `result`，失败 Promise 失败，`message` 是给人看的原因。

方法清单和参数见 [Control API](../control-api.md)。参数和结果的类型在 `crates/smelt-model/src/control_api.rs`。

### 要错误码时用 `callDetail`

```ts
const r = await callDetail(machine, "agent.definition.create", { id, name });
if (!r.ok && r.code === "conflict") {
  // 已经有这个 id
} else if (!r.ok) {
  showError(r.message);
}
```

`callDetail` 不会失败，给的是 `{ ok: true, value }` 或 `{ ok: false, code, message }`。`code` 是运行时的错误码：`invalid_request`、`unsupported_version`、`method_not_found`、`invalid_params`、`permission_denied`、`not_found`、`failed_precondition`、`conflict`、`busy`、`temporarily_unavailable`、`operation_failed`、`internal`，以后可能加新的。连不上这类不是运行时回的错误时是 `null`。

### 大正文用 `callWithBlobs`

```ts
callWithBlobs<T>(machine, method, params, blobs: BlobAt[], timeoutMs?): Promise<T>
```

几 MB 的 diff、文件正文、一页对话这类插件只是转交给原生组件的内容，别进插件的 JS 堆：自带插件共用一个 VM 和 256 MB 的堆，一处爆了全都停。`blobs` 列出回复里哪些位置的正文以 `Blob` 交来，用 `blobUrl` 换成 `blob:` 地址交给原生组件（`SmeltEditor` 的 `valueUrl`、`SmeltMarkdown` 的 `markdownUrl`、`SmeltDiffView` 的 `patchUrl`、`<Image>` 的 `src`）。插件的堆随条数增长，不随字节数。

```ts
// 文本：JSON 指针
await callWithBlobs(machine, "git.diff", params, ["/patch"]);
// base64 字节（图片），类型在回复的 mime_at
await callWithBlobs(machine, "file.open", params, [
  { at: "/content/data", base64: true, mime_at: "/content/mime" },
]);
// 数组每一项里的正文
await callWithBlobs(machine, "history.detail", params, [{ at: "/detail/turns", each: "/text" }]);
```

那里没有正文就照原样。`callDetail` 的最后一个参数也是 `blobs`，位置相对 `value`。反方向不用这个：`params` 里放 `Blob`，运行时收到的就是它的文本。

`blobUrl(() => blob)` 在组件里调：换了 Blob 才换地址，旧地址和组件卸载时 revoke。

### `request`

```ts
request(machine, payload: object, timeoutMs?): Promise<any>
```

给运行时发一条守护命令：带 `op` 的原样请求，如 `{ op: "history_rename", ... }`。只有 Control API 还没覆盖的少数操作才用它；它的形状不在 Control API 的版本约定里，能用 `call` 就用 `call`。超时同样默认 60 秒。

## 机器不在线时

调不在线的机器会失败。要等它上线再做，用 `waitOnline`：

```ts
let failures = 0;
while (true) {
  if (!(await waitOnline(machine, failures))) return; // 机器被移除了
  try {
    await loadEverything(machine);
    return;
  } catch {
    failures += 1;
  }
}
```

- `waitOnline(machine)` 在线就马上给 `true`；不在线就等到它上线；机器被移除了给 `false`。等的是宿主事件，不轮询。
- 第二个参数是连续失败了几次。大于 0 时先退避一会儿（250 ms 起翻倍，最长 8 秒），重新连上会提前叫醒。用来处理「在线但调用失败」，这种情况没有事件可等。

## 运行时话题

运行时的有些变化是推出来的，不用问。插件先登记要哪些话题，再用 `onEvent` 收：

```ts
bootTask(async () => {
  await registerTopics(["automations.changed"]);
});

onEvent((event) => {
  if (event.type !== "runtime_topic" || event.topic !== "automations.changed") return;
  const { machine, message } = event;
  switch (message.kind) {
    case "event":
    case "snapshot":
      apply(machine, message.payload);
      break;
    case "lagged":
      void reload(machine);
      break;
    case "unavailable":
      markUnsupported(machine, message.reason);
      break;
  }
}, ["runtime_topic"]);
```

宿主按所有插件要的话题的并集向每台机器订，推来的只交给登记了这个话题的插件。读哪个话题就自己登记哪个：别的插件登记了也一样要登记，不然它一删你就断了。`registerTopics` 是整组登记，空数组是撤下。

`message` 是下面之一：

| `kind` | 意思 | 该做什么 |
| --- | --- | --- |
| `"event"` | 一条事件，`payload` 的形状见那个话题 | 按它更新 |
| `"snapshot"` | 带快照的话题的整份状态：订上（含重连）时一份，丢了事件后再一份 | 整份换掉自己留的 |
| `"lagged"` | 断开或积压，可能错过了事件 | 没有快照的话题自己重读；带快照的话题等后面的快照 |
| `"unavailable"` | 这台机器这一轮连接上不会推这个话题，多半是运行时比这个话题旧。`reason` 给人看 | 这台机器上这项功能不可用。重连后会再订 |

不带快照的话题，登记后自己用 `call` 读一次初始状态。

能登记的话题：

| 话题 | 载荷 | 快照 |
| --- | --- | --- |
| `workspace.changed` | 改完的整份工作区目录 | |
| `automations.changed` | 自动化变了 | |
| `project.changed` | 项目里文件变了。只在 `project.watch` 过的项目上发。一般用宿主事件 `project_changed` 就够了 | |
| `agent_memory.changed` | 智能体记忆目录里的文件变了。只在 `agent.memory.watch` 过的智能体上发 | |
| `agent_host.changed` | 整份智能体配置 | 有 |
| `attention.changed` | 一段会话的提醒变了。宿主已经镜像进 `sessionAttention()` | 有 |
| `pi_config.changed` | Pi 的配置变了 | |
| `history.changed` | 历史存档改过 | |
| `new_session.catalog_changed` | 「新建会话」的选项 | 有 |
| `terminal.notification`、`terminal.progress` | 终端程序发的通知、进度 | |
| `fs.usage.progress` | `{ scan, files, bytes, done }`：占用统计扫了多少，每秒几次，扫完（含取消）发一条 `done: true` | |

会话状态和工作区目录宿主已经订着，直接读[宿主状态](host-state.md#会话)，不用登记。

## 宿主给的其他能力

插件里没有 Node API，文件操作走运行时（见下面），本机和远程机器一样。宿主另外开了几样本机能力：

| 函数 | 做什么 |
| --- | --- |
| `fetch(url, init)` | Web 标准的 `fetch`，联网用它 |
| `openUrl(url)` | 用系统浏览器打开链接 |
| `openFile({ machine, path, line })` | 交给文件打开者打开，见[打开文件](commands.md#打开文件) |
| `revealPath(path)` | 在访达里显示本机路径 |
| `clipboardWrite(text)` | 写剪贴板 |
| `discoverApps(queries)` | 找本机装了哪些应用或命令行工具，可以带图标 |
| `launchPath({ kind, target, path })` | 用找到的应用或命令打开一个本机路径 |
| `processUsage()` | Smelt 自己的 CPU、内存 |
| `renderMermaidSvg(source)` | 把 Mermaid 源码画成 SVG |

文件操作走运行时，本机和远程机器一样：

| 方法 | 做什么 |
| --- | --- |
| `fs.stat` | `{ path }` → `{ kind, target_kind, size, allocated, modified, identity }`。`kind` 是 `"file"`、`"dir"`、`"symlink"`、`"other"`；符号链接的 `target_kind` 是它指向的东西，断了是 `null`。`allocated` 是实际占的磁盘字节，`modified` 是毫秒时间戳，`identity` 是 `{ dev, ino }` |
| `fs.read_dir` | `{ path }` → `{ entries }`，每项是 `name` 加上同 `fs.stat` 的字段。原样一层，不过滤不排序；目录不存在报 `not_found`、没权限报 `permission_denied` |
| `fs.delete` | `{ path, trash?, expect? }`。`trash: true` 移到废纸篓，否则直接删（目录连同内容）。`expect` 传之前拿到的 `identity`，路径上已经换成别的文件就报 `conflict`，不删 |
| `fs.volume` | `{ path }` → `{ mount, name, file_system, total, available }`，字节。macOS 上 `available` 含系统可清掉的空间，和访达一致 |
| `fs.known_dirs` | `{}` → `{ home, cache, config, data, temp, desktop, documents, downloads, trash }`，那台机器上的常用位置。macOS 的 `cache` 是 `~/Library/Caches`、`trash` 是 `~/.Trash`；没有的给 `null`（如 Windows 的 `trash`） |
| `fs.read` | `{ path, offset?, length?, encoding? }` → `{ size, content }`。`encoding` 是 `"base64"`（默认）或 `"utf8"`，不是 UTF-8 时报 `invalid_params`。一次最多 16 MB，大文件用 `offset`、`length` 分段，`size` 是整个文件的长度 |
| `fs.write` | `{ path, content, encoding?, mode? }`。`mode` 是 `"overwrite"`（默认）、`"create"`（已存在报 `conflict`）、`"append"`。不建上级目录 |
| `fs.create_dir` | `{ path }`，缺的上级目录一起建。已经是目录算成功 |
| `fs.rename` | `{ from, to, overwrite? }`，改名或移动，跨卷时先复制再删。目标已存在报 `conflict`，`overwrite: true` 时替换 |
| `fs.copy` | `{ from, to, overwrite? }`，文件或整个目录，目录里的符号链接复制成链接 |
| `fs.usage.scan` | `{ path }` → `{ scan }`。后台统计这个目录下每一层占多少，马上返回统计 id |
| `fs.usage.children` | `{ scan, path }` → `{ done, progress, entry, children }`。扫完后 `entry` 是 `path` 自己，`children` 是下一层，按占用从大到小；没扫完 `done` 是 `false`。子目录的大小是扫描那一刻的，文件当场列，删掉的不再出现 |
| `fs.usage.release` | `{ scan }`。不看了就放掉；在扫的会停下。同时留着的统计最多 4 个，满了 `fs.usage.scan` 报 `busy` |

路径都是那台机器上的绝对路径。统计按实际占的块算，硬链接只算一次，不跟符号链接，不跨卷。进度经话题 `fs.usage.progress` 推，见上面。

## 对话

`acpSubscribe`、`acpAction`、`acpHistory` 这组函数直接连运行时里的 ACP 对话流，是内置对话模块 `smelt/conversation` 用的底层接口，事件形状跟着 ACP 协议走，变得多。

想在插件里放一段对话，嵌对话视图或用对话模块，不要直接用这组函数。想让插件驱动一段对话（起对话、发话、取那一轮的结果），用 Control API 的 `session.create`、`conversation.prompt`、`conversation.turn_result`，见 [Control API：插件驱动对话](../control-api.md#插件驱动对话)。

## 相关

- [Control API](../control-api.md)
- [宿主状态与事件](host-state.md)
- [版本与兼容](compatibility.md)
