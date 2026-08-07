#!/usr/bin/env bash
# Android 端（Flutter + Rust/NDK）的构建环境初始化。
#
# 检查各工具版本，不达标的就地安装，最后把这次运行需要的环境变量写进
# .ci/.env/android.sh，交给 .ci/android/ci.sh source。
#
# 构建机的三条硬规矩（不写系统目录 / 全局配置只留在当前上下文 / 不复用机器上
# 已有的工具链目录）见 .ci/lib/common.sh 顶部。
#
# 用法：
#   ./.ci/android/env-setup.sh              检查并按需安装
#   ./.ci/android/env-setup.sh --check      只检查不安装（不达标则退出码非 0）
#
# 可用环境变量覆盖（内网机器如果连不上外网，用这些指向镜像）：
#   SMELT_CI_TOOLCHAIN_ROOT     工具链安装位置，默认 ~/.smelt-ci/toolchains
#   FLUTTER_GIT_URL             Flutter SDK 的 git 源
#   FLUTTER_STORAGE_BASE_URL    Flutter 引擎产物的下载源
#   PUB_HOSTED_URL              pub 包管理源
#   RUSTUP_UPDATE_ROOT          rustup 自身的下载源
#   RUSTUP_DIST_SERVER          Rust 工具链的下载源
#   CARGO_REGISTRY_MIRROR       crates.io 镜像
#   ANDROID_SDK_URL_BASE        Android SDK 仓库源，默认 dl.google.com
#   ANDROID_CMDLINE_TOOLS_URL   直接指定 commandline-tools 压缩包地址
#   SMELT_ANDROID_JAVA_HOME     指定用哪个 JDK（不设则自动挑一个合适版本）
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

# ── 版本要求：单一事实来源 ──────────────────────────────────────────────────
# Flutter    mobile/pubspec.yaml 要求 Dart sdk ^3.12.2，Flutter 3.44.8 捆的正是
#            Dart 3.12.2，再低就解析不了依赖。
# Rust       编 crates/smelt-mobile，edition 2024 需要 1.85+。
# JDK        Gradle 9.1 + AGP 9 支持到 JDK 25；JDK 26 目前会被 Gradle 直接拒绝，
#            所以这里既要下限也要上限。
# SDK/NDK    必须和 Flutter 模板里的默认值一致，见
#            packages/flutter_tools/gradle/src/main/kotlin/FlutterExtension.kt。
#            改这里要同步 mobile/rust_builder/android/build.gradle 里写死的那份。
REQUIRED_FLUTTER="3.44.8"
REQUIRED_RUST="1.85.0"
REQUIRED_JDK_MIN=17
REQUIRED_JDK_MAX=25
ANDROID_COMPILE_SDK="36"
ANDROID_BUILD_TOOLS="36.0.0"
ANDROID_NDK="28.2.13676358"
# commandline-tools 没有稳定的 "latest" 别名，只能钉版本号。
ANDROID_CMDLINE_TOOLS_VERSION="13114758"

# cargokit 会按 -Ptarget-platform 决定编哪些 ABI，这里把可能用到的都装上。
# android-x86（i686）只在 debug 跑模拟器时用得到，但装它很便宜。
RUST_ANDROID_TARGETS=(
  aarch64-linux-android
  armv7-linux-androideabi
  x86_64-linux-android
  i686-linux-android
)

ANDROID_SDK_DIR="$TOOLCHAIN_ROOT/android-sdk"

CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

# ── 前置检查：这些装不了，只能报清楚 ────────────────────────────────────────

# 挑一个 Gradle 能接受的 JDK。
#
# 不直接用 PATH 上的 java：构建机上常年装着最新的 JDK，而 Gradle 对新版本的支持
# 总是滞后一两个大版本（JDK 26 出来时 Gradle 9.1 只认到 25），撞上就是一句
# "Unsupported class file major version"，很难联想到是 JDK 太新。
resolve_jdk() {
  if [[ -n "${SMELT_ANDROID_JAVA_HOME:-}" ]]; then
    [[ -x "$SMELT_ANDROID_JAVA_HOME/bin/javac" ]] \
      || die "SMELT_ANDROID_JAVA_HOME 指向的不是有效 JDK：$SMELT_ANDROID_JAVA_HOME"
    JAVA_HOME_RESOLVED="$SMELT_ANDROID_JAVA_HOME"
    ok "JDK（来自 SMELT_ANDROID_JAVA_HOME）$("$JAVA_HOME_RESOLVED/bin/javac" -version 2>&1)"
    return 0
  fi

  # java_home 是 macOS 自带的，按版本降序找一个落在支持区间里的。
  # 显式初始化 v：set -u 下 (( v = ... )) 的形式在 bash 3.2（macOS 自带）里
  # 仍会先求值未定义的 v，报 unbound variable。
  local v="$REQUIRED_JDK_MAX" home
  while ((v >= REQUIRED_JDK_MIN)); do
    home="$(/usr/libexec/java_home -v "$v" 2>/dev/null || true)"
    if [[ -n "$home" && -x "$home/bin/javac" ]]; then
      JAVA_HOME_RESOLVED="$home"
      ok "JDK ${v}（${home}）"
      return 0
    fi
    v=$((v - 1))
  done

  die "找不到 ${REQUIRED_JDK_MIN}~${REQUIRED_JDK_MAX} 之间的 JDK。
  Gradle 9.1 不接受更高版本，装 JDK 需要管理员权限，请联系构建机管理员执行：
    brew install --cask temurin@21
  或用 SMELT_ANDROID_JAVA_HOME 指向已有的 JDK。"
}

preflight() {
  require_macos
  require_xcode_clt
  resolve_jdk
}

# ── Flutter ─────────────────────────────────────────────────────────────────
# 与 .ci/ios/env-setup.sh 装到同一个 FLUTTER_DIR：两个平台共用一份 Flutter SDK，
# 版本要求也来自同一处（mobile/pubspec.yaml）。
setup_flutter() {
  local have=""
  have="$(tool_version "$FLUTTER_DIR/bin/flutter" --version)"

  need_install "Flutter" "$have" "$REQUIRED_FLUTTER" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "Flutter 不满足要求（--check 模式不安装）"

  local git_url="${FLUTTER_GIT_URL:-https://github.com/flutter/flutter.git}"
  if [[ ! -d "$FLUTTER_DIR/.git" ]]; then
    info "克隆 Flutter $REQUIRED_FLUTTER 到 $FLUTTER_DIR"
    mkdir -p "$(dirname "$FLUTTER_DIR")"
    git clone --depth 1 --branch "$REQUIRED_FLUTTER" "$git_url" "$FLUTTER_DIR" \
      || die "克隆 Flutter 失败。内网机器请设置 FLUTTER_GIT_URL 指向镜像。"
  else
    info "升级 Flutter 到 $REQUIRED_FLUTTER"
    git -C "$FLUTTER_DIR" fetch --depth 1 origin "refs/tags/$REQUIRED_FLUTTER:refs/tags/$REQUIRED_FLUTTER" \
      || die "拉取 Flutter $REQUIRED_FLUTTER 失败"
    git -C "$FLUTTER_DIR" checkout -q "$REQUIRED_FLUTTER" || die "切换 Flutter 版本失败"
  fi

  export PUB_CACHE="$PUB_CACHE_DIR" FLUTTER_SUPPRESS_ANALYTICS=true
  info "预热 Flutter（首次会下载 Dart SDK 与引擎，较慢）"
  "$FLUTTER_DIR/bin/flutter" --version >/dev/null || die "Flutter 预热失败"

  ok "Flutter $(tool_version "$FLUTTER_DIR/bin/flutter" --version)"
}

# ── Rust ────────────────────────────────────────────────────────────────────
# 和 .ci/mac/env-setup.sh 共用 CARGO_HOME / RUSTUP_HOME，区别只在多装几个
# Android target。
setup_rust() {
  export CARGO_HOME="$CARGO_HOME_DIR" RUSTUP_HOME="$RUSTUP_HOME_DIR"
  local rustup_bin="$CARGO_HOME_DIR/bin/rustup"
  local rustc_bin="$CARGO_HOME_DIR/bin/rustc"
  local have=""
  have="$(tool_version "$rustc_bin" --version)"

  if need_install "Rust" "$have" "$REQUIRED_RUST"; then
    [[ "$CHECK_ONLY" == 1 ]] && die "Rust 不满足要求（--check 模式不安装）"

    if [[ -x "$rustup_bin" ]]; then
      info "升级 Rust 工具链"
      "$rustup_bin" update stable || die "rustup update 失败"
      "$rustup_bin" default stable >/dev/null
    else
      info "安装 rustup 到 $CARGO_HOME_DIR"
      local update_root="${RUSTUP_UPDATE_ROOT:-https://static.rust-lang.org/rustup}"
      curl -fsSL --retry 3 "$update_root/dist/$(uname -m)-apple-darwin/rustup-init" \
        -o "$TOOLCHAIN_ROOT/rustup-init" \
        || die "下载 rustup-init 失败。内网机器请设置 RUSTUP_UPDATE_ROOT 指向镜像。"
      chmod +x "$TOOLCHAIN_ROOT/rustup-init"
      "$TOOLCHAIN_ROOT/rustup-init" -y --no-modify-path --default-toolchain stable \
        || die "安装 rustup 失败"
      rm -f "$TOOLCHAIN_ROOT/rustup-init"
    fi
    ok "Rust $(tool_version "$rustc_bin" --version)"
  fi

  # Android target：cargokit 交叉编译时缺哪个都是编到一半才报错，提前补齐。
  local installed target
  installed="$("$rustup_bin" target list --installed 2>/dev/null || true)"
  for target in "${RUST_ANDROID_TARGETS[@]}"; do
    if grep -qx "$target" <<<"$installed"; then
      ok "Rust target $target"
      continue
    fi
    [[ "$CHECK_ONLY" == 1 ]] && die "缺少 Rust target ${target}（--check 模式不安装）"
    info "安装 Rust target $target"
    "$rustup_bin" target add "$target" || die "安装 Rust target $target 失败"
  done
}

# ── Android SDK ─────────────────────────────────────────────────────────────
# 不用 Android Studio：构建机上装不了 GUI 应用，也没必要。commandline-tools 里的
# sdkmanager 足够拉齐 platform / build-tools / NDK。
setup_android_sdk() {
  local sdkmanager="$ANDROID_SDK_DIR/cmdline-tools/latest/bin/sdkmanager"

  if [[ ! -x "$sdkmanager" ]]; then
    [[ "$CHECK_ONLY" == 1 ]] && die "Android commandline-tools 未安装（--check 模式不安装）"
    info "安装 Android commandline-tools 到 $ANDROID_SDK_DIR"

    local url="${ANDROID_CMDLINE_TOOLS_URL:-}"
    if [[ -z "$url" ]]; then
      local base="${ANDROID_SDK_URL_BASE:-https://dl.google.com/android/repository}"
      url="$base/commandlinetools-mac-${ANDROID_CMDLINE_TOOLS_VERSION}_latest.zip"
    fi

    local tmp
    tmp="$(mktemp -d)"
    # trap 只覆盖这个函数体内的失败路径；成功路径末尾也会清掉。
    trap 'rm -rf "$tmp"' RETURN
    curl -fsSL --retry 3 "$url" -o "$tmp/cmdline-tools.zip" \
      || die "下载 Android commandline-tools 失败。内网机器请设置 ANDROID_CMDLINE_TOOLS_URL。"
    unzip -q "$tmp/cmdline-tools.zip" -d "$tmp/extract" || die "解压 commandline-tools 失败"

    # 压缩包解出来是 cmdline-tools/，但 sdkmanager 要求自己位于
    # <sdk>/cmdline-tools/<版本或 latest>/ 下，否则它算不出 SDK 根目录，
    # 会把包装到当前工作目录去。
    mkdir -p "$ANDROID_SDK_DIR/cmdline-tools"
    rm -rf "$ANDROID_SDK_DIR/cmdline-tools/latest"
    mv "$tmp/extract/cmdline-tools" "$ANDROID_SDK_DIR/cmdline-tools/latest"
    rm -rf "$tmp"
    trap - RETURN

    [[ -x "$sdkmanager" ]] || die "commandline-tools 安装后仍找不到 sdkmanager"
  fi
  ok "Android commandline-tools"

  local packages=(
    "platform-tools"
    "platforms;android-${ANDROID_COMPILE_SDK}"
    "build-tools;${ANDROID_BUILD_TOOLS}"
    "ndk;${ANDROID_NDK}"
  )

  # 已装的包 sdkmanager 会跳过，所以无脑跑一遍即可；但 --check 模式下要先判断。
  if [[ "$CHECK_ONLY" == 1 ]]; then
    local installed
    installed="$(JAVA_HOME="$JAVA_HOME_RESOLVED" "$sdkmanager" --list_installed 2>/dev/null || true)"
    local pkg
    for pkg in "${packages[@]}"; do
      grep -q "^\s*${pkg//;/\\;}\b" <<<"$installed" \
        || die "缺少 Android SDK 组件 ${pkg}（--check 模式不安装）"
      ok "Android SDK $pkg"
    done
    return 0
  fi

  # 许可协议必须先接受，否则 sdkmanager 装到一半会停在交互提示上把 CI 挂死。
  # yes 的管道在 sdkmanager 提前退出时会拿到 SIGPIPE，用 || true 兜住。
  info "接受 Android SDK 许可协议"
  yes 2>/dev/null | JAVA_HOME="$JAVA_HOME_RESOLVED" "$sdkmanager" \
    --sdk_root="$ANDROID_SDK_DIR" --licenses >/dev/null 2>&1 || true

  info "安装 Android SDK 组件：${packages[*]}"
  JAVA_HOME="$JAVA_HOME_RESOLVED" "$sdkmanager" \
    --sdk_root="$ANDROID_SDK_DIR" "${packages[@]}" >/dev/null \
    || die "安装 Android SDK 组件失败。内网机器请设置 ANDROID_SDK_URL_BASE 指向镜像。"

  local pkg
  for pkg in "${packages[@]}"; do ok "Android SDK $pkg"; done
}

# ── 产出运行上下文 ──────────────────────────────────────────────────────────
write_env_file() {
  local file
  file="$(begin_env_file android)"

  cat >>"$file" <<EOF

# 工具链装在 \$HOME 下的独立目录，不碰系统目录，也不碰机器上可能已有的
# ~/.cargo / ~/.rustup / ~/.pub-cache。
export FLUTTER_ROOT="$FLUTTER_DIR"
export PUB_CACHE="$PUB_CACHE_DIR"
export CARGO_HOME="$CARGO_HOME_DIR"
export RUSTUP_HOME="$RUSTUP_HOME_DIR"

# 钉死 JDK：构建机上的默认 java 可能比 Gradle 9.1 支持的还新，撞上只会报一句
# 难懂的 "Unsupported class file major version"。
export JAVA_HOME="$JAVA_HOME_RESOLVED"

# Flutter 和 Gradle 认的是 ANDROID_SDK_ROOT / ANDROID_HOME 两个变量，都设上。
export ANDROID_SDK_ROOT="$ANDROID_SDK_DIR"
export ANDROID_HOME="$ANDROID_SDK_DIR"
export ANDROID_NDK_HOME="$ANDROID_SDK_DIR/ndk/$ANDROID_NDK"

export PATH="\$JAVA_HOME/bin:$FLUTTER_DIR/bin:$CARGO_HOME_DIR/bin:$ANDROID_SDK_DIR/platform-tools:\$PATH"

# 关掉遥测。用环境变量而不是 flutter config，后者会写 ~/.flutter，那是持久的
# 全局状态。
export FLUTTER_SUPPRESS_ANALYTICS=true
export DART_ANALYTICS_DISABLED=1

# Gradle 的守护进程会在构建结束后继续占着内存，CI 上没有复用价值。
export GRADLE_OPTS="-Dorg.gradle.daemon=false"
EOF

  passthrough_mirrors "$file" FLUTTER_STORAGE_BASE_URL PUB_HOSTED_URL \
    RUSTUP_DIST_SERVER CARGO_REGISTRY_MIRROR
}

main() {
  info "Android 端构建环境初始化（工具链根目录：${TOOLCHAIN_ROOT}）"
  echo

  preflight
  echo

  setup_flutter
  setup_rust
  setup_android_sdk
  echo

  if [[ "$CHECK_ONLY" == 1 ]]; then
    ok "所有工具版本均满足要求"
    return 0
  fi

  write_env_file
  echo
  ok "环境就绪，已写入 .ci/.env/android.sh"
  echo "  接下来执行构建：./.ci/android/ci.sh"
}

main "$@"
