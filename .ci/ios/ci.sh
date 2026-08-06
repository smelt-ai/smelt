#!/usr/bin/env bash
# iOS 端（Flutter）的构建执行。环境由 .ci/ios/env-setup.sh 准备，本脚本只负责跑。
#
# 用法：
#   ./.ci/ios/ci.sh            analyze + test（默认，不需要签名）
#   ./.ci/ios/ci.sh analyze    只拉依赖 + 静态分析
#   ./.ci/ios/ci.sh test       只跑单元测试
#   ./.ci/ios/ci.sh build      编译 iOS release 产物
#   ./.ci/ios/ci.sh all        以上全部
#
# 环境变量：
#   SMELT_CI_SKIP_ENV=1          不 source .ci/.env/ios.sh，直接用当前 PATH 上的工具
#   SMELT_IOS_EXPORT_OPTIONS     指向 exportOptions.plist，设了就出可分发的 .ipa；
#                                不设则只编 .app 且不签名（见 stage_build）
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

MOBILE_DIR="$REPO_ROOT/mobile"
cd "$REPO_ROOT"

load_ci_env ios

command -v flutter >/dev/null 2>&1 || die "PATH 上找不到 flutter，请先运行 ./.ci/ios/env-setup.sh"

stage_deps() {
  info "Flutter：拉依赖"
  # 也是在验证 xterm 那条 git 依赖能解析：它指向 smelt-ai/xterm.dart 的一个
  # commit，pub 上没有。内网机器如果访问不了 github.com，会在这一步失败。
  (cd "$MOBILE_DIR" && flutter pub get)
}

stage_analyze() {
  stage_deps
  info "Flutter：静态分析"
  (cd "$MOBILE_DIR" && flutter analyze)
  ok "静态分析通过"
}

stage_test() {
  info "Flutter：单元测试"
  # 纯 Dart 测试，不起模拟器：构建机上通常没有可用的 Simulator runtime，而且起
  # 一个要几十秒。真机/模拟器集成测试属于另一条流水线。
  (cd "$MOBILE_DIR" && flutter test)
  ok "测试通过"
}

stage_build() {
  info "iOS：编译 release"

  if [[ -n "${SMELT_IOS_EXPORT_OPTIONS:-}" ]]; then
    [[ -f "$SMELT_IOS_EXPORT_OPTIONS" ]] \
      || die "SMELT_IOS_EXPORT_OPTIONS 指向的文件不存在：$SMELT_IOS_EXPORT_OPTIONS"
    info "使用导出配置：$SMELT_IOS_EXPORT_OPTIONS"
    (cd "$MOBILE_DIR" && flutter build ipa --release \
      --export-options-plist="$SMELT_IOS_EXPORT_OPTIONS")
    ok "产物已生成在 mobile/build/ios/ipa/"
  else
    # 默认不签名。构建机上装证书和描述文件要操作 login keychain，那是持久的机器
    # 状态改动，也不该由脚本代劳；而验证「代码能不能编过」并不需要签名。要出可
    # 分发的包时，由流水线自己备好证书并设 SMELT_IOS_EXPORT_OPTIONS。
    info "未设置 SMELT_IOS_EXPORT_OPTIONS，按不签名方式编译（仅验证可编译性）"
    (cd "$MOBILE_DIR" && flutter build ios --release --no-codesign)
    ok "产物已生成在 mobile/build/ios/iphoneos/"
  fi
}

main() {
  local target="${1:-default}"
  local started=$SECONDS

  case "$target" in
    default) stage_analyze; echo; stage_test ;;
    all)     stage_analyze; echo; stage_test; echo; stage_build ;;
    analyze) stage_analyze ;;
    test)    stage_deps; stage_test ;;
    build)   stage_deps; stage_build ;;
    *)       die "未知参数：${target}（可用：default / all / analyze / test / build）" ;;
  esac

  echo
  ok "iOS 构建完成，耗时 $((SECONDS - started))s"
}

main "$@"
