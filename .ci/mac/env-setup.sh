#!/usr/bin/env bash
# macOS 桌面端（Rust + GPUI）的构建环境初始化。
#
# 检查各工具版本，不达标的就地升级，最后把这次运行需要的环境变量写进
# .ci/.env/mac.sh，交给 .ci/mac/ci.sh source。
#
# 构建机的三条硬规矩（不写系统目录 / 全局配置只留在当前上下文 / 不复用机器上
# 已有的工具链目录）见 .ci/lib/common.sh 顶部。
#
# 用法：
#   ./.ci/mac/env-setup.sh              检查并按需升级
#   ./.ci/mac/env-setup.sh --check      只检查不安装（不达标则退出码非 0）
#
# 可用环境变量覆盖（内网机器如果连不上外网，用这些指向镜像）：
#   SMELT_CI_TOOLCHAIN_ROOT   工具链安装位置，默认 ~/.smelt-ci/toolchains
#   RUSTUP_UPDATE_ROOT        rustup 自身的下载源
#   RUSTUP_DIST_SERVER        Rust 工具链的下载源
#   NEXTEST_DOWNLOAD_URL      cargo-nextest 预编译包地址
#   CARGO_REGISTRY_MIRROR     crates.io 镜像（会写成 source replacement）
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

# ── 版本要求：单一事实来源 ──────────────────────────────────────────────────
# 都是「最低版本」，高于它不动，低于它才升级。定这几个数的依据：
#
# Rust      工作区是 edition 2024。1.97.0 是本地与 GitHub CI 验证过的版本。
# nextest   smeltd 的 resume_handoff 测试摆弄进程级全局状态（fd 表、waitpid、
#           进程组），同进程跑会互相串味。nextest 每个测试一个独立进程才稳。
# Python    只有打包（scripts/package-mac.sh）用得到，且它要求 >= 3.10。装 Python
#           要管理员权限，这里只检查不安装。
REQUIRED_RUST="1.97.0"
REQUIRED_NEXTEST="0.9.143"
REQUIRED_PYTHON="3.10"

CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

# ── 前置检查：这些装不了，只能报清楚 ────────────────────────────────────────
preflight() {
  require_macos
  require_xcode_clt

  # 打包脚本会用 file 校验产物必须是 arm64，Intel 机器直接拒绝，早点说清楚。
  if [[ "$(uname -m)" != "arm64" ]]; then
    warn "当前是 $(uname -m)，不是 arm64。跑测试没问题，但 .ci/mac/ci.sh package 打出的包会被 package-mac.sh 拒收。"
  fi

  local py_version=""
  if command -v python3 >/dev/null 2>&1; then
    py_version="$(python3 -c 'import sys; print(".".join(map(str, sys.version_info[:3])))' 2>/dev/null || true)"
  fi
  if [[ -z "$py_version" ]] || ! version_ge "$py_version" "$REQUIRED_PYTHON"; then
    # 只有打包用得到，所以不 die——跑测试的流水线不该被它挡住。
    warn "Python ${py_version:-未安装} 低于打包所需的 ${REQUIRED_PYTHON}。只影响 .ci/mac/ci.sh package，不影响构建与测试。"
    warn "  需要打包的话，让管理员装一个 3.10+，或用 SMELT_PYTHON 指向已有的解释器。"
  else
    ok "Python ${py_version}（>= ${REQUIRED_PYTHON}，打包用）"
  fi
}

# ── Rust ────────────────────────────────────────────────────────────────────
# 装到自己的 CARGO_HOME/RUSTUP_HOME 里，不动机器上可能已有的 ~/.cargo。
setup_rust() {
  export CARGO_HOME="$CARGO_HOME_DIR" RUSTUP_HOME="$RUSTUP_HOME_DIR"
  local rustup_bin="$CARGO_HOME_DIR/bin/rustup"
  local have=""

  have="$(tool_version "$CARGO_HOME_DIR/bin/rustc" --version)"

  need_install "Rust" "$have" "$REQUIRED_RUST" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "Rust 不满足要求（--check 模式不安装）"

  if [[ ! -x "$rustup_bin" ]]; then
    info "安装 rustup 到 $CARGO_HOME_DIR"
    mkdir -p "$TOOLCHAIN_ROOT"
    local init="$TOOLCHAIN_ROOT/rustup-init.sh"
    curl -fsSL --retry 3 "${RUSTUP_UPDATE_ROOT:-https://sh.rustup.rs}" -o "$init" \
      || die "下载 rustup 失败。内网机器请设置 RUSTUP_UPDATE_ROOT 指向镜像。"
    # --no-modify-path 是关键：默认行为会往 ~/.profile、~/.bashrc 里塞 PATH，
    # 那是对构建机的永久污染。PATH 由本脚本写进 .ci/.env/mac.sh，只在本次运行内有效。
    sh "$init" -y --no-modify-path --default-toolchain "$REQUIRED_RUST" --profile minimal \
      || die "rustup 安装失败"
    rm -f "$init"
  else
    info "升级 Rust 到 $REQUIRED_RUST"
    "$rustup_bin" toolchain install "$REQUIRED_RUST" --profile minimal || die "Rust 工具链安装失败"
    "$rustup_bin" default "$REQUIRED_RUST" || die "切换默认工具链失败"
  fi

  ok "Rust $(tool_version "$CARGO_HOME_DIR/bin/rustc" --version)"
}

# ── cargo-nextest ───────────────────────────────────────────────────────────
setup_nextest() {
  local bin="$CARGO_HOME_DIR/bin/cargo-nextest"
  local have=""
  have="$(tool_version "$bin" nextest --version)"

  need_install "cargo-nextest" "$have" "$REQUIRED_NEXTEST" || return 0
  [[ "$CHECK_ONLY" == 1 ]] && die "cargo-nextest 不满足要求（--check 模式不安装）"

  info "安装 cargo-nextest"
  mkdir -p "$CARGO_HOME_DIR/bin"
  # 官方预编译包，比 cargo install 从源码编快一个数量级（后者要拉一堆依赖）。
  curl -fsSL --retry 3 "${NEXTEST_DOWNLOAD_URL:-https://get.nexte.st/latest/mac}" \
    | tar zxf - -C "$CARGO_HOME_DIR/bin" \
    || die "下载 cargo-nextest 失败。内网机器请设置 NEXTEST_DOWNLOAD_URL 指向镜像。"
  ok "cargo-nextest $(tool_version "$bin" nextest --version)"
}

# ── 产出运行上下文 ──────────────────────────────────────────────────────────
write_env_file() {
  local file
  file="$(begin_env_file mac)"

  cat >>"$file" <<EOF

# 工具链装在 \$HOME 下的独立目录，不碰系统目录，也不碰机器上可能已有的
# ~/.cargo / ~/.rustup。
export CARGO_HOME="$CARGO_HOME_DIR"
export RUSTUP_HOME="$RUSTUP_HOME_DIR"
export PATH="$CARGO_HOME_DIR/bin:\$PATH"

# CI 上不需要增量编译产物，关掉省磁盘；构建机磁盘是共享资源。
export CARGO_INCREMENTAL=0
# 网络抖动时给 cargo 多试几次，构建机的外网出口通常不如开发机稳。
export CARGO_NET_RETRY=5
EOF

  # 镜像配置是可选的：内网机器连不上 crates.io 时才需要。写进 CARGO_HOME 下的
  # config.toml 而不是 ~/.cargo/config.toml，同样是为了不影响别的项目。
  if [[ -n "${CARGO_REGISTRY_MIRROR:-}" ]]; then
    mkdir -p "$CARGO_HOME_DIR"
    cat >"$CARGO_HOME_DIR/config.toml" <<EOF
[source.crates-io]
replace-with = "mirror"

[source.mirror]
registry = "$CARGO_REGISTRY_MIRROR"
EOF
    info "已配置 crates.io 镜像：$CARGO_REGISTRY_MIRROR"
  fi

  passthrough_mirrors "$file" RUSTUP_DIST_SERVER
}

main() {
  info "macOS 端构建环境初始化（工具链根目录：${TOOLCHAIN_ROOT}）"
  echo

  preflight
  echo

  setup_rust
  setup_nextest
  echo

  if [[ "$CHECK_ONLY" == 1 ]]; then
    ok "所有工具版本均满足要求"
    return 0
  fi

  write_env_file
  echo
  ok "环境就绪，已写入 .ci/.env/mac.sh"
  echo "  接下来执行构建：./.ci/mac/ci.sh"
}

main "$@"
