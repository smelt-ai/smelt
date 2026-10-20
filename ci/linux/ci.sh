#!/usr/bin/env bash
# Linux 无头运行时（smeltd）的构建执行。环境由 ci/linux/env-setup.sh 准备，本脚本
# 只负责跑。只构建运行时这一侧的 crate：桌面 GUI 只有 macOS 版。
#
# 不交叉编译：在哪种架构的 Linux 机器上跑，就出哪种架构的包。
#
# 用法：
#   ./ci/linux/ci.sh              lint + build + test（默认）
#   ./ci/linux/ci.sh lint         rustfmt + clippy
#   ./ci/linux/ci.sh build        只编译
#   ./ci/linux/ci.sh test         只跑测试
#   ./ci/linux/ci.sh package      编 release，打 dist/smeltd-<版本>-linux-<架构>.tar.gz
#
# 环境变量：
#   SMELT_CI_SKIP_ENV=1           不 source ci/.env/linux.sh，直接用当前 PATH 上的工具
#   CARGO_BUILD_JOBS              限制并行度
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"
cd "$REPO_ROOT"

require_linux
load_ci_env linux

for tool in cargo cargo-nextest; do
  command -v "$tool" >/dev/null 2>&1 || die "PATH 上找不到 ${tool}，请先运行 ./ci/linux/env-setup.sh"
done

# 运行时 smeltd 及它依赖的工作区 crate。不能用 --workspace：GUI 那几个 crate 依赖
# GPUI，只能在 macOS 上编。
RUNTIME_PACKAGES=(
  smelt-paths smelt-sys smelt-model smelt-event-protocol smelt-event-bus smelt-store
  smelt-pairing smelt-runtime-client smelt-core smelt-git smelt-iroh smeltd
)
PACKAGE_ARGS=()
for package in "${RUNTIME_PACKAGES[@]}"; do
  PACKAGE_ARGS+=(-p "$package")
done

# 随运行时一起分发的程序。
PROGRAMS=(smeltd smelt-notify)

stage_lint() {
  info "光标：禁止手型"
  "$REPO_ROOT/ci/lint-pointer.sh"
  ok "没有手型光标"
  info "Rust：格式"
  cargo fmt --check "${PACKAGE_ARGS[@]}"
  ok "rustfmt 通过"
  info "Rust：Clippy"
  # 和 mac 一样不传 -D warnings：正确性闸门在 workspace.lints deny。
  cargo clippy "${PACKAGE_ARGS[@]}" --all-targets --locked
  ok "Clippy 通过"
}

stage_build() {
  info "Rust：编译"
  cargo build "${PACKAGE_ARGS[@]}" --locked
  ok "编译通过"
}

stage_test() {
  info "Rust：测试"
  # nextest 每个测试一个进程：smeltd 的交接测试摆弄进程级全局状态。
  cargo nextest run "${PACKAGE_ARGS[@]}" --locked
  ok "测试通过"
}

workspace_version() {
  awk -F'"' '/^\[workspace.package\]/{inside=1; next} /^\[/{inside=0} inside && /^version *=/{print $2; exit}' Cargo.toml
}

stage_package() {
  local version arch name staging
  version="$(workspace_version)"
  [[ -n "$version" ]] || die "读不到工作区版本号"
  arch="$(uname -m)"
  name="smeltd-${version}-linux-${arch}"
  info "打包 ${name}"
  cargo build --release -p smeltd --bins --locked

  staging="dist/$name"
  rm -rf "$staging" "dist/$name.tar.gz"
  mkdir -p "$staging"
  local program
  for program in "${PROGRAMS[@]}"; do
    install -m 0755 "target/release/$program" "$staging/$program"
  done
  install -m 0755 scripts/install-smeltd.sh "$staging/install.sh"
  install -m 0755 scripts/smelt-up.sh "$staging/smelt-up"
  # 远程桌面（install.sh --with-desktop）的启动脚本、XFCE 设置、Chrome 策略。
  cp -R scripts/desktop "$staging/desktop"
  tar -C dist -czf "dist/$name.tar.gz" "$name"
  rm -rf "$staging"
  ok "产物：dist/$name.tar.gz"
}

case "${1:-all}" in
  all)
    stage_lint
    stage_build
    stage_test
    ;;
  lint) stage_lint ;;
  build) stage_build ;;
  test) stage_test ;;
  package) stage_package ;;
  *) die "未知阶段：$1（可选 lint / build / test / package）" ;;
esac
