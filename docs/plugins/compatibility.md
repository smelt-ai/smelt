# 版本与兼容

插件跟着 Smelt 一起升级。这一页说明升级时插件这边会遇到什么，以及怎么写能少受影响。

## 现在的情况

- **插件不声明它要哪个版本的 Smelt**。清单没有这样的字段，宿主也不比较插件的 `version`。
- **`smelt/sdk` 由宿主注入**，永远是当前 Smelt 的那一份。插件以源码分发，每次加载时宿主按当前版本的 SDK 和 NSH 打包、编译；插件目录里的预编译字节码会被忽略。
- 所以 Smelt 升级后，插件用的是新 SDK。SDK 删了或改了你用到的东西，问题在加载或运行时才出现：`check` 报错、调用时报错，或者读到的形状不一样。

升级 Smelt 后：

```sh
smelt plugin types ~/.smelt/plugins/com.example.notes   # 刷新类型声明
smelt plugin check ~/.smelt/plugins/com.example.notes   # 按新版本校验
smelt plugin status com.example.notes                   # 看实际加载结果
```

类型声明能在编辑器里先看出签名变了的地方。

## 哪些地方会变

| 地方 | 稳定程度 | 说明 |
| --- | --- | --- |
| 清单字段、四种插件、目录约定 | 稳 | |
| 登记函数（`registerViews` 等）、`run`、`onCommand`、`onEvent`、`store` | 稳 | 可能加可选字段 |
| Smelt 组件（`SmeltButton` 等）、主题令牌、图标 | 较稳 | 可能加 props、加令牌、加图标；个别图标可能换掉 |
| 宿主事件 `HostEvent` | 较稳 | 可能加新的事件类型；按 `type` 过滤，不认识的忽略 |
| 宿主状态的整理过的访问器（`machineMeta`、`conversationFacts`、`sessionAttention`、`activeContext`） | 较稳 | 可能加字段、加枚举值 |
| 运行时的原始结构（`workspaceDirectory`、`sessionStates`）、`request` | 会变 | 跟着运行时协议变 |
| Control API（`call`） | 有版本 | 见下 |
| [内部](reference/sdk.md#内部)导出、`smelt/conversation` | 不稳 | 跟着内置功能改，不要依赖 |
| 内置插件的契约（`contract.js` 里的 id 和参数） | 较稳 | id 一般不改，用户的快捷键、布局都认它 |

## 读数据要宽容

为了加字段、加枚举值时不坏：

- 对象里不认识的字段，忽略。
- 枚举值（提醒的 `kind`、状态的种类）遇到不认识的，按「其他」处理，不要抛错、不要整条丢掉。
- `onEvent` 按 `type` 只收自己要的。
- 运行时话题收到 `"unavailable"`，那台机器上这项功能就不可用，不是错误。

## 运行时的版本

插件调的是每台机器上的运行时，远程机器的运行时可能比桌面旧：

- 旧运行时没有你调的方法，`callDetail` 给 `code: "method_not_found"`。
- 旧运行时不推你登记的话题，收到 `"unavailable"`。

据此把这台机器上的这项功能标成不可用，不要让整个插件失败。Control API 自己的版本规则见 [Control API：版本与重试](../control-api.md#版本与重试)。

## 你的插件自己的兼容

### 库

表结构用 `store.migrate` 管，只往后加步骤，见[存储：迁移](storage.md#迁移)。用户装回旧版插件时 `migrate` 会拒绝动更新过的库。

### 对外的 id

视图、命令、页、布局的 id 一旦发出去，就有人在用：用户的快捷键、保存的布局、别的插件的调用都认 id。

- 不要改 id。改标题、改图标都没关系。
- 命令要加参数，加成可选的。

### 覆写的内置插件

覆写版不跟着内置版升级。Smelt 升级后，内置版可能改了表结构、契约、用到的 SDK。对比内置版的改动，必要时重新 fork 一份，见[插件之间：覆写](plugin-interop.md#覆写同-id)。

## 相关

- [调试与测试](debugging.md)
- [`smelt/sdk` 参考](reference/sdk.md)
