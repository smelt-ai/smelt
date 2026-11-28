# 写界面

插件的界面用 [NSH](https://nsh.dev/docs/) 写：TSX、细粒度响应式、Tailwind 风格的 `class`，由宿主原生绘制。NSH 本身的元素、样式、布局、列表、动画以 nsh.dev 为准。这一页讲在 Smelt 里写界面要守的规矩、Smelt 加的组件、图标和主题。

## 组件只跑一次

NSH 不是 React。组件函数只在挂上时执行**一次**，之后只有读了 signal 的地方跟着更新，函数本身不会重跑。

```tsx
export function Counter() {
  const count = signal(0);
  console.log("只打一次");
  return (
    <SmeltButton label={`Count: ${count()}`} onPress={() => count.update((n) => n + 1)} />
  );
}
```

`label={...}` 里读了 `count()`，所以只有这个属性跟着变。详见 NSH 的 [Reactivity](https://nsh.dev/docs/reactivity)。

由此有几条规矩：

### 不要解构 props

```tsx
// 错：只拿到第一次的值，之后不更新
function Title({ text }: { text: string }) {
  return <Text>{text}</Text>;
}

// 对：在 JSX 里读 props.text
function Title(props: { text: string }) {
  return <Text>{props.text}</Text>;
}
```

视图的 `props.args` 也一样。

### 文字要在 `<Text>` 里

```tsx
// 错
<View>Hello</View>

// 对
<View><Text>Hello</Text></View>
```

### 条件和列表用 `<If>`、`<Each>`

`If` 和 `Each` 是全局的，不用 import：

```tsx
<If when={note()} else={<Text class="text-muted-foreground">没有选中</Text>}>
  {(n) => <Text>{n().title}</Text>}
</If>

<Each of={notes()} key={(n) => n.id}>
  {(n) => <NoteRow note={n} />}
</Each>
```

子函数拿到的是 accessor（`n()`），不是值。行很多时用 `nsh` 的 `<List>`（虚拟滚动）。

### 订阅要清理

组件里订的事件、定时器，用 `cleanup` 在卸下时取消：

```tsx
export function Panel() {
  const off = onEvent(handle, ["project_changed"]);
  cleanup(off);
  // ...
}
```

### 渲染里不做 IO

调运行时、读库都是异步的，放进 `bootTask`、事件回调、`effect` 或 `resource`；界面只读内存里的 signal。

### 不轮询

不要用 `setInterval` 反复问宿主或运行时。等变化一律用状态切片、[宿主事件](host-state.md#宿主事件)、[运行时话题](runtime.md#运行时话题)、`waitOnline`。要逐帧推进的（拖到边上自动滚动），用 `requestAnimationFrame`，按两帧的时间差算步长，停下就别再要帧。

## Smelt 组件

`smelt/sdk` 导出的组件已经接好了 Smelt 的主题、字号、动效和交互。有对应的 Smelt 组件时，用它，不要直接用 `nsh/base` 的同名组件。

### `SmeltButton`

```tsx
<SmeltButton label="保存" variant="primary" onPress={save} />
<SmeltButton icon="icons/trash.svg" tooltip="删除" variant="quiet" onPress={remove} />
<SmeltButton label="刷新" icon="icons/refresh.svg" size="sm" loading={busy()} onPress={refresh} />
```

颜色由 `variant` 定，几何（高度、内边距、字档、图标）由 `size` 定，`shape` 只改圆角。不接 `style`：颜色、圆角、内边距不能在调用处改，`class` 只放布局（宽度、`flex`、外边距）。

按钮和输入框共用一张尺寸表，同一档并排时一样高；只有图标的按钮见方，边长等于高度。图标跟同档的字配，有字没字一样大，嫌图标小就换大一档，不单调图标。

| 档 | 高 | 字 | 图标 | 用在 |
| --- | --- | --- | --- | --- |
| `xs` | 20 | `small` | 14 | 行里的就地操作（关闭、暂存、加号） |
| `sm` | 24 | `text_xsmall` | 14 | 面板顶栏、工具条 |
| `md` | 28 | `ui` | 16 | 标题栏、表单、对话框 |

| Prop | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `label` | `string` | | 按钮上的字 |
| `icon` | `SmeltIconPath` | | 字前面的图标。只有图标没有字时是方形图标按钮 |
| `trailingIcon` | `SmeltIconPath` | | 字后面的图标 |
| `tooltip` | `string` | | 悬停提示 |
| `command` | `string` | | 命令 id。只用来在提示里显示这条命令的快捷键，**不会**替你执行，执行写在 `onPress` 里 |
| `onPress` | `(e) => void` | | 点了。`disabled`、`loading` 时不调 |
| `variant` | `"secondary"` `"primary"` `"quiet"` `"danger"` `"danger-quiet"` `"floating"` `"dismiss"` | `"secondary"` | `primary` 主按钮，一块界面里最多一个；`quiet` 没有底、悬停才有，工具栏和行内操作用；`danger` 红字；`danger-quiet` 安静的红字；`floating` 浮在内容上（表面色、带投影）；`dismiss` 卡片角上的关闭，最淡的字色 |
| `size` | `"xs"` `"sm"` `"md"` | `"md"` | 见上表 |
| `shape` | `"rounded"` `"pill"` `"circle"` | `"rounded"` | `pill` 两端全圆；`circle` 用在只有图标的按钮上 |
| `fill` | `boolean` | `false` | 撑满宽度 |
| `disabled` | `boolean` | `false` | 不能点 |
| `loading` | `boolean` | `false` | 在忙：不能点，显示转圈 |
| `selected` | `boolean` | `false` | 选中的样子（无障碍上算选中）。`quiet` 按钮选中时有底 |
| `accessibilityLabel` | `string` | `label` 或 `tooltip` | 读屏念的名字。只有图标时一定要给 `tooltip` 或它 |
| `class`、`testID` | | | `class` 只放布局 |
| `children` | `JSX.Element` | | 自定义内容，代替 `label` |

### 行、输入框、分段、菜单项、确认框

| 组件 | 基于 | 用途 |
| --- | --- | --- |
| `SmeltRow` | `Button` | 可点的列表行、树行。键盘可聚焦，悬停 `row_hover`、选中 `row_selected`（只靠底色，不画竖条），箭头光标。行高、内边距、缩进放 `class` / `style`，颜色和圆角归组件。不要用 `<View onPress>` 搭行 |
| `SmeltInput` | `Input` | 文本框：`size` 同按钮的尺寸表（行里就地改名用 `md`，字和行标题一样），`invalid` 红边，`bare` 不画底和边（嵌在已有外框里），`multiline` 多行，`mono` 等宽字；`onFocus` / `onBlur` / `onLayout` 照常转发 |
| `SmeltSegmented` | `ToggleGroup` | 分段控件：`options`（每项可带 `tooltip`）、`value`、`onChange`；`multiple` 时可多选 |
| `SmeltMenuItem`、`SmeltMenuSeparator`、`SmeltMenuLabel` | `MenuItem`、`MenuSeparator` | 菜单项（图标、勾、危险色）、分隔线、分组名。放在 `SmeltMenu` / `SmeltContextMenu` 里 |
| `SmeltConfirmDialog` | `SmeltDialog` | 确认框：标题、说明、取消 + 确认（`danger` 时是危险按钮）。只能按钮或 Esc 关，`busy` 时确认转圈 |

### 浮层

这几个是 `nsh/base` 同名组件包上了 Smelt 的样子和进出动效，props 和 `nsh/base` 的一样（去掉了动效相关的几个）：

| 组件 | 基于 | 用途 |
| --- | --- | --- |
| `SmeltPopover` | `Popover` | 点开的卡片 |
| `SmeltMenu` | `Menu` | 下拉菜单 |
| `SmeltContextMenu` | `ContextMenu` | 右键菜单 |
| `SmeltSelect` | `Select` | 选择框 |
| `SmeltTooltip` | `Tooltip` | 悬停提示 |
| `SmeltDialog` | `Dialog` | 对话框：整层淡入淡出，卡片从下面升 4px，遮罩用主题的 `scrim` |

props 见 NSH 的 [nsh/base](https://nsh.dev/modules/nsh-base)。

```tsx
const open = signal(false);

<SmeltDialog
  open={open()}
  onOpenChange={({ open: next }) => open.set(next)}
  accessibilityLabel="新建笔记"
  class={`m-auto ${DIALOG_SURFACE_CLASS} p-5`}
  style={{ width: 420 }}
>
  {/* ... */}
</SmeltDialog>
```

自己拼浮层时，用这些 class 常量拿到同样的样子：

| 常量 | 样子 |
| --- | --- |
| `POPUP_CLASS` | 弹出卡片（`SmeltPopover` 默认用它） |
| `MENU_CLASS` | 菜单、下拉、选择框的弹出层 |
| `MENU_ITEM_CLASS` | 菜单项 |
| `TOOLTIP_CLASS` | 提示 |
| `INPUT_CLASS` | 输入框 |
| `DIALOG_SURFACE_CLASS` | 对话框的卡片 |

### 展开收起

`SmeltDisclosure` 是 `nsh/base` 的 `Collapsible` 带上 Smelt 的展开动效；`SmeltDisclosureChevron` 是配套的箭头，`open` 时转下来。标题和内容的间距写成内容的上内边距。

### 内容

| 组件 | 说明 |
| --- | --- |
| `SmeltMarkdown` | 渲染 Markdown。`markdown` 是源文；`machine`、`cwd` 用来解析里面的文件链接，点了交给[文件打开者](commands.md#打开文件)；`streaming` 在内容还在往后长时打开；`selectable` 能选中复制 |
| `SmeltEditor` | 代码编辑器：`value`/`defaultValue`、`language`、`readOnly`、`lineNumbers`、`softWrap`、`folding`、`onChange({ value })`、`onSelectionChange`、`onContextMenu` 等。`editorRef` 拿到命令式接口 |
| `SmeltDiffView` | 显示差异：给 `patch`，或给 `oldValue` 和 `newValue`。`layout` 是 `"split"` 或 `"unified"`，`inline` 定行内高亮的粒度，可以在行上加注释（`annotations` + `renderAnnotation`） |
| `SmeltVector` | 画一段 SVG 源码：`markup`、`width`、`height`。自带图标里没有的图形用它 |
| `SmeltWindowBar` | 自己画顶栏的视图用它让出红绿灯，见[视图](views.md#自己画顶栏) |
| `SmeltPageScroll` | 居中限宽（默认 720pt）的滚动页骨架，设置页与说明页共用；传 `scrollKey` 可在切页时自动回顶 |
| `SmeltSection` | 分组卡片：可选标题/说明 + 主题描边卡片（不做圆角裁剪以免阻断 GPU 图块缓存） |
| `SmeltSectionDivider` | 卡片内左右缩进 16pt 的 1pt 分隔线 |
| `SmeltSectionCell` | `SmeltSection` 内标准单元格（左右 16pt、上下 8pt），`divider` 为真时在上方加分隔线 |
| `SmeltSettingRow` | 设置行：左侧标题/说明/附注/错误，右侧放控件 |
| `ViewInstance` | 嵌一个登记过的视图，见[视图](views.md#嵌入别的视图) |

`SmeltNative*` 开头的几个（终端、桌面、头像等）是给内置插件用的原生组件，props 跟着内置插件变，用户插件不要依赖。

## 图标

```tsx
<SmeltIcon path="icons/search.svg" size={14} />
<SmeltIcon path="icons/loader.svg" size={14} spin />
```

| Prop | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `path` | `SmeltIconPath` | | 图标路径 |
| `size` | `number` | | 边长 |
| `color` | `string` | 当前字色 | |
| `opacity` | `number` | | |
| `rotate` | `number` | | 旋转角度 |
| `spin` | `boolean` | `false` | 一直转，表示在忙 |

只能用 Smelt 自带的图标，路径是 `icons/<名字>.svg`。类型 `SmeltIconPath` 列出了全部，编辑器里能补全。线条风格统一，粗细一致。智能体的标志在 `smelt-icons/agent-<id>.svg`，一般从 `conversationFacts()` 的 `agent.icon` 拿，不要自己拼。

插件目录里的图片文件宿主不加载。要的图形自带图标里没有，用 `SmeltVector` 画 SVG 源码。

## 主题

### 主题令牌

`class` 里写颜色用主题令牌，换主题、换深浅色时自动跟着变：

| 令牌 | 用在 |
| --- | --- |
| `background` / `foreground` | 视图的底和正文 |
| `card` / `card-foreground` | 浮起的面 |
| `popover` / `popover-foreground` | 浮层 |
| `primary` / `primary-foreground` | 强调的底和上面的字 |
| `secondary` / `secondary-foreground` | 次要的底 |
| `muted` / `muted-foreground` | 弱化的底；次要文字 |
| `faint` | 最淡的文字：占位、提示 |
| `accent` / `accent-foreground` | 悬停的行 |
| `row-selected` | 选中的行 |
| `soft` / `soft-hover` | 柔和的底和它的悬停 |
| `border` / `input` / `ring` | 分隔线；输入框边；焦点圈 |
| `link` | 链接 |
| `run` / `need` / `ok` / `bad` | 状态色：在跑、等人、成功、失败或危险 |
| `destructive` | 同 `bad` |
| `on-bad` | `bad` 底上的字 |
| `scrim` | 遮罩 |
| `sidebar`、`sidebar-foreground`、`sidebar-accent`、`sidebar-border` | 侧栏 |
| `chart-1` … `chart-5` | 图表的分类色 |

写法和 Tailwind 一样：`bg-background`、`text-muted-foreground`、`border-border`、`text-bad`、`bg-row-selected`。完整清单在插件的 `.nsh/types/nsh/tokens.d.ts`。

阴影用 `shadow-<档>`，跟主题走：主题 `elevation` 的每一档都有类名（键里的下划线换连字符，`shadow-raised`、`shadow-floating`、`shadow-overlay`、`shadow-md`、`shadow-switch-on`……）；按钮另用 `shadow-button` / `shadow-button-primary` / `shadow-button-floating`，宿主已按底色透不透换好。

圆角也可在 `class` 里直接引用主题令牌：`rounded-(--radius-xs)`、`rounded-(--radius-sm)`、`rounded-(--radius-md)`、`rounded-(--radius-lg)`、`rounded-(--radius-xl)`、`rounded-(--radius-row)`。

> [!IMPORTANT]
> 不要写死颜色（`#3a6ee1`、`text-blue-500`）。写死的颜色在别的主题、深色模式下会对不上，内置插件也一律不写。

### 在代码里读主题

`class` 表达不了的（画布、计算出来的样式），读 `theme()`：

```ts
const t = theme();
t.palette.text;              // "#1a1a18"
t.palette.row_selected;      // "rgba(...)"
t.colors.depth_top;          // 主题文件里的任意颜色键
t.colors.success.bg;         // 状态色四件套 bg / fg / border / icon
t.elevation.floating;        // [{ x, y, blur, spread, color, inset }]
t.type.ui.size;              // 正文字号
t.radius.md;                 // 圆角
```

主题有的，`theme()` 里全有：宿主按 smelt-ui 的主题结构整份导出，不挑字段，主题加一个位插件就能读到，不用改宿主。

| 字段 | 内容 |
| --- | --- |
| `palette` | 语义色板，画界面先用它：底色（`bg`、`side`、`surface`、`soft*`）、字（`text`、`muted`、`faint`，选中行上的 `*_selected`）、行与线（`row_hover`、`row_selected`、`hair`、`hair_strong`、`focus`、`selection`）、主色（`ink*`、`on_ink`、`accent`、`on_accent`、`link`）、状态（`need`、`ok`、`bad`、`on_bad`）、开关（`track`、`knob`、`knob_on`）、`popover_text`、`scrim`、`chart[5]`，按钮三态 `button_primary` / `button_secondary` / `button_submit`（`[常态, 悬停, 按下]`）和字色 `*_foreground` |
| `colors` | 主题文件里的每个颜色键，键名同[主题](themes.md)：色板没有的位从这里取（`depth_top`、`layer_01`、`border_strong`……）；`success` / `warning` / `danger` / `info` 是 `{ bg, fg, border, icon }` |
| `elevation` | 每一档投影，`boxShadow` 数组，可直接放进 `style`；同一档也有 `shadow-<档>` 类名 |
| `type` | 每一档字：`ui`、`ui_medium`、`ui_compact`、`small`、`small_medium`、`body`、`body_medium`、`text_small`、`text_small_medium`、`text_xsmall`、`text_xsmall_medium`、`mono`，每档 `{ size, line_height, weight }`，已按窗口缩放算好 |
| `radius` | 圆角：`xs`、`sm`、`md`、`lg`、`xl`，列表行 `row` |
| `fonts.mono` | 等宽字体 |
| `rem` | 当前的 1rem 是多少像素 |

`theme()` 是响应式的，在 `computed`、JSX 里读会跟着主题变。

## 文案

- 少写字：按钮用动词，标题用名词，不加「请」「您」，不加感叹号。
- 状态用颜色和图标表达，不用 emoji。
- 失败在发起的地方就地说（按钮下面、那一行里），不弹全局提示。没有落点的才发系统通知（`smelt.app/notify`）。
- 文字都走 [`t()`](i18n.md)，不写死在代码里。

更多见 [UI 设计语言](../ui-design-language.md)。

## 常见错误

| 错 | 对 |
| --- | --- |
| `function View({ args })` | `function View(props)`，读 `props.args` |
| `<View>{name}</View>` | `<View><Text>{name}</Text></View>` |
| `{items().map(...)}` | `<Each of={items()} key={...}>` |
| `setInterval(() => reload(), 2000)` | `onEvent(reload, ["sessions_changed"])` 或运行时话题 |
| 组件里 `onEvent(...)` 不取消 | `cleanup(onEvent(...))` |
| `class="text-gray-500"`、`color: "#888"` | `class="text-muted-foreground"`、`theme().palette.muted` |
| `<SmeltButton command="x/y" />` 指望它执行 | `onPress={() => run("x/y")}` |
| 组件函数里 `await call(...)` | 放进 `bootTask`、事件回调或 `resource` |
| `width: "100%"` 同时写左右 `margin` | 省掉 `width: "100%"`（列容器默认拉伸），或把左右留白放到父容器 `paddingHorizontal` |
| 在 `<Scroll>` 上设 `borderRadius`，或用圆角 + `overflow: "hidden"` 包 `<Scroll>`/`<List>`/`<SmeltDiffView>` | 圆角底色放在外层容器、不加 `overflow: "hidden"`，避免圆角裁剪整棵子树阻断 GPU 图块缓存 |
| `SmeltDisclosure` 的 `content` 根节点写 `margin` | 改写 `padding`，确保展开高度动画量到的尺寸与视觉一致 |

## 相关

- NSH：[JSX and components](https://nsh.dev/docs/jsx-and-components)、[Styling](https://nsh.dev/docs/styling)、[Lists](https://nsh.dev/docs/lists)
- [主题插件](themes.md)
- [多语言](i18n.md)
