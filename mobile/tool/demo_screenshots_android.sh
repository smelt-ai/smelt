#!/usr/bin/env bash
# 在 Android 模拟器 / 设备上用演示数据把每一屏截下来，供视觉审查。
#
#   flutter build apk --debug --target-platform android-arm64 --dart-define=SMELT_DEMO=true
#   tool/demo_screenshots_android.sh <输出前缀> [屏幕…]
#
# 屏幕名见 lib/data/demo/demo_screens.dart；不给就全截。每屏截浅色和深色两张。
# 目标屏写进 App 数据目录（debug 包才能 run-as），App 读完即删。
set -euo pipefail
# 每屏启动后等多久再截（秒）。刚开机的模拟器、debug 包冷启动慢，截到启动页就加大它。
settle=${SETTLE:-2}
prefix=${1:?输出前缀，如 review/phone-android}
shift
screens=("$@")
[ ${#screens[@]} -eq 0 ] && screens=(pass triage conversation approval stamped terminal terminal-offline sessions agents settings machine launch)
pkg=ai.smelt.mobile
adb install -r build/app/outputs/flutter-apk/app-debug.apk >/dev/null
activity=$(adb shell cmd package resolve-activity --brief "${pkg}" | tail -1 | tr -d '\r')
for mode in no yes; do
  adb shell cmd uimode night "${mode}" >/dev/null
  for screen in "${screens[@]}"; do
    adb shell am force-stop "${pkg}"
    adb shell "run-as ${pkg} sh -c 'mkdir -p files && printf %s ${screen} > files/demo-screen'"
    adb logcat -c
    adb shell am start -W -n "${activity}" >/dev/null
    # 等 App 说「演示就绪」（跳到目标屏、那一帧画完之后才打），再截。
    # 固定等待不可靠：负载高、冷启动的模拟器上会截到启动页。
    ready=
    for _ in $(seq 1 120); do
      if adb logcat -d -s flutter 2>/dev/null | grep -q "Smelt 演示就绪：${screen}"; then
        ready=1
        break
      fi
      sleep 1
    done
    if [ -z "${ready}" ]; then
      echo "等不到 ${screen} 画完，停止截图" >&2
      exit 1
    fi
    sleep "${settle}"
    # 截之前确认前台是我们：设备上开着别的 App 时，截到的就是它，这种截图不能用。
    if ! adb shell dumpsys activity activities | grep -q "topResumedActivity=.*${pkg}/"; then
      echo "前台不是 ${pkg}，停止截图（${screen}）" >&2
      exit 1
    fi
    name=$([ "${mode}" = no ] && echo light || echo dark)
    adb exec-out screencap -p > "${prefix}-${screen}-${name}.png"
    echo "${prefix}-${screen}-${name}.png"
  done
done
adb shell cmd uimode night no >/dev/null
