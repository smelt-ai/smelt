#!/usr/bin/env bash
# Linux 无头运行时（smeltd）的构建环境初始化。
#
# 检查各工具版本，不达标的就地升级，最后把这次运行需要的环境变量写进
# ci/.env/linux.sh，交给 ci/linux/ci.sh source。
#
# 构建机的三条硬规矩（不写系统目录 / 全局配置只留在当前上下文 / 不复用机器上
# 已有的工具链目录）见 ci/lib/common.sh 顶部。
#
# 不交叉编译：x86_64 和 aarch64 各在对应架构的 Linux 机器上跑这套脚本。
#
# 用法：
#   ./ci/linux/env-setup.sh              检查并按需升级
#   ./ci/linux/env-setup.sh --check      只检查不安装（不达标则退出码非 0）
#
# 可用环境变量覆盖（内网机器连不上外网时指向镜像）：
#   SMELT_CI_TOOLCHAIN_ROOT   工具链安装位置，默认 ~/.smelt-ci/toolchains
#   RUSTUP_UPDATE_ROOT        rustup 自身的下载源
#   RUSTUP_DIST_SERVER        Rust 工具链的下载源
#   NEXTEST_DOWNLOAD_URL      cargo-nextest 预编译包地址
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

# 和 rust-toolchain.toml、ci/mac/env-setup.sh 保持一致。
REQUIRED_RUST="1.97.0"
REQUIRED_NEXTEST="0.9.143"

CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

# ── 前置检查：系统包装不了（没有 sudo），只能报清楚 ─────────────────────────
preflight() {
  require_linux
  case "$(uname -m)" in
    x86_64 | aarch64) ;;
    *) die "不支持的架构：$(uname -m)（只出 x86_64 和 aarch64）" ;;
  esac
  # 内置 SQLite 和几个 -sys crate 要 C 编译器，没有的话编到一半才炸。
  command -v cc >/dev/null 2>&1 \
    || die "缺少 C 编译器。请让构建机管理员安装 build-essential（Debian/Ubuntu）或 gcc（RHEL 系）"
  command -v curl >/dev/null 2>&1 || die "缺少 curl"
}

# ── Rust ────────────────────────────────────────────────────────────────────
# 装到自己的 CARGO_HOME/RUSTUP_HOME 里，不动机器上可能已有的 ~/.cargo。
setup_rust() {
  export CARGO_HOME="$CARGO_HOME_DIR" RUSTUP_HOME="$RUSTUP_HOME_DIR"
  local rustup_bin="$CARGO_HOME_DIR/bin/rustup"
  local have
  have="$(tool_version "$CARGO_HOME_DIR/bin/rustc" --version)"

  if need_install "Rust" "$have" "$REQUIRED_RUST"; then
    [[ "$CHECK_ONLY" == 1 ]] && die "Rust 不满足要求（--check 模式不安装）"
    if [[ ! -x "$rustup_bin" ]]; then
      info "安装 rustup 到 $CARGO_HOME_DIR"
      mkdir -p "$TOOLCHAIN_ROOT"
      local init="$TOOLCHAIN_ROOT/rustup-init.sh"
      curl -fsSL --retry 3 "${RUSTUP_UPDATE_ROOT:-https://sh.rustup.rs}" -o "$init" \
        || die "下载 rustup 失败。内网机器请设置 RUSTUP_UPDATE_ROOT 指向镜像。"
      # --no-modify-path：不往 ~/.profile 之类塞 PATH，PATH 只写进 ci/.env/linux.sh。
      sh "$init" -y --no-modify-path --default-toolchain "$REQUIRED_RUST" --profile minimal \
        || die "rustup 安装失败"
      rm -f "$init"
    else
      info "升级 Rust 到 $REQUIRED_RUST"
      "$rustup_bin" toolchain install "$REQUIRED_RUST" --profile minimal || die "Rust 工具链安装失败"
      "$rustup_bin" default "$REQUIRED_RUST" || die "切换默认工具链失败"
    fi
  fi
  [[ -x "$rustup_bin" ]] || die "找不到 rustup（${rustup_bin}）"

  # minimal profile 不含 rustfmt/clippy，版本达标时也要补。
  if [[ "$CHECK_ONLY" == 1 ]]; then
    local comps
    comps="$("$rustup_bin" component list --toolchain "$REQUIRED_RUST" --installed 2>/dev/null || true)"
    grep -q '^rustfmt' <<<"$comps" || die "缺少 rustfmt（--check 模式不安装）"
    grep -q '^clippy' <<<"$comps" || die "缺少 clippy（--check 模式不安装）"
  else
    "$rustup_bin" component add rustfmt clippy --toolchain "$REQUIRED_RUST" \
      || die "安装 rustfmt/clippy 失败"
  fi
  ok "Rust $(tool_version "$CARGO_HOME_DIR/bin/rustc" --version)"
}

# ── cargo-nextest ───────────────────────────────────────────────────────────
# smeltd 的交接测试摆弄进程级全局状态（fd 表、waitpid、进程组），要每个测试一个进程。
setup_nextest() {
  local bin="$CARGO_HOME_DIR/bin/cargo-nextest"
  local have
  have="$(tool_version "$bin" nextest --version)"
  need_install "cargo-nextest" "$have" "$REQUIRED_NEXTEST" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "cargo-nextest 不满足要求（--check 模式不安装）"

  local platform
  case "$(uname -m)" in
    x86_64) platform="linux" ;;
    aarch64) platform="linux-arm" ;;
  esac
  info "安装 cargo-nextest"
  mkdir -p "$CARGO_HOME_DIR/bin"
  curl -fsSL --retry 3 "${NEXTEST_DOWNLOAD_URL:-https://get.nexte.st/latest/$platform}" \
    | tar zxf - -C "$CARGO_HOME_DIR/bin" \
    || die "下载 cargo-nextest 失败。内网机器请设置 NEXTEST_DOWNLOAD_URL 指向镜像。"
  ok "cargo-nextest $(tool_version "$bin" nextest --version)"
}

write_env_file() {
  local file
  file="$(begin_env_file linux)"
  cat >>"$file" <<EOT

export CARGO_HOME="$CARGO_HOME_DIR"
export RUSTUP_HOME="$RUSTUP_HOME_DIR"
export PATH="$CARGO_HOME_DIR/bin:\$PATH"

# CI 上不需要增量编译产物，关掉省磁盘。
export CARGO_INCREMENTAL=0
export CARGO_NET_RETRY=5
EOT
  passthrough_mirrors "$file" RUSTUP_DIST_SERVER
}

main() {
  info "Linux 运行时构建环境初始化（$(uname -m)，工具链根目录：${TOOLCHAIN_ROOT}）"
  echo

  preflight
  setup_rust
  setup_nextest
  echo

  if [[ "$CHECK_ONLY" == 1 ]]; then
    ok "所有工具版本均满足要求"
    return 0
  fi

  write_env_file
  ok "环境就绪，已写入 ci/.env/linux.sh"
  echo "  接下来执行构建：./ci/linux/ci.sh"
}

main "$@"
