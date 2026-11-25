# 存储

每个插件有一个自己的 SQLite 库，在 `~/.smelt/plugin-data/<插件 id>/store.sqlite3`。用 SDK 的 `store` 读写，SQL 在宿主的后台线程上跑，不卡界面。

```ts
import { bootTask, store } from "smelt/sdk";

bootTask(async () => {
  await store.migrate([
    `CREATE TABLE notes (
       id INTEGER PRIMARY KEY,
       title TEXT NOT NULL,
       body TEXT NOT NULL DEFAULT '',
       updated_at INTEGER NOT NULL
     );`,
  ]);
});

const notes = await store.query("SELECT id, title FROM notes ORDER BY updated_at DESC");
await store.execute("UPDATE notes SET title = ? WHERE id = ?", ["New title", 3]);
```

插件只能碰自己的库：插件 id 由宿主定死，换不了别人的。

## `store`

| 函数 | 结果 | 说明 |
| --- | --- | --- |
| `query(sql, params?)` | `Promise<object[]>` | 每行一个对象，键是列名 |
| `execute(sql, params?)` | `Promise<number>` | 执行一条语句，结果是改了几行 |
| `write(statements)` | `Promise<number[]>` | 几条语句放进一个事务，全成或全不成，结果是每条改了几行 |
| `migrate(steps)` | `Promise<void>` | 把表结构迁到最新，见下 |

同一个插件的调用严格按发出的顺序执行：先 `execute` 再 `query`，读到的一定是写过的。不同插件之间互不等待。

### 参数和结果

`params` 是数组，对应 SQL 里的 `?`。

| JS 参数 | 存成 |
| --- | --- |
| `string` | `TEXT` |
| 整数（安全整数范围内） | `INTEGER` |
| 别的 `number` | `REAL` |
| `boolean` | `INTEGER`，`1` 或 `0` |
| `null` | `NULL` |

对象、数组、`undefined` 不能当参数。要存结构化的东西，拆成列存；不要把一整个 JSON 塞进一列。

读出来的值：`INTEGER` 和 `REAL` 是 `number`，`TEXT` 是 `string`，`NULL` 是 `null`。布尔读出来是 `0`/`1`，自己转。查到 `BLOB` 列会报错，插件的库里不要放二进制。

### 事务

`execute` 每条自己提交。要几条一起成功，用 `write`：

```ts
await store.write([
  { sql: "DELETE FROM tags WHERE note_id = ?", params: [id] },
  { sql: "DELETE FROM notes WHERE id = ?", params: [id] },
]);
```

## 迁移

`migrate(steps)` 的规则：

- `steps[i]` 把库从版本 `i` 迁到 `i + 1`，空库是版本 0。版本号记在库的 `PRAGMA user_version` 里。
- 已经跑过的步骤不再跑。每一步和它的版本号在同一个事务里，失败就停在上一个版本，Promise 失败。
- 一步里可以有好几条语句，用 `;` 隔开。

所以：

- **只往数组后面加**，已经发出去的步骤不要改，也不要删。改了的步骤在已经迁过的用户那里不会再跑，两边的表就对不上了。
- 一个版本的插件只调一次 `migrate`，放在 `bootTask` 里，在别的读写之前。

```ts
await store.migrate([
  // 1
  "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
  // 2：后来加的
  "ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;",
]);
```

### 库比插件新

用户装回了旧版插件，库的版本比 `steps` 的长度大，`migrate` 报错，不动这个库：旧代码读不懂新表。错误信息是 `... is version 3, newer than this plugin (it only knows up to 2)`。

这时插件应该把错误显示出来，不要自己删库重建。用户换回新版插件就好了。

## 数据跟着插件 id 走

- 库按插件 id 放。插件改了 id，就是一个新的空库。
- 删掉插件，库还在；装回来接着用。
- [覆写](plugin-interop.md#覆写和-fork-内置插件)内置插件时，覆写版和内置版用**同一个库**。覆写版改了表结构，切回内置版可能就读不了了。改之前先备份：

  ```sh
  sqlite3 ~/.smelt/plugin-data/<id>/store.sqlite3 ".backup /tmp/<id>.sqlite3"
  ```

## 存什么

- 插件自己的东西：用户的设置、置顶、排序、缓存。
- 运行时已经有的（会话、项目、自动化）不要再存一份，从运行时读。存两份就会对不上。
- 只存一处：同一份数据不要既存库又存别处。

## 数据目录

`dataDir()` 给出 `~/.smelt/plugin-data/<插件 id>/` 的绝对路径。插件没有文件系统接口，不能直接读写这个目录；它的用处是给要路径的地方（比如 `revealPath` 打开给用户看）。

## 相关

- [第一个插件：加一条命令，存进 SQLite](getting-started.md#加一条命令存进-sqlite)
- [版本与兼容](compatibility.md)
