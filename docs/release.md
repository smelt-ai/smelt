# 发版

推 `v*` tag 触发 `.github/workflows/release.yml`，产物都传到同一个 GitHub Release：

| 产物 | job | 说明 |
|---|---|---|
| `Smelt.dmg` | `build`（macos-14） | `make dist-build`，用 secrets 里的自签名证书签名（`CODESIGN_P12_*`） |
| `smeltd-<版本>-linux-{x86_64,aarch64}.tar.gz`、`install.sh` | `linux` → `linux-release` | 两种架构各自原生构建，见 `docs/archive/remote-runtime.html` 的 D7 |
| `Smelt-<版本>-android-arm64.apk` | `android` → `android-release` | 正式签名（`SMELT_ANDROID_*` secrets），没配签名直接失败 |
| `SHA256SUMS` | `linux-release` → `checksums` | 先只含 Linux 包（一键安装和运行时自动升级马上能用），全部产物到齐后重写成覆盖所有产物 |

## 打 RC

tag 必须**正好**是 `v<Cargo.toml 的工作区版本>`，预发布也一样：桌面请远程运行时升级时，运行时按桌面的版本号
去同名 tag 的 Release 下载 `smeltd-<版本>-linux-<架构>.tar.gz`，版本号里少了 `-rc.N` 就找不到包。

1. 提 PR 把 `Cargo.toml` 的 `[workspace.package] version` 改成这次的版本（如 `0.9.1-rc.1`），合进 `main`。
2. 在这个提交上打 tag 并推送：

   ```sh
   git tag -a v0.9.1-rc.1 -m "v0.9.1-rc.1"
   git push gh v0.9.1-rc.1
   ```

3. tag 带 `-` 后缀就发成预发布：不会成为 Latest，Linux 一键安装默认也不会装它（要装得设 `SMELT_VERSION`）。
4. 测试通过后把版本改成 `0.9.1`、合进 `main`，再打 `v0.9.1` 发正式版。

## 发布说明

由 `build` job 的「Release notes」步骤生成，不用 GitHub 默认的起点（它往回找上一个正式版，而 v0.6.6–v0.6.14
不在现在 main 的历史上，会一路退回 v0.6.5，把早就发过的 PR 再列一遍）。起点按版本号往前找第一个是本次祖先的 tag：
预发布从上一个 tag（含 rc）算起，正式版从上一个正式版算起，所以正式版的说明涵盖它之前所有 rc 的改动。

## 版本号

- 桌面与运行时：`Cargo.toml` 的 `version`。
- Android：versionName 是 tag 去掉 `v`（含 `-rc.N`，与工作区版本相同）；versionCode 是 release 流水线的 `run_number`，逐次递增，
  新包总能覆盖安装旧包。`mobile/pubspec.yaml` 里的版本只给本地开发构建用。

## Android 签名

APK 必须一直用同一把 keystore 签名，否则已装的用户只能卸载重装、数据丢失。keystore 生成于 2026-10-19，
证书 SHA256 指纹 `F3:68:94:C3:7F:AC:49:70:9A:FE:3D:93:41:09:96:36:52:F0:84:DC:32:7E:13:55:8D:DC:3B:6A:79:B3:5D:32`，
存在 repo secrets：`SMELT_ANDROID_KEYSTORE_BASE64`、`SMELT_ANDROID_KEYSTORE_PASSWORD`、`SMELT_ANDROID_KEY_PASSWORD`、
`SMELT_ANDROID_KEY_ALIAS`。原件与密码由维护者离线保管，不进仓库。

本地出正式签名包：

```sh
SMELT_ANDROID_KEYSTORE=<jks 路径> SMELT_ANDROID_KEYSTORE_PASSWORD=… \
SMELT_ANDROID_KEY_ALIAS=smelt SMELT_ANDROID_KEY_PASSWORD=… \
SMELT_BUILD_NAME=0.9.0 SMELT_BUILD_NUMBER=<比线上大的数> \
./ci/android/ci.sh build
```

iOS 没有发版流水线：免费开发者账号只能直装到自己的设备（`scripts/install-ios-profile.sh`）。
