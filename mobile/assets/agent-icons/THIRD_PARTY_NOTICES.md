# 第三方图标资源

这些 SVG 与桌面的 `crates/smelt/assets/icons/` 是同一份源文件。两边各存一份是因为
gpui 和 Flutter 的打包器只认自己目录下的文件；声明在这里重复一遍，是为了让移动端
分发物自带完整出处。

新增一家 agent 时两份 bundle 都要放图标，漏了会被 `smelt-core` 的
`every_kind_has_an_icon_in_the_desktop_and_mobile_bundles` 拦住。

`agent-claude.svg`、`agent-codex.svg`、`agent-grok.svg`、`agent-cursor.svg`、
`agent-opencode.svg` 和 `agent-kiro.svg` 来自
[LobeHub Icons](https://github.com/lobehub/lobe-icons) 的静态 SVG 集合，该集合以
MIT License 发布。Kiro 名称与商标归 Amazon / AWS 所有。

`agent-copilot.svg` 使用 GitHub 提供的 `GitHub_Invertocat_Black.svg` 资源，
仅将填充色表达为 `currentColor` 以适配 Smelt 的深浅主题。图标所对应的产品名称
和商标仍归各自权利人所有。

`agent-antigravity.svg` 使用
[Google Antigravity 官方品牌标志](https://www.antigravity.google/press)；仅将填充色
表达为 `currentColor`。Antigravity 名称与商标归 Google 所有。

`agent-pi.svg` 使用 [Pi 官方 logo](https://pi.dev/logo-auto.svg)，Pi 仓库以 MIT License
发布（Copyright (c) 2025 Mario Zechner）；仅将填充色表达为 `currentColor`。Pi 名称
与商标归其权利人所有。

`agent-omp.svg` 改编自 [Oh My Pi 官方图标](https://github.com/can1357/oh-my-pi/blob/main/assets/icon.svg)，
以 MIT License 发布（Copyright (c) 2025 Mario Zechner，Copyright (c) 2025–2026 Can Bölük，
Copyright (c) 2026 Stencil Labs, Inc.）。改为单色、保留插件接头的负形并居中为方形。
Oh My Pi 名称与商标归其权利人所有。

> Permission is hereby granted, free of charge, to any person obtaining a copy of this
> software and associated documentation files (the "Software"), to deal in the Software
> without restriction, including without limitation the rights to use, copy, modify,
> merge, publish, distribute, sublicense, and/or sell copies of the Software, and to
> permit persons to whom the Software is furnished to do so, subject to the following
> conditions:
>
> The above copyright notice and this permission notice shall be included in all copies
> or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
> INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
> PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
> HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
> CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
> OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

`agent-crush.svg` 是 Crush 官方
[`crush-icon-solo.png`](https://github.com/charmbracelet/crush/blob/main/internal/ui/notification/crush-icon-solo.png)
的单色小尺寸改编，保留像素心形角色的轮廓与脸部负形。上游资源
Copyright 2025–2026 Charmbracelet, Inc.，随 Crush 以
[FSL-1.1-MIT](https://github.com/charmbracelet/crush/blob/main/LICENSE.md) 发布。
图形仅用于在 Smelt 中标识对应客户端，Crush 名称与商标归其权利人所有。

`agent-custom.svg`（自定义 ACP Agent 的通用图标）来自 [Lucide](https://lucide.dev/icons/bot)
的 `bot` 图标，以 ISC License 发布，未改动图形。
