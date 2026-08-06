#!/usr/bin/env bash
# 各平台 CI 脚本共用的地基。由 .ci/<平台>/*.sh source，不单独执行。
#
# ────────────────────────────────────────────────────────────────────────────
# 这些机器是 UP 发布系统的构建机，多个项目共用，且没有 sudo。据此有三条硬规矩，
# 本文件和所有调用方都必须守住：
#
# 1. 一律不往系统目录写（/usr/local、/opt、/Library …）。所有工具链装在
#    $SMELT_CI_TOOLCHAIN_ROOT 下，默认 ~/.smelt-ci/toolchains。装在 $HOME 而不是
#    工作区里，是因为编译 GPUI 那堆 git 依赖、下载 Flutter 引擎产物都非常慢，
#    跨次构建复用能省大量时间——这类通用工具留在机器上是合理的。
#
# 2. 全局配置只留在当前运行上下文里。不写 ~/.bashrc、~/.zshrc、~/.profile，不碰
#    git config --global，不跑 flutter config（那会写 ~/.flutter）。所有设置都以
#    环境变量形式写进 .ci/.env/<平台>.sh，进程退出即失效。
#
# 3. 不复用机器上已有的 ~/.cargo、~/.rustup、~/.pub-cache、~/.gem。那些可能是
#    管理员或别的项目在用的，往里装东西等于改别人的环境。我们用 CARGO_HOME /
#    RUSTUP_HOME / PUB_CACHE / GEM_HOME 指到自己的目录里，互不干扰。

# 允许被重复 source（mac/ios 的 env-setup 与 ci 可能在同一 shell 里串跑）。
[[ -n "${SMELT_CI_COMMON_LOADED:-}" ]] && return 0
SMELT_CI_COMMON_LOADED=1

# 从本文件位置反推，调用方在哪个目录、怎么被调用都不影响。
CI_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CI_DIR="$(dirname "$CI_LIB_DIR")"
REPO_ROOT="$(dirname "$CI_DIR")"
ENV_DIR="$CI_DIR/.env"

# 工具链根目录。mac 用它下面的 cargo/rustup，ios 用 flutter/pub-cache/gems，
# 各占各的子目录，两个平台可以共存在同一台机器上。
TOOLCHAIN_ROOT="${SMELT_CI_TOOLCHAIN_ROOT:-$HOME/.smelt-ci/toolchains}"

CARGO_HOME_DIR="$TOOLCHAIN_ROOT/cargo"
RUSTUP_HOME_DIR="$TOOLCHAIN_ROOT/rustup"
FLUTTER_DIR="$TOOLCHAIN_ROOT/flutter"
PUB_CACHE_DIR="$TOOLCHAIN_ROOT/pub-cache"
GEM_HOME_DIR="$TOOLCHAIN_ROOT/gems"

info() { printf '\033[36m▶\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '\033[33m⚠\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# ── 版本处理 ────────────────────────────────────────────────────────────────

# 版本比较不用 sort -V：macOS 自带的是 BSD sort，老版本没有 -V，构建机上不能赌。
# awk 按点分段比，缺的段当 0（3.12 与 3.12.0 相等）。
version_ge() {
  awk -v have="$1" -v want="$2" '
    BEGIN {
      n = split(have, a, "."); m = split(want, b, ".");
      max = (n > m ? n : m);
      for (i = 1; i <= max; i++) {
        x = (i <= n ? a[i] + 0 : 0);
        y = (i <= m ? b[i] + 0 : 0);
        if (x > y) exit 0;
        if (x < y) exit 1;
      }
      exit 0;
    }'
}

# 版本号可能带后缀（1.97.0-nightly、3.44.8-hotfix.1），只取前面的数字部分。
numeric_version() { sed -E 's/[^0-9.].*$//; s/\.+$//' <<<"$1"; }

# 取工具版本号：扫 --version 首行，挑第一个长得像版本号的字段。
#
# 不写死「取第 2 个字段」是因为各家格式不一致：rustc 是「rustc 1.97.0 (...)」，
# 而 pod 只印一个裸的「1.15.2」。按形状找更稳。
#
# 另一个关键是那个 `|| true`：工具可能存在但跑不起来——机器上留着一个残缺的
# rustup shim 就是典型情况，它会报错退出。此时开了 pipefail 的话，`var=$(...)`
# 的退出码就是非 0，set -e 会把脚本静默打死。而这种情况正应该当作「没装好」继续
# 走安装流程，不是终止。
tool_version() {
  local bin="$1"; shift
  [[ -x "$bin" ]] || return 0
  local out
  out="$("$bin" "$@" 2>/dev/null || true)"
  awk 'NR==1 {
    for (i = 1; i <= NF; i++)
      if ($i ~ /^v?[0-9]+\.[0-9]+/) { sub(/^v/, "", $i); print $i; exit }
  }' <<<"$out"
}

# 返回 0 表示「要装/要升级」，返回 1 表示「已达标」。
need_install() {
  local what="$1" have="$2" want="$3"
  if [[ -z "$have" ]]; then
    info "$what 未安装（需要 >= ${want}）"
    return 0
  fi
  if version_ge "$(numeric_version "$have")" "$want"; then
    ok "$what ${have}（>= ${want}）"
    return 1
  fi
  info "$what $have 低于要求的 ${want}，需要升级"
  return 0
}

# ── 前置检查 ────────────────────────────────────────────────────────────────

require_macos() {
  [[ "$(uname -s)" == "Darwin" ]] \
    || die "只支持 macOS，当前是 $(uname -s)"
}

# 检查 Xcode Command Line Tools。编译要链接系统框架，没有 CLT 会在编译到一半才
# 炸，且报错很难懂。装它需要管理员权限，我们装不了，只能报清楚。
require_xcode_clt() {
  xcode-select -p >/dev/null 2>&1 \
    || die "缺少 Xcode Command Line Tools。装它需要管理员权限，请联系构建机管理员执行：xcode-select --install"
}

# ── 运行上下文 ──────────────────────────────────────────────────────────────

# 每个平台一个 env 文件。分开是因为两个平台的流水线可能跑在不同机器上，各自
# env-setup 后互不覆盖。
env_file_for() { echo "$ENV_DIR/$1.sh"; }

# ci.sh 用：加载本平台的运行上下文。
#
# 环境变量全部来自这个文件，作用域仅限本进程。构建机的标准环境不会被改动，这也
# 意味着不 source 它就找不到工具链——所以缺了要报清楚，而不是让后面的命令抛一句
# 难懂的 command not found。
load_ci_env() {
  local platform="$1"
  local file
  file="$(env_file_for "$platform")"

  [[ "${SMELT_CI_SKIP_ENV:-0}" == 1 ]] && return 0

  if [[ ! -f "$file" ]]; then
    die "没找到 .ci/.env/${platform}.sh，请先运行：./.ci/${platform}/env-setup.sh
  （若确认工具链已在 PATH 上，可用 SMELT_CI_SKIP_ENV=1 跳过）"
  fi
  # shellcheck source=/dev/null
  source "$file"
}

# 写 env 文件的公共头部。各平台在此基础上追加自己那几行。
begin_env_file() {
  local platform="$1"
  local file
  file="$(env_file_for "$platform")"
  mkdir -p "$ENV_DIR"
  cat >"$file" <<EOF
# 由 .ci/${platform}/env-setup.sh 生成，请勿手改。
# 重新生成：./.ci/${platform}/env-setup.sh
#
# 这里的每一项都只在 source 它的那个 shell 里生效。之所以走文件而不是直接改
# ~/.bashrc 之类，就是为了不污染构建机的标准环境——这些机器是多项目共用的。
EOF
  echo "$file"
}

# 把设置过的镜像地址透传进 env 文件，供 ci.sh 阶段继续用。
passthrough_mirrors() {
  local file="$1"; shift
  local var
  for var in "$@"; do
    if [[ -n "${!var:-}" ]]; then
      echo "export $var=\"${!var}\"" >>"$file"
      info "透传镜像配置：$var"
    fi
  done
}
