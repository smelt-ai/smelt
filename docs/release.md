# 发版

推 `v*` tag 触发 `.github/workflows/release.yml`，产物都传到同一个 GitHub Release（iOS 例外，传到 TestFlight）：

| 产物 | job | 说明 |
|---|---|---|
| `Smelt.dmg` | `build`（macos-14） | `make dist-build`，用 secrets 里的自签名证书签名（`CODESIGN_P12_*`） |
| `smeltd-<版本>-linux-{x86_64,aarch64}.tar.gz`、`install.sh` | `linux` → `linux-release` | 两种架构各自原生构建，见 `docs/archive/remote-runtime.html` 的 D7 |
| `Smelt-<版本>-android-arm64.apk` | `android` → `android-release` | 正式签名（`SMELT_ANDROID_*` secrets），没配签名直接失败 |
| `smelt-<版本>-windows-x86_64-setup.exe` | `windows` → `windows-release` | Inno Setup 安装程序，`smelt.exe`、`smelt.com`、`smeltd.exe`、`smelt-notify.exe`、`smelt-pi.exe` 装在同一个目录。第一版不签名，见下面的「Windows」 |
| iOS 构建（TestFlight） | `ios`（macos-26） | 传到 App Store Connect，处理完自动发给 TestFlight 内部测试组，不挂 Release。签名见下面的「iOS 签名」 |
| `SHA256SUMS` | `linux-release` → `checksums` | 先只含 Linux 包（一键安装和运行时自动升级马上能用），全部产物到齐后重写成覆盖所有产物 |

## 发版步骤

版本号只在 tag 上，代码里不写（`Cargo.toml` 固定 `0.0.0`）：打包时从 tag 算出版本号编进程序，见下面的「版本号」。
发版就是打 tag，统一用 `scripts/release.sh` 取号：

```sh
scripts/release.sh --rc        # 预发布：2026.1023.0-rc.1，已有进行中的 rc 就 rc.2、rc.3…
scripts/release.sh             # 正式版：有进行中的 rc 就去掉 -rc.N 发它，否则取今天的新号
scripts/release.sh --dry-run   # 只看会取到哪个号
```

脚本在本地 `main` 的当前提交上打 tag，不改文件、不提交，最后打印推送命令，确认后自己推：本地 `main` 和
GitHub 一致时只推 tag（`git push <远端> v<版本>`）；本地领先时连 `main` 一起推（`git push --atomic <远端> main v<版本>`），
直接推 `main` 会绕过 PR 和 `ci-ok` 检查，需要管理员权限。发版流水线自己会完整构建一遍。

tag 带 `-` 后缀就发成预发布：不会成为 Latest，Linux 一键安装默认也不会装它（要装得设 `SMELT_VERSION`）。

## 发布说明

由 `build` job 的「Release notes」步骤生成，不用 GitHub 默认的起点（它往回找上一个正式版，而 v0.6.6–v0.6.14
不在现在 main 的历史上，会一路退回 v0.6.5，把早就发过的 PR 再列一遍）。起点按版本号往前找第一个是本次祖先的 tag：
预发布从上一个 tag（含 rc）算起，正式版从上一个正式版算起，所以正式版的说明涵盖它之前所有 rc 的改动。

## 版本号

按日期取号，格式 `YYYY.MDD.N`：

- `YYYY` 四位年份；`MDD` 是月份接两位日期（10 月 23 日是 `1023`，1 月 3 日是 `103`）；`N` 是当天第几个版本，从 `0` 起。
  例：`2026.1023.0`、同一天再发 `2026.1023.1`。
- `MDD` 不补成四位：版本号要是合法的 semver，各段不能有前导零（`0103` 会被拒）。月份在前、日期固定两位，所以
  不同日子不会撞号，数值大小也和日期先后一致。
- 日期就是打 tag 那天。当天的 rc 接着发 `-rc.N+1`，当天发正式版就沿用 rc 的号、只去掉 `-rc.N`；前几天没发完的
  rc 不再接着用旧日期，不管再发 rc 还是正式版都取今天的新号（`2026.1026.0-rc.2` 隔天再发是 `2026.1027.0-rc.1`）。
- 版本号只表示先后，不表示兼容：桌面和运行时兼不兼容看控制协议版本，不看它。v0.9.1 之后改用这套，
  数值上 `2026.x` 比 `0.9.x` 大，运行时「只升不降」和自动更新都照常。

版本号不写在代码里，打包时由 `scripts/version.sh` 从 git tag 算出，经环境变量 `SMELT_BUILD_VERSION` 在编译时编进程序
（`crates/smelt-version`）。`Cargo.toml` 的 `version` 固定 `0.0.0`，发版不改它，也就没有「改版本号」的提交。

- 提交正好是发版 tag `v<版本>`：版本号就是 `<版本>`，如 `2026.1027.0-rc.1`。发版流水线直接把 tag 名传进
  `SMELT_BUILD_VERSION`。
- tag 之后的提交：最近的发版 tag 加 semver 构建信息 `+g<提交>`，如 `2026.1027.0-rc.1+g32c3172`。同一个 tag 之后打的包
  各不相同，能直接对回代码。比新旧时不看 `+` 后面；请远程运行时升级时去掉它，升到 GitHub 上那个 Release
  （`smelt_version::RELEASE`）。运行时本身是 `<版本>+g…` 时，不会被「升」回 `<版本>`。
- 只合并本仓库、自己不打 tag 的下游仓库，按最近合进来的本仓库提交算（`scripts/version.sh <提交>`）：合进来的正好是
  发版提交时，打出的包版本号就等于 tag。
- 算不出来（不在 git 仓库里、浅克隆、没有 tag）打包直接失败，不打出版本号是占位值的包。没注入版本号的开发构建
  （`cargo run`、`make build`）显示 `0.0.0-dev`，不能请远程运行时升级。
- `SMELT_BUILD_VERSION` 只管编译时注入；一键安装脚本里指定装哪个版本的是另一个变量 `SMELT_VERSION`。

- 桌面与运行时：上面算出的版本号。macOS 的 `CFBundleShortVersionString` 也是它；`CFBundleVersion` 取打包时的 UTC 时间
  （同 iOS 构建号），纯数字、只增不减。
- Android：versionName 是 tag 去掉 `v`（含 `-rc.N`）；versionCode 是 release 流水线的 `run_number`，逐次递增，
  新包总能覆盖安装旧包。
- iOS：版本号是发版版本号去掉 `-rc.N`（Apple 只认纯数字的 `x.y.z`）：同一次发版的 rc 和正式版共用一个版本号，在
  TestFlight 里靠构建号区分。构建号取出包时的 UTC 时间（`%y%j%H%M`：两位年、一年中的第几天、时、分），只增不减，
  又在 32 位整数以内。见下面的「移动端」。
- `mobile/pubspec.yaml` 里的版本只给本地开发构建用，发布包一律在命令行上传入版本号，不改它。

## 移动端

手机的版本号跟发版统一：手机发布时，版本号就是它出自的那次发版的号。但**不是每次发版都要发手机**。手机能不能连上
运行时只看控制协议版本，跟版本号无关，所以只有这两种情况要发：

1. 控制协议的最低支持版本抬高了（`CONTROL_API_MIN_SUPPORTED_VERSION`，比如 v2 → v3）：新运行时不再接受旧手机。
   这种发版桌面、运行时、手机要一起发，而且先等手机过审，或者至少先上 TestFlight，再推 tag，免得用户先升了
   运行时、手机却连不上。
2. 手机自己有新功能或修复。

别的发版只发桌面和运行时，App Store 上的手机留在旧版本，照常能连。手机的版本号会因此跳着走（`2026.1023.0` 之后
直接是 `2026.1105.0`），App Store 只要求递增。

- Android：发版流水线自动出包，跟着每次发版走，见上面的「版本号」。
- iOS：发版流水线的 `ios` job 每次发版都传一个构建到 TestFlight，内部测试组自动收到（组设置里要开着「自动分发」；
  出口合规已在 `Info.plist` 声明，构建不会卡在「缺少合规证明」）。外部测试和上架都要过 Apple 审核，暂时不做；
  要不要提交 App Store 审核，按上面两种情况在 App Store Connect 里手动选构建提交。

  协议抬高最低版本的那种发版要先上 TestFlight 再推 tag，流水线做不到这个顺序，要在本地出包上传。
  `scripts/release.sh` 在本地打好 tag 之后就能出包，不必等推送，按「本地打 tag → 出 iOS 包传 TestFlight → 推送」走。
  推送后流水线会再传一个同版本号的构建，构建号更大，不冲突。在 tag 上执行：

  ```sh
  git checkout v<版本>
  scripts/build-ios.sh --upload   # 出包直接上传；不带 --upload（= make ios）只出 ipa，在 mobile/build/ios/ipa/
  ```

  脚本只在当前提交正好是发版 tag、`mobile/` 和 `crates/` 没有未提交改动时出包，保证包和发版的代码对得上。
  签名用 `mobile/ios/Flutter/SigningDefaults.xcconfig` 里的正式团队和 Bundle ID，导出设置在
  `mobile/ios/ExportOptions.plist`。本地用 Xcode 里登录的账号签名。

## Windows

`windows` job 在 `windows-latest` 上 `cargo build --release -p smelt -p smelt-cli -p smeltd --locked`，再用
`scripts/build-smelt-pi.sh` 编 `smelt-pi.exe`（Bun 1.4.3，`bun-windows-x64`）。下面这些文件放进暂存目录，
再用 Inno Setup 7（钉 7.1.0、对哈希）按 `scripts/windows-installer.iss` 编成
`smelt-<版本>-windows-x86_64-setup.exe`：

- `smelt.exe`
- `smelt.com`：`smelt-cli.exe` 改名。`smelt.exe` 是 GUI 子系统，命令行子命令在 cmd / PowerShell
  里没有输出；敲 `smelt` 时先找到 `.com`，子命令在它里面跑，不带子命令就拉起 `smelt.exe`
- `smeltd.exe`
- `smelt-notify.exe`
- `smelt-pi.exe`
- Pi 运行要的 `photon_rs_bg.wasm`（以及打出来的 `skills/`、`native/`）

安装程序按用户安装，不要管理员权限，默认装到 `%LOCALAPPDATA%\Programs\Smelt`，建开始菜单项，可选把安装目录
加进用户 PATH（敲 `smelt` 找到 `smelt.com`）。桌面运行时持有命名互斥量 `SmeltDesktop`，安装和卸载开头查到它就
提示先关掉 Smelt。卸载会停掉本机守护（它跑的是 `%USERPROFILE%\.smelt\bin` 里的副本，不在安装目录），数据目录
不删。

桌面在自己旁边找 `smeltd.exe`、`smelt-notify.exe`、`smelt-pi.exe`。由 `windows-release` 挂到同一个 Release。
校验和在 `checksums` 里和其它产物写在一起。

这一版 exe 和安装程序都不签名。下载后 Windows SmartScreen 可能提示未知发布者，选择仍要运行即可。
桌面的应用内更新认的就是这个安装程序：发布里同时有它和 `SHA256SUMS`、清单里已经有它的校验和，才算这个发布能装。
下载、校验后带 `/VERYSILENT /WAITPID=<桌面 pid> [/RELAUNCH=<桌面 exe>]` 拉起，桌面退出后安装程序覆盖安装，见
`crates/smelt-client/src/updater/windows.rs`。
本机守护可以冷启动。升级是关掉旧进程再拉起新的，终端会话不保留。Release 说明里有同样的一段。

## Android 签名

APK 必须一直用同一把 keystore 签名，否则已装的用户只能卸载重装、数据丢失；应用内更新也装不上去
（系统按签名认是不是同一个应用）。应用内更新认的是 Release 里的 `Smelt-<版本>-android-arm64.apk` 和
`SHA256SUMS` 里它的条目，版本号取自 `--build-name`，见 `mobile/README.md` 的「In-app updates」。keystore 生成于 2026-10-19，
证书 SHA256 指纹 `F3:68:94:C3:7F:AC:49:70:9A:FE:3D:93:41:09:96:36:52:F0:84:DC:32:7E:13:55:8D:DC:3B:6A:79:B3:5D:32`，
存在 repo secrets：`SMELT_ANDROID_KEYSTORE_BASE64`、`SMELT_ANDROID_KEYSTORE_PASSWORD`、`SMELT_ANDROID_KEY_PASSWORD`、
`SMELT_ANDROID_KEY_ALIAS`。原件与密码由维护者离线保管，不进仓库。

本地出正式签名包：照 `mobile/README.md` 的 Release signing 写好 `mobile/android/key.properties`，再

```sh
cd mobile && flutter build apk --release --target-platform android-arm64 \
  --build-name 2026.1023.0 --build-number <比线上大的数>
```

## iOS 签名

自动签名，团队和 Bundle ID 见上面的「移动端」。CI 上没有 Xcode 账号，用 repo secrets 代替：

- App Store Connect API 密钥：导出时用云端托管的分发证书签名，再直接上传。私钥在 Apple，CI 上不放分发证书。
  在 App Store Connect → 用户和访问 → 集成 → 团队密钥里建，权限选 Admin（云端托管证书要这个权限）。
  - `SMELT_ASC_API_KEY_P8`：下载的 `AuthKey_<Key ID>.p8` 的全部内容（只能下载一次）
  - `SMELT_ASC_API_KEY_ID`：Key ID
  - `SMELT_ASC_API_ISSUER_ID`：同一页上方的 Issuer ID
- 一张固定的 Apple Development 证书：归档时要一张本机有私钥的开发证书，不给的话 Xcode 每次用密钥新建一张，
  团队里越攒越多。从钥匙串导出证书和私钥成 .p12：
  - `SMELT_IOS_DEV_CERT_P12_BASE64`：`base64 -i dev.p12 | pbcopy`
  - `SMELT_IOS_DEV_CERT_P12_PASSWORD`：导出时设的密码

  证书一年过期，到期换一张、更新这两个 secret。

缺任何一个 `ios` job 都直接失败。
