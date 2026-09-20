# 第三方图标资源

这些 SVG 与桌面的 `crates/smelt/assets/icons/` 是同一份源文件。两边各存一份是因为
gpui 和 Flutter 的打包器只认自己目录下的文件；声明在这里重复一遍，是为了让移动端
分发物自带完整出处。

新增一家 agent 时两份 bundle 都要放图标，漏了会被 `smelt-core` 的
`every_kind_has_an_icon_in_the_desktop_and_mobile_bundles` 拦住。

`agent-claude.svg`、`agent-codex.svg`、`agent-grok.svg`、`agent-cursor.svg`、
`agent-opencode.svg`、`agent-kiro.svg` 和 `agent-dsh.svg` 来自
[LobeHub Icons](https://github.com/lobehub/lobe-icons) 的静态 SVG 集合，该集合以
MIT License 发布。DeepSeek 名称与商标归 DeepSeek 所有；Kiro 名称与商标归
Amazon / AWS 所有。

`agent-copilot.svg` 使用 GitHub 提供的 `GitHub_Invertocat_Black.svg` 资源，
仅将填充色表达为 `currentColor` 以适配 Smelt 的深浅主题。图标所对应的产品名称
和商标仍归各自权利人所有。

`agent-antigravity.svg` 使用
[Google Antigravity 官方品牌标志](https://www.antigravity.google/press)；仅将填充色
表达为 `currentColor`。Antigravity 名称与商标归 Google 所有。

`agent-pi.svg` 使用 [Pi 官方 logo](https://pi.dev/logo-auto.svg)，Pi 仓库以 MIT License
发布（Copyright (c) 2025 Mario Zechner）；仅将填充色表达为 `currentColor`。Pi 名称
与商标归其权利人所有。

`agent-crush.svg` 是 Crush 官方
[`crush-icon-solo.png`](https://github.com/charmbracelet/crush/blob/main/internal/ui/notification/crush-icon-solo.png)
的单色小尺寸改编，保留像素心形角色的轮廓与脸部负形。上游资源
Copyright 2025–2026 Charmbracelet, Inc.，随 Crush 以
[FSL-1.1-MIT](https://github.com/charmbracelet/crush/blob/main/LICENSE.md) 发布。
图形仅用于在 Smelt 中标识对应客户端，Crush 名称与商标归其权利人所有。
