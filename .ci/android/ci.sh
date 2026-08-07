#!/usr/bin/env bash
# Android 端（Flutter）的构建执行。环境由 .ci/android/env-setup.sh 准备，本脚本
# 只负责跑。
#
# 用法：
#   ./.ci/android/ci.sh            analyze + test（默认，不需要签名）
#   ./.ci/android/ci.sh analyze    只拉依赖 + 静态分析
#   ./.ci/android/ci.sh test       只跑单元测试
#   ./.ci/android/ci.sh build      编译 release APK
#   ./.ci/android/ci.sh bundle     编译 release AAB（上架 Google Play 用）
#   ./.ci/android/ci.sh all        analyze + test + build
#
# 环境变量：
#   SMELT_CI_SKIP_ENV=1            不 source .ci/.env/android.sh，直接用当前 PATH
#   SMELT_ANDROID_KEYSTORE         签名用的 keystore 文件路径
#   SMELT_ANDROID_KEYSTORE_PASSWORD
#   SMELT_ANDROID_KEY_ALIAS
#   SMELT_ANDROID_KEY_PASSWORD
#   SMELT_ANDROID_ABIS             要编的 ABI，逗号分隔，默认 android-arm64
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../lib" && pwd)/common.sh"

MOBILE_DIR="$REPO_ROOT/mobile"
cd "$REPO_ROOT"

load_ci_env android

command -v flutter >/dev/null 2>&1 || die "PATH 上找不到 flutter，请先运行 ./.ci/android/env-setup.sh"
[[ -n "${ANDROID_SDK_ROOT:-}" ]] || die "ANDROID_SDK_ROOT 未设置，请先运行 ./.ci/android/env-setup.sh"

# 默认只编 arm64。真机几乎全是 arm64，把四个 ABI 都编一遍意味着 Rust 依赖树要
# 交叉编译四份，构建时间翻好几倍。要出多 ABI 包时显式设 SMELT_ANDROID_ABIS。
ANDROID_ABIS="${SMELT_ANDROID_ABIS:-android-arm64}"

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
  # 纯 Dart 测试，不起模拟器：构建机上起一个 AVD 要几分钟且需要 KVM/HAXM。
  # 真机/模拟器集成测试属于另一条流水线。
  (cd "$MOBILE_DIR" && flutter test)
  ok "测试通过"
}

# 把签名信息落成 android/key.properties，供 app/build.gradle.kts 读取。
#
# 不用环境变量直通 Gradle，是因为 Gradle 会把 -P 参数记进构建缓存和各种日志里，
# 密码容易泄进 CI 输出。文件写在 gitignore 覆盖的位置，构建完就删。
KEY_PROPERTIES="$MOBILE_DIR/android/key.properties"

write_keystore_config() {
  [[ -n "${SMELT_ANDROID_KEYSTORE:-}" ]] || return 1

  [[ -f "$SMELT_ANDROID_KEYSTORE" ]] \
    || die "SMELT_ANDROID_KEYSTORE 指向的文件不存在：$SMELT_ANDROID_KEYSTORE"
  local var
  for var in SMELT_ANDROID_KEYSTORE_PASSWORD SMELT_ANDROID_KEY_ALIAS SMELT_ANDROID_KEY_PASSWORD; do
    [[ -n "${!var:-}" ]] || die "设置了 SMELT_ANDROID_KEYSTORE 就必须同时设置 $var"
  done

  # storeFile 走绝对路径：build.gradle.kts 里用 rootProject.file() 解析，
  # 相对路径的基准是 mobile/android/，容易搞错。
  local keystore_abs
  keystore_abs="$(cd "$(dirname "$SMELT_ANDROID_KEYSTORE")" && pwd)/$(basename "$SMELT_ANDROID_KEYSTORE")"

  umask 077
  cat >"$KEY_PROPERTIES" <<EOF
storeFile=$keystore_abs
storePassword=$SMELT_ANDROID_KEYSTORE_PASSWORD
keyAlias=$SMELT_ANDROID_KEY_ALIAS
keyPassword=$SMELT_ANDROID_KEY_PASSWORD
EOF
  info "已写入签名配置：$KEY_PROPERTIES"
  return 0
}

cleanup_keystore_config() { rm -f "$KEY_PROPERTIES"; }

stage_build() {
  local artifact="${1:-apk}"

  if write_keystore_config; then
    trap cleanup_keystore_config EXIT
    info "Android：编译 release ${artifact}（正式签名）"
  else
    # 默认不带签名配置。构建机上放证书是持久的机器状态改动，不该由脚本代劳；
    # 而验证「代码能不能编过」并不需要正式签名——此时 build.gradle.kts 会退回
    # debug 签名，产物能装但不可分发。
    info "未设置 SMELT_ANDROID_KEYSTORE，按 debug 签名编译（仅验证可编译性，产物不可分发）"
  fi

  info "目标 ABI：$ANDROID_ABIS"
  case "$artifact" in
    apk)
      (cd "$MOBILE_DIR" && flutter build apk --release --target-platform "$ANDROID_ABIS")
      ok "产物已生成在 mobile/build/app/outputs/flutter-apk/"
      ;;
    aab)
      (cd "$MOBILE_DIR" && flutter build appbundle --release --target-platform "$ANDROID_ABIS")
      ok "产物已生成在 mobile/build/app/outputs/bundle/"
      ;;
    *) die "未知产物类型：$artifact" ;;
  esac
}

main() {
  local target="${1:-default}"
  local started=$SECONDS

  case "$target" in
    default) stage_analyze; echo; stage_test ;;
    all)     stage_analyze; echo; stage_test; echo; stage_build apk ;;
    analyze) stage_analyze ;;
    test)    stage_deps; stage_test ;;
    build)   stage_deps; stage_build apk ;;
    bundle)  stage_deps; stage_build aab ;;
    *)       die "未知参数：${target}（可用：default / all / analyze / test / build / bundle）" ;;
  esac

  echo
  ok "Android 构建完成，耗时 $((SECONDS - started))s"
}

main "$@"
