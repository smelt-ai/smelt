#!/usr/bin/env bash
# iOS 端（Flutter）的构建环境初始化。
#
# 检查各工具版本，不达标的就地升级，最后把这次运行需要的环境变量写进
# .ci/.env/ios.sh，交给 .ci/ios/ci.sh source。
#
# 构建机的三条硬规矩（不写系统目录 / 全局配置只留在当前上下文 / 不复用机器上
# 已有的工具链目录）见 .ci/lib/common.sh 顶部。
#
# 用法：
#   ./.ci/ios/env-setup.sh              检查并按需升级
#   ./.ci/ios/env-setup.sh --check      只检查不安装（不达标则退出码非 0）
#
# 可用环境变量覆盖（内网机器如果连不上外网，用这些指向镜像）：
#   SMELT_CI_TOOLCHAIN_ROOT   工具链安装位置，默认 ~/.smelt-ci/toolchains
#   FLUTTER_GIT_URL           Flutter SDK 的 git 源
#   FLUTTER_STORAGE_BASE_URL  Flutter 引擎产物的下载源
#   PUB_HOSTED_URL            pub 包管理源
#   GEM_SOURCE_URL            RubyGems 镜像（装 CocoaPods 用）
#   COCOAPODS_CDN_URL         CocoaPods 的 spec 源
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

# ── 版本要求：单一事实来源 ──────────────────────────────────────────────────
# Flutter    mobile/pubspec.yaml 要求 Dart sdk ^3.12.2，Flutter 3.44.8 捆的正是
#            Dart 3.12.2，再低就解析不了依赖。
# CocoaPods  iOS 构建时 flutter 会自动调 pod install 装 Runner 的原生依赖。
REQUIRED_FLUTTER="3.44.8"
REQUIRED_COCOAPODS="1.15.2"

CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

# ── 前置检查：这些装不了，只能报清楚 ────────────────────────────────────────
preflight() {
  require_macos
  require_xcode_clt

  # iOS 构建要完整的 Xcode.app，光有 Command Line Tools 不够：xcodebuild 需要
  # iOS SDK 和 Simulator runtime，那些只随 Xcode.app 分发。
  #
  # 没有 Xcode 时 xcode-select -p 会指向 CommandLineTools 目录，据此判断。切换
  # xcode-select 路径要 sudo，我们做不了，只能把要管理员做的事说清楚。
  local dev_dir
  dev_dir="$(xcode-select -p 2>/dev/null || true)"
  if [[ "$dev_dir" != *Xcode*.app* ]]; then
    die "当前 xcode-select 指向 ${dev_dir:-未知}，不是完整的 Xcode。
  iOS 构建需要 Xcode.app 提供的 iOS SDK，Command Line Tools 不含这些。
  切换路径需要管理员权限，请联系构建机管理员执行：
    sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
  fi
  local xcode_version
  xcode_version="$(tool_version "$(command -v xcodebuild)" -version)"
  [[ -n "$xcode_version" ]] || die "xcodebuild 跑不起来，Xcode 安装可能不完整"
  ok "Xcode ${xcode_version}"

  # 首次用 Xcode 要接受许可协议，否则 xcodebuild 一律拒绝执行。接受要 sudo。
  if ! xcodebuild -checkFirstLaunchStatus >/dev/null 2>&1; then
    warn "Xcode 首次启动检查未通过（可能是许可协议未接受或组件未安装）。"
    warn "  需要管理员执行：sudo xcodebuild -runFirstLaunch"
  fi
}

# ── Flutter ─────────────────────────────────────────────────────────────────
setup_flutter() {
  local have=""
  have="$(tool_version "$FLUTTER_DIR/bin/flutter" --version)"

  need_install "Flutter" "$have" "$REQUIRED_FLUTTER" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "Flutter 不满足要求（--check 模式不安装）"

  local git_url="${FLUTTER_GIT_URL:-https://github.com/flutter/flutter.git}"
  if [[ ! -d "$FLUTTER_DIR/.git" ]]; then
    info "克隆 Flutter $REQUIRED_FLUTTER 到 $FLUTTER_DIR"
    mkdir -p "$(dirname "$FLUTTER_DIR")"
    # --depth 1 拿单个 tag：完整历史 2G 起步，构建机上没必要。
    git clone --depth 1 --branch "$REQUIRED_FLUTTER" "$git_url" "$FLUTTER_DIR" \
      || die "克隆 Flutter 失败。内网机器请设置 FLUTTER_GIT_URL 指向镜像。"
  else
    info "升级 Flutter 到 $REQUIRED_FLUTTER"
    git -C "$FLUTTER_DIR" fetch --depth 1 origin "refs/tags/$REQUIRED_FLUTTER:refs/tags/$REQUIRED_FLUTTER" \
      || die "拉取 Flutter $REQUIRED_FLUTTER 失败"
    git -C "$FLUTTER_DIR" checkout -q "$REQUIRED_FLUTTER" || die "切换 Flutter 版本失败"
  fi

  # 首次运行会下载 Dart SDK 与引擎产物。放在这里跑，是为了让下载失败暴露在环境
  # 准备阶段，而不是等到 ci.sh 跑构建时才炸。
  export PUB_CACHE="$PUB_CACHE_DIR" FLUTTER_SUPPRESS_ANALYTICS=true
  info "预热 Flutter（首次会下载 Dart SDK 与引擎，较慢）"
  "$FLUTTER_DIR/bin/flutter" --version >/dev/null || die "Flutter 预热失败"

  ok "Flutter $(tool_version "$FLUTTER_DIR/bin/flutter" --version)"
}

# ── CocoaPods ───────────────────────────────────────────────────────────────
# 用系统 ruby 的 gem，但把 GEM_HOME 指到我们自己的目录：这样不需要 sudo，也不会
# 往机器共用的 gem 目录里塞东西。装到 ~/.gem（--user-install 的默认位置）同样会
# 影响别的项目，所以也不用。
setup_cocoapods() {
  # 只设 GEM_HOME，不设 GEM_PATH。gem install 会装到 GEM_HOME，而搜索路径仍然
  # 保留 ruby 自带的 gem 目录——CocoaPods 依赖 base64、json 这些随 ruby 分发的
  # default gem，把 GEM_PATH 独占改成我们自己的目录会把它们踢出搜索路径，pod 一
  # 启动就报 "Could not find 'base64'"。
  export GEM_HOME="$GEM_HOME_DIR"
  unset GEM_PATH
  local bin="$GEM_HOME_DIR/bin/pod"
  local have=""
  have="$(tool_version "$bin" --version)"

  need_install "CocoaPods" "$have" "$REQUIRED_COCOAPODS" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "CocoaPods 不满足要求（--check 模式不安装）"

  command -v gem >/dev/null 2>&1 || die "找不到 gem，无法安装 CocoaPods"

  info "安装 CocoaPods 到 $GEM_HOME_DIR"
  # 不锁具体版本，装最新的即可：REQUIRED_COCOAPODS 是下限，不是钉子。构建机的
  # ruby 版本未知，锁一个老版本反而容易碰上原生扩展编不过。
  local gem_args=(install cocoapods --no-document)
  [[ -n "${GEM_SOURCE_URL:-}" ]] && gem_args+=(--clear-sources --source "$GEM_SOURCE_URL")
  # 装的是纯 ruby + 少量原生扩展（ffi），CLT 已在 preflight 里确认过。
  gem "${gem_args[@]}" \
    || die "安装 CocoaPods 失败。内网机器请设置 GEM_SOURCE_URL 指向 RubyGems 镜像。"

  local now
  now="$(tool_version "$bin" --version)"
  version_ge "$(numeric_version "$now")" "$REQUIRED_COCOAPODS" \
    || die "装出来的 CocoaPods ${now:-未知} 仍低于要求的 $REQUIRED_COCOAPODS"
  ok "CocoaPods $now"
}

# ── 产出运行上下文 ──────────────────────────────────────────────────────────
write_env_file() {
  local file
  file="$(begin_env_file ios)"

  cat >>"$file" <<EOF

# 工具链装在 \$HOME 下的独立目录，不碰系统目录，也不碰机器上可能已有的
# ~/.pub-cache / ~/.gem。
export FLUTTER_ROOT="$FLUTTER_DIR"
export PUB_CACHE="$PUB_CACHE_DIR"

# 只设 GEM_HOME：gem 会装到这里，但搜索路径仍保留 ruby 自带的 gem 目录。若把
# GEM_PATH 也独占改掉，CocoaPods 依赖的 default gem（base64 等）就找不到了。
export GEM_HOME="$GEM_HOME_DIR"
export PATH="$FLUTTER_DIR/bin:$GEM_HOME_DIR/bin:\$PATH"

# 关掉遥测。用环境变量而不是 flutter config，后者会写 ~/.flutter，那是持久的
# 全局状态。
export FLUTTER_SUPPRESS_ANALYTICS=true
export DART_ANALYTICS_DISABLED=1

# CocoaPods 默认会往终端要交互（比如首次 repo 更新的提示），CI 上必须关掉。
export COCOAPODS_DISABLE_STATS=true
export CP_HOME_DIR="$TOOLCHAIN_ROOT/cocoapods"
EOF

  passthrough_mirrors "$file" FLUTTER_STORAGE_BASE_URL PUB_HOSTED_URL COCOAPODS_CDN_URL
}

main() {
  info "iOS 端构建环境初始化（工具链根目录：${TOOLCHAIN_ROOT}）"
  echo

  preflight
  echo

  setup_flutter
  setup_cocoapods
  echo

  if [[ "$CHECK_ONLY" == 1 ]]; then
    ok "所有工具版本均满足要求"
    return 0
  fi

  write_env_file
  echo
  ok "环境就绪，已写入 .ci/.env/ios.sh"
  echo "  接下来执行构建：./.ci/ios/ci.sh"
}

main "$@"
