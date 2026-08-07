# 构建机脚本

按平台分目录，目前有 `mac`（桌面端，Rust + GPUI）、`ios` 和 `android`（移动端，
Flutter）。每个平台两个脚本：`env-setup.sh` 准备环境，`ci.sh` 执行构建。

```
./.ci/mac/env-setup.sh     && ./.ci/mac/ci.sh
./.ci/ios/env-setup.sh     && ./.ci/ios/ci.sh
./.ci/android/env-setup.sh && ./.ci/android/ci.sh
```

`env-setup.sh` 检查工具版本，不达标的就地升级，最后把这次运行需要的环境变量写进
`.ci/.env/<平台>.sh`（已 gitignore）。`ci.sh` source 它再干活。加 `--check` 可以
只检查不安装，不达标时退出码非 0。

两边的工具链共用 `~/.smelt-ci/toolchains`（可用 `SMELT_CI_TOOLCHAIN_ROOT` 改），
各占各的子目录，同一台机器上可以并存。

## 各平台入口

| 命令 | 做什么 |
| --- | --- |
| `./.ci/mac/ci.sh` | 编译 + 测试整个 Rust 工作区 |
| `./.ci/mac/ci.sh build` / `test` | 只做其中一步 |
| `./.ci/mac/ci.sh package` | 编 release 并打 dmg，产物在 `dist/` |
| `./.ci/ios/ci.sh` | 静态分析 + 单元测试 |
| `./.ci/ios/ci.sh analyze` / `test` / `build` | 只做其中一步 |
| `./.ci/ios/ci.sh all` | 以上全部 |
| `./.ci/android/ci.sh` | 静态分析 + 单元测试 |
| `./.ci/android/ci.sh analyze` / `test` / `build` | 只做其中一步 |
| `./.ci/android/ci.sh bundle` | 编 release AAB（上架 Google Play 用） |
| `./.ci/android/ci.sh all` | analyze + test + build |

`.ci/ios/ci.sh build` 默认不签名，只验证能不能编过。要出可分发的 `.ipa`，把
`SMELT_IOS_EXPORT_OPTIONS` 指向 exportOptions.plist。

`.ci/android/ci.sh build` 同理：不设 `SMELT_ANDROID_KEYSTORE` 时用 debug 签名，
产物能装但不可分发。要出正式包，设 `SMELT_ANDROID_KEYSTORE` 及配套的
`SMELT_ANDROID_KEYSTORE_PASSWORD` / `SMELT_ANDROID_KEY_ALIAS` /
`SMELT_ANDROID_KEY_PASSWORD`，脚本会写临时的 `mobile/android/key.properties`，
构建结束即删除。

Android 默认只编 arm64，多一个 ABI 就多一遍 Rust 依赖树的交叉编译；要多 ABI 时
设 `SMELT_ANDROID_ABIS`（逗号分隔，如 `android-arm64,android-arm`）。

## 构建机上的约束

这些机器是 UP 发布系统的构建机，多项目共用且没有 sudo。脚本据此守三条规矩：

1. **不往系统目录写。** 工具链都装在 `$SMELT_CI_TOOLCHAIN_ROOT` 下。装在 `$HOME`
   而不是工作区里，是因为编译 GPUI 那堆 git 依赖、下载 Flutter 引擎产物都很慢，
   跨次构建复用能省大量时间。
2. **全局配置只留在当前运行上下文里。** 不写 `~/.bashrc` 之类，不碰
   `git config --global`，不跑 `flutter config`（那会写 `~/.flutter`）。所有设置
   都以环境变量形式写进 `.ci/.env/`，进程退出即失效。
3. **不复用机器上已有的** `~/.cargo`、`~/.rustup`、`~/.pub-cache`、`~/.gem`。那些
   可能是管理员或别的项目在用的。

装不了的东西（Xcode、Xcode CLT、Python 3.10+、JDK）脚本只检查并报出要管理员执行
的具体命令。Android 侧还多一条：Gradle 9.1 只接受 JDK 17~25，构建机上如果只有更
新的 JDK，`env-setup.sh` 会直接报错而不是让 Gradle 抛一句难懂的
"Unsupported class file major version"；可用 `SMELT_ANDROID_JAVA_HOME` 指定。

## 内网镜像

连不上外网时，用环境变量指向镜像，`env-setup.sh` 会用它们下载并把相关项透传给
`.ci/` 下的 `ci.sh`：

- mac：`RUSTUP_UPDATE_ROOT`、`RUSTUP_DIST_SERVER`、`NEXTEST_DOWNLOAD_URL`、
  `CARGO_REGISTRY_MIRROR`
- ios：`FLUTTER_GIT_URL`、`FLUTTER_STORAGE_BASE_URL`、`PUB_HOSTED_URL`、
  `GEM_SOURCE_URL`、`COCOAPODS_CDN_URL`
- android：`FLUTTER_GIT_URL`、`FLUTTER_STORAGE_BASE_URL`、`PUB_HOSTED_URL`、
  `RUSTUP_UPDATE_ROOT`、`RUSTUP_DIST_SERVER`、`CARGO_REGISTRY_MIRROR`、
  `ANDROID_SDK_URL_BASE`、`ANDROID_CMDLINE_TOOLS_URL`

## 检出路径要短

smeltd 的测试要在 `<仓库根>/target/` 下 bind unix socket，macOS 限制整条路径不超过
103 字符，算下来仓库根目录不能超过 53 字符。超了会有十来个测试报
`path must be shorter than SUN_LEN`。`.ci/mac/ci.sh` 会在跑测试前拦下来。

工作区建议放在类似 `/Users/ci/w/smelt` 的短路径下。
