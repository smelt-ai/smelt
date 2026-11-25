# 调试与测试

## 先跑 `check`

```sh
smelt plugin check ~/.smelt/plugins/com.example.notes
```

不用开 App，做一遍加载时的检查：清单、文案、主题、打包、import 白名单、没声明的全局变量、编字节码。大多数错误在这里就能看到。见[命令行：check](cli.md#check)。

## 再看 `status`

`check` 过了，App 里不一定加载上了：启动时会报错、和别的插件撞了 id。保存后等一两秒：

```sh
smelt plugin status com.example.notes
```

`error` 是 `null`、`views` 里有你的视图，才算加载好了。插件有错误时退出码是 1，可以放进脚本里。见[命令行：status](cli.md#status)。

「设置 → 面板插件」里也能看到每个插件的状态和错误，有「打开日志」「打开插件目录」。

## 看日志

`console.log`、`console.warn`、`console.error` 和未捕获的异常都写进 `~/.smelt/app.log`：

```sh
tail -f ~/.smelt/app.log | grep '\[plugin\]'
```

```
[2026-11-25 10:42:07.311] [info] [plugin] notes: loaded 12 notes (pid=4821)
```

- 插件来源一律记作 `[plugin]`，行里**不带插件 id**。自己打的日志加个前缀，方便过滤。
- NSH 运行时本身的警告和错误也记作 `[plugin]`。
- 日志超过上限就滚到 `app.log.1`，只留一代。
- 对象按 JSON 打出来，错误带调用栈。

> [!TIP]
> `console.debug` 也会写进去。别在逐帧或每次渲染都跑的地方打日志，日志文件会很快滚掉。

## 常见问题

| 现象 | 多半是 |
| --- | --- |
| `check` 报 import 不在白名单 | import 了 Node 模块（`fs`、`path`）或别的插件目录里的文件。见[介绍：能 import 什么](introduction.md#能-import-什么) |
| `check` 报没声明的全局变量 | 用了 `process`、`Buffer`、`require` 这类 Node 全局 |
| 视图那块显示「出错了」 | 组件抛错。去日志找调用栈，见[视图：渲染出错](views.md#渲染出错) |
| 界面不跟着数据变 | 解构了 props，或者在组件函数顶层读了一次值存进普通变量。见[写界面](ui.md#组件只跑一次) |
| 视图迟迟不出现 | `bootTask` 里的事没做完。宿主最多等 3 秒就挂视图 |
| 启动时该做的事没做 | `bootTask` 里抛了错。启动任务失败只记一条 `bootTask failed` 日志，插件照样算就绪 |
| 命令调不到 | 命令没登记、名字里带了插件 id 前缀、`smelt app run` 时没标 `cli: true`。见[命令](commands.md) |
| 快捷键不灵 | 和别的插件或保留键撞了，后登记的不绑。见[命令：快捷键](commands.md#快捷键) |
| 改了没反应 | 改的是被忽略的文件（`tests/`、`.test.`、点开头的），或者改了仓库里的内置插件但用的是装好的 App |
| 覆写的内置插件没生效 | 覆写版加载失败，退回了内置版。`status` 里 `fallback_to_builtin` 是 `true` |
| `migrate` 报库比插件新 | 装了旧版插件。见[存储：库比插件新](storage.md#库比插件新) |
| 升级 Smelt 后类型对不上 | 重跑 `smelt plugin types <目录>` |

## 单元测试

界面不好测，把逻辑拆出来测。内置插件的做法：

- 数据、规则写在**不 import 宿主模块**的纯 JS/TS 文件里（不 import `nsh`、`smelt/sdk`），要用的宿主函数从参数传进来；
- 测试放 `tests/` 下，文件名带 `.test.`，用 [Bun](https://bun.sh) 跑。

```js
// tests/model.test.mjs
import { expect, test } from "bun:test";
import { sortNotes } from "../model.js";

test("pinned notes come first", () => {
  const notes = [{ id: 1, pinned: 0 }, { id: 2, pinned: 1 }];
  expect(sortNotes(notes).map((n) => n.id)).toEqual([2, 1]);
});
```

```sh
bun test ~/.smelt/plugins/com.example.notes/tests
```

`tests/` 和 `.test.` 文件不打包进插件、改了也不触发重载。内置插件的例子：[`plugins/quota/tests/model.test.mjs`](../../plugins/quota/tests/model.test.mjs)。

## 改内置插件

在仓库里改 `plugins/` 下的内置插件，要用开发构建的 App 才能看到：开发构建读仓库的 `plugins/` 并热重载，装好的 `.app` 读它自己包里的那份。

## 相关

- [命令行](cli.md)
- [写界面：常见错误](ui.md#常见错误)
