#!/usr/bin/env bash
# macOS 桌面端（Rust + GPUI）的构建执行。环境由 ci/mac/env-setup.sh 准备，
# 本脚本只负责跑。
#
# 用法：
#   ./ci/mac/ci.sh            build + test（默认）
#   ./ci/mac/ci.sh build      只编译
#   ./ci/mac/ci.sh test       只跑测试
#   ./ci/mac/ci.sh package    编 release 并打 dmg（产物在 dist/）
#
# 环境变量：
#   SMELT_CI_SKIP_ENV=1          不 source ci/.env/mac.sh，直接用当前 PATH 上的工具
#   SMELT_CI_ALLOW_LONG_PATH=1   跳过 unix socket 路径长度检查（见 check_path_budget）
#   CARGO_BUILD_JOBS             限制并行度。构建机多项目共用时值得设，GPUI 编译很吃内存
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"
cd "$REPO_ROOT"

load_ci_env mac

for tool in cargo cargo-nextest; do
  command -v "$tool" >/dev/null 2>&1 || die "PATH 上找不到 ${tool}，请先运行 ./ci/mac/env-setup.sh"
done

# 这段排除表与 .github/workflows/ci.yml 保持一致，两边都改才有意义。原因：
#
# - damage_gate_tests 走 Terminal::spawn 的完整生产路径，要真 PTY、真 shell，以及
#   本机活着的 smeltd。构建机上没有。
# - short_ordered_list_bubble_does_not_collapse_below_min_width 在 main 上就是红的：
#   gpui 的测试平台对 test window 抛 "not implemented: Test Windows are not backed
#   by a real platform window"，是依赖升级带来的，与被测逻辑无关。
NEXTEST_FILTER='
  not test(damage_gate_tests)
  and not test(short_ordered_list_bubble_does_not_collapse_below_min_width)
'

# smeltd 的测试要 bind unix socket，路径是从源码目录推出来的：
#
#   <仓库根>/target/smeltd-tests/s-<pid>-<16位哈希>.sock
#
# macOS 的 sockaddr_un.sun_path 只有 104 字节（含结尾 NUL），超了 bind 直接返回
# "path must be shorter than SUN_LEN"。这条路径不受 CARGO_TARGET_DIR 影响——它是
# 从 CARGO_MANIFEST_DIR 算的，所以只能靠检出目录本身够短。
#
# 拦在这里是因为不拦的话，报错要等到编译几分钟之后才出现，而且那句错误看不出跟
# 工作区路径有关，构建机上极难排查。
check_path_budget() {
  [[ "${SMELT_CI_ALLOW_LONG_PATH:-0}" == 1 ]] && return 0

  # 后缀最长的情况：/target/smeltd-tests/ + s- + 5 位 pid + - + 16 位哈希 + .sock
  local suffix_max=50
  local sun_len_max=103          # 104 减掉结尾的 NUL
  local budget=$((sun_len_max - suffix_max))

  if (( ${#REPO_ROOT} > budget )); then
    die "检出路径过长：${#REPO_ROOT} 字符，上限 ${budget}
  $REPO_ROOT

  smeltd 的测试要在 <仓库根>/target/ 下 bind unix socket，macOS 限制整条路径不超过
  ${sun_len_max} 字符。超了会有十来个测试报 \"path must be shorter than SUN_LEN\"。
  请把工作区换到更短的路径（例如 /Users/ci/w/smelt）后重试。
  确认无需跑 smeltd 测试时，可用 SMELT_CI_ALLOW_LONG_PATH=1 跳过本检查。"
  fi
}

stage_build() {
  info "Rust：编译"
  # --locked 保证严格按 Cargo.lock 解析。gpui 那条 git 依赖在 Cargo.toml 里没锁
  # rev，只有 lock 文件锁了；不加这个参数，构建机可能悄悄拉到上游新提交，编出来
  # 的东西和本地验证过的不是同一份。
  cargo build --workspace --locked
  ok "编译通过"
}

stage_test() {
  check_path_budget
  info "Rust：测试"
  # 用 nextest 而不是 cargo test：smeltd 的 resume_handoff 测试摆弄进程级全局状态
  # （fd 表、waitpid、进程组），同进程跑会互相串味，大约五次挂一次。nextest 每个
  # 测试一个独立进程，这个干扰就不存在了。
  cargo nextest run --workspace --locked -E "$NEXTEST_FILTER"
  ok "测试通过"
}

stage_package() {
  info "打包 macOS 产物"
  [[ "$(uname -m)" == "arm64" ]] \
    || die "打包必须在 arm64 机器上：package-mac.sh 会用 file 校验产物架构，Intel 直接拒绝"

  # dmgbuild 需要 Python >= 3.10。package-mac.sh 自己会建 venv 装依赖，不污染系统
  # python，这里只把版本不够的情况提前拦下来——否则要等 release 编译完才报错。
  local py="${SMELT_PYTHON:-python3}"
  command -v "$py" >/dev/null 2>&1 || die "找不到 ${py}，打包需要 Python >= 3.10"
  "$py" -c 'import sys; sys.exit(sys.version_info < (3, 10))' \
    || die "$py 版本低于 3.10，打包会失败。用 SMELT_PYTHON 指向更新的解释器。"

  make dist-build
  ok "产物已生成在 dist/"
}

main() {
  local target="${1:-all}"
  local started=$SECONDS

  case "$target" in
    all)     stage_build; echo; stage_test ;;
    build)   stage_build ;;
    test)    stage_test ;;
    package) stage_package ;;
    *)       die "未知参数：${target}（可用：all / build / test / package）" ;;
  esac

  echo
  ok "mac 构建完成，耗时 $((SECONDS - started))s"
}

main "$@"
