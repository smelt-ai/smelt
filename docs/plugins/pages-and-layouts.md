# 页与布局

主窗口一次显示一**页**。每页用一份**布局**来摆：哪些视图放在左、右、下停靠区和中间，怎么分栏、怎么分标签。工作台是一页，设置是另一页。

页和布局都由插件登记。出厂的工作台在内置插件 `smelt.layouts` 里，它不写代码，只有清单、布局文件和文案。

## 怎么路由

- 启动时进**首页**：按登记顺序，第一个 `home: true` 的页。
- 布局里可以摆一个**出口**（`smelt/outlet`），这是这页里换内容的地方。出口没放东西时，显示页的 `index` 视图。
- **打开视图**（`smelt.app/open-view`）时：
  - 哪页的布局里摆了这个视图，就去那页把它露出来；
  - 哪页都没摆，就放进出口（当前页有出口放这页，没有就去首页的出口）。
- **换页**用 `smelt.app/open-page`。页头的关闭回到来的那页。
- 同一页里换出口的内容不算来路。来路只记在内存里，重启后回首页。

## 登记页

在清单里写（布局插件），或者在代码里用 `registerPages`：

```ts
await registerPages([
  { name: "review", title: text("page.review"), index: `${pluginId()}/review-home` },
]);
```

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `name` | `string` | 必填 | 插件内的名字，不能有 `/`。页 id 是 `<插件 id>/<name>` |
| `title` | `string \| Record<string, string>` | 同 `name` | 页头和设置里的名字 |
| `home` | `boolean` | `false` | 是首页。好几个页都说自己是首页时，按登记顺序取第一个 |
| `index` | `string` | 无 | 出口里没放东西时显示的视图 id |

一个页都没有登记时，宿主不拼凑视图，显示空态，引导用户去设置里的「面板插件」。

## 登记布局

```ts
await registerLayouts([
  {
    page: `${pluginId()}/review`,
    name: "default",
    title: text("layout.default"),
    state: REVIEW_LAYOUT, // 对象或 JSON 字符串
    materials: { left: "sidebar" },
  },
]);
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `page` | `string` | 给哪一页，完整页 id。可以给别的插件的页带布局，页不必已经登记 |
| `name` | `string` | 插件内的名字，不能有 `/`。布局 id 是 `<插件 id>/<name>` |
| `title` | `string \| Record<string, string>` | 设置里给人看的名字 |
| `state` | `object \| string` | Dock 布局，格式见下 |
| `materials` | `object` | 各块地方垫什么系统材质，见[材质](#材质) |

页和布局都按插件整组登记：换掉这个插件上次登记的全部，空数组是全撤下；有一项不合法，整组不登记，原来的不动。

**每页用哪份布局**：首页用用户在「外观」里选的那份；别的页用给它登记的第一份。选的那份暂时没登记（插件还没加载、被卸了）时宿主先用别的，不改用户的选择，等它登记上就换回去。

## 布局文件格式

布局是 Dock 区域状态的 JSON，`version` 目前是 `1`。面板名就是视图 id。

```json
{
  "version": 1,
  "center": {
    "panel_name": "StackPanel",
    "info": { "stack": { "sizes": [820], "axis": 0 } },
    "children": [
      {
        "panel_name": "TabPanel",
        "info": { "tabs": { "active_index": 0 } },
        "children": [
          { "panel_name": "smelt/outlet", "children": [], "info": { "panel": null } }
        ]
      }
    ]
  },
  "left_dock": {
    "placement": "left",
    "size": 260.0,
    "open": true,
    "panel": {
      "panel_name": "TabPanel",
      "info": { "tabs": { "active_index": 0 } },
      "children": [
        { "panel_name": "smelt.session-list/main", "children": [], "info": { "panel": null } }
      ]
    }
  }
}
```

| 键 | 说明 |
| --- | --- |
| `center` | 中间那块，必有 |
| `left_dock`、`right_dock`、`bottom_dock` | 停靠区，可选。`placement` 是 `left`/`right`/`bottom`，`size` 是宽（下停靠区是高），`open` 是开着还是收起，`panel` 是里面的面板 |

面板有三种：

| `panel_name` | `info` | 是什么 |
| --- | --- | --- |
| `"StackPanel"` | `{ "stack": { "sizes": [...], "axis": 0 } }` | 分栏。`axis` 为 `0` 时左右排，为 `1` 时上下排；`sizes` 是各栏的大小；`children` 是各栏 |
| `"TabPanel"` | `{ "tabs": { "active_index": 0 } }` | 一组标签。`children` 是各个标签，`active_index` 是选中第几个 |
| 视图 id | `{ "panel": null }` | 叶子：一个视图。`children` 为空 |

除了视图，叶子还可以是两个宿主面板：

- **`smelt/outlet`**：出口，见[怎么路由](#怎么路由)。
- **`smelt/stack`**：一摞，见下。

### 一摞

```json
{
  "panel_name": "smelt/stack",
  "children": [],
  "info": {
    "panel": {
      "axis": "vertical",
      "items": [
        { "view": "smelt.launcher/nav", "fit": true },
        { "view": "smelt.session-list/main" },
        {
          "stack": {
            "axis": "horizontal",
            "items": [
              { "view": "smelt.launcher/settings", "fit": true },
              { "view": "smelt.quota/bar" }
            ]
          },
          "fit": true
        }
      ]
    }
  }
}
```

一摞把几个视图按顺序排在一格里。和分栏的区别：

- 没有分隔条，拖不动，不分标签，不能单独放大；
- 标了 `fit: true` 的项按内容定大小，其余的平分剩下的地方；
- 里面的视图不画外壳的标题带。

适合侧栏这种上中下固定几块的地方。`axis` 是 `"vertical"` 或 `"horizontal"`；一项是 `{ "view": "<视图 id>" }`，或者换个方向再排一摞 `{ "stack": {...} }`。

出口、设置页、多实例视图不能放进一摞。

### 材质

`materials` 说明各块地方垫什么系统材质，键是 `left`、`right`、`bottom`、`center`：

| 值 | 效果 |
| --- | --- |
| `"sidebar"` | 导航侧栏：透得最明显，带桌面的颜色 |
| `"window"` | 窗口底：几乎不透，只隐约带一点桌面的颜色。适合检查器、辅助栏这类次要但常看的地方 |
| 不写 | 不透，铺实的主题底色 |

材质跟着地方走，不跟视图：视图拖到别处，透不透看它落在哪。主题底色按「外观」里的不透明度铺在材质上。Windows 上整扇窗口只有一种系统材质，声明了的地方都透它。

## 用户改过的布局

用户拖动视图、调分栏大小、切标签，宿主都会按布局 id 存档，下次照存档摆。存档只记摆法（分组、顺序、大小、选中），不增减视图。

你发了新版布局，只要摆的视图集合或摞的内容变了，旧存档就不再用，按新的摆。所以只改大小、默认选中这类摆法时，已经动过布局的用户看不到变化。

## 视图在不在布局里

- `viewLaidOut(id)`：当前各页装的布局里摆了这个视图。
- `viewShown(id)`：此刻看得见。

布局里没摆的视图，打开时进出口。见[视图](views.md#视图的状态)。

## 相关

- [目录与清单：布局插件的清单](project-structure.md#布局插件的清单)
- [视图](views.md)
- [宿主命令：open-view、open-page](reference/host-commands.md)
