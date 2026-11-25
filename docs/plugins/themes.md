# 主题插件

只想换整个 App 的配色，不用写代码：做一个**主题插件**，清单的 `entry` 指向一份主题 JSON。脚本插件也可以在 `themes/` 下顺带几份主题。

## 起一个主题

```sh
smelt plugin init me.sunset --theme --name "Sunset"
smelt plugin check ~/.smelt/plugins/me.sunset
smelt settings set appearance.theme="Sunset Dark"
```

`init --theme` 生成：

```
me.sunset/
  smelt-plugin.json    {"id":"me.sunset","name":"Sunset","version":"0.1.0","entry":"themes/theme.json"}
  themes/theme.json    一浅一深两个主题：Sunset Light、Sunset Dark
```

也可以在「设置 → 外观」里选。改了 JSON 保存，正用着这个主题的话界面马上重新上色。

## 主题文件

```json
{
  "version": 1,
  "name": "Sunset",
  "author": "Your Name",
  "url": "https://example.com",
  "themes": [
    {
      "name": "Sunset Dark",
      "mode": "dark",
      "colors": {
        "bg_base": "#16171a",
        "text_base": "#e6e7eb",
        "bg_accent": "#e07a5f",
        "success.fg": "#6fc276"
      },
      "elevation": {
        "floating": [{ "y": 4, "blur": 8, "color": "#0000004d" }]
      },
      "syntax": {
        "keyword": { "color": "#f08d6c", "font_weight": 600 },
        "comment": { "color": "#7b818c", "font_style": "italic" }
      }
    }
  ]
}
```

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `version` | 是 | 格式版本，现在只有 `1` |
| `name` | 是 | 系列名 |
| `author`、`url` | 否 | 作者、主页 |
| `themes[].name` | 是 | 主题名，设置里显示的就是它，`appearance.theme` 填的也是它 |
| `themes[].mode` | 是 | `"light"` 或 `"dark"` |
| `themes[].colors` | 是 | 颜色，见下 |
| `themes[].elevation` | 否 | 投影 |
| `themes[].syntax` | 否 | 代码高亮 |

一个系列里一浅一深最好：用户让 App 跟着系统切深浅色时，在同一系列里找另一档。

## 颜色

只有 `bg_base`（主内容的底）和 `text_base`（正文）必填，而且不能带透明度。没写的颜色在 OKLab 色彩空间里从已有的颜色推出来，所以：

- 只写这两个，得到一套能用的主题；
- 再加一个 `bg_accent`，强调色就有了；
- 哪个推出来的不满意，单独写它，下游跟着它推的颜色也跟着变。

颜色写成 `#rgb`、`#rgba`、`#rrggbb`、`#rrggbbaa`。

| 组 | 键 |
| --- | --- |
| 底与层级 | `page`（侧栏所在的外层）、`bg_base`、`bg_deep`、`layer_01`–`layer_04`、`surface_raised`（弹层、菜单）、`bg_inverse`、`bg_contrast`、`bg_button_neutral`、`bg_icon_button_contrast`、`bg_accent` |
| 文字 | `text_base`、`text_muted`、`text_faint`、`text_inverse`、`text_contrast`、`text_accent` |
| 图标 | `icon_base`、`icon_muted`、`icon_faint`、`icon_inverse`、`icon_contrast`、`icon_accent` |
| 边与交互 | `border_muted`、`border_base`、`border_strong`、`border_focus`、`hover`、`pressed`、`contrast_hover`、`contrast_pressed`、`selection`、`scrim` |
| 发送按钮 | `submit_highlight`、`submit_hover`、`submit_fg`、`submit_icon` |
| 开关 | `knob`、`knob_on` |
| 其他 | `depth_top`、`depth_bottom`、`white`、`black` |
| 状态 | `success.bg`、`success.fg`、`success.border`、`success.icon`，`warning.*`、`danger.*`、`info.*` 同样四个 |
| 图表 | `chart.1`–`chart.5` |

每个颜色怎么推、用在哪，见 [UI 设计语言：颜色](../ui-design-language.md#2--颜色)。

## 投影

`elevation` 的键：`raised`、`floating`、`overlay`、`md`、`button_neutral`、`button_contrast`、`elements`、`switch_off`、`switch_on`、`knob`、`input`。

每个键是一组投影，从上到下叠：

```json
"overlay": [
  { "y": 12, "blur": 32, "color": "#00000033" },
  { "spread": 1, "color": "#ffffff14", "inset": true }
]
```

`x`、`y`、`blur`、`spread` 不写是 0，`inset` 不写是 `false`，`color` 必填。`[]` 是不要投影。没写的键用默认。

## 代码高亮

`syntax` 的键是 tree-sitter 的高亮名：`attribute`、`boolean`、`comment`、`comment.doc`、`constant`、`constructor`、`function`、`keyword`、`label`、`number`、`operator`、`property`、`punctuation`、`string`、`string.escape`、`tag`、`text.code.span`、`text.literal`、`title`、`type`、`variable`、`variant` 等。

每项 `{ color?, font_style?, font_weight? }`：`font_style` 是 `"normal"`、`"italic"`、`"underline"`；`font_weight` 是 100 到 900 的整百。

不写 `syntax`，用同深浅的内置高亮。写了就**整套换掉**：没写的名字不再上色，不会和内置的合并。

## 校验

校验是严格的，一份插件的所有主题文件一起登记：

- `version` 不是 1、有不认识的键、颜色格式不对、种子色带透明度、类型不对；
- 主题名为空，或同一插件里重名；
- 用了保留名 `smelt Light`、`smelt Dark`，或和别的插件的主题重名。

有一条不对，这个插件的主题**整组不生效**，之前登记的那份原样留着。`smelt plugin check` 会把错误报出来。

## 脚本插件带主题

脚本插件在目录里放 `themes/*.json`，加载时一起登记，格式同上。主题插件除了 `entry` 指的那份，`themes/` 下的其他 `.json` 也会登记。

插件的界面怎么跟着主题变，见[写界面：主题](ui.md#主题)。
