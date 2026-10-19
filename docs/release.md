# 发版

推 `v*` tag 触发 `.github/workflows/release.yml`，产物都传到同一个 GitHub Release：

| 产物 | job | 说明 |
|---|---|---|
| `Smelt.dmg` | `build`（macos-14） | `make dist-build`，用 secrets 里的自签名证书签名（`CODESIGN_P12_*`） |
| `smeltd-<版本>-linux-{x86_64,aarch64}.tar.gz`、`SHA256SUMS`、`install.sh` | `linux` → `linux-release` | 两种架构各自原生构建，见 `docs/remote-runtime.html` 的 D7 |
| `Smelt-<版本>-android-arm64.apk` | `android` → `android-release` | 正式签名（`SMELT_ANDROID_*` secrets），没配签名直接失败 |

## 打 RC

1. 确认 `main` 上 `Cargo.toml` 的 `version` 就是这次要发的版本（如 `0.9.0`）。tag 去掉 `v` 和 `-` 后缀后必须与它一致。
2. 在要发的 `main` 提交上打 tag 并推送：

   ```sh
   git tag v0.9.0-rc.3
   git push gh v0.9.0-rc.3
   ```

3. tag 带 `-` 后缀就发成预发布：不会成为 Latest，Linux 一键安装默认也不会装它（要装得设 `SMELT_VERSION`）。
4. 测试通过后在同一个提交（或修复后的提交）上打 `v0.9.0`，发成正式版。

## 版本号

- 桌面与运行时：`Cargo.toml` 的 `version`。
- Android：versionName 是 tag 去掉 `v`（含 `-rc.N`）；versionCode 是 release 流水线的 `run_number`，逐次递增，
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
