#!/usr/bin/env bash
# 在 iOS 模拟器上用演示数据把每一屏截下来，供视觉审查。
#
#   flutter build ios --simulator --debug --dart-define=SMELT_DEMO=true
#   tool/demo_screenshots.sh <模拟器 UDID> <输出前缀> [屏幕…]
#
# 屏幕名见 lib/data/demo/demo_screens.dart；不给就全截。每屏截浅色和深色两张。
set -euo pipefail
# 每屏启动后等多久再截（秒）。刚开机的模拟器、debug 包冷启动慢，截到启动页就加大它。
settle=${SETTLE:-6}
udid=${1:?模拟器 UDID}
prefix=${2:?输出前缀，如 review/phone-ios}
shift 2
screens=("$@")
[ ${#screens[@]} -eq 0 ] && screens=(pass triage conversation approval stamped terminal terminal-offline sessions agents settings machine launch)
app=build/ios/iphonesimulator/Runner.app
xcrun simctl install "${udid}" "${app}"
# iOS 上 Dart 读不到 simctl 传的环境变量：把屏幕名写进 App 数据目录，App 读完即删。
support="$(xcrun simctl get_app_container "${udid}" ai.smelt.mobile data)/Library/Application Support"
mkdir -p "${support}"
for mode in light dark; do
  xcrun simctl ui "${udid}" appearance "${mode}"
  for screen in "${screens[@]}"; do
    xcrun simctl terminate "${udid}" ai.smelt.mobile >/dev/null 2>&1 || true
    printf '%s' "${screen}" > "${support}/demo-screen"
    xcrun simctl launch "${udid}" ai.smelt.mobile >/dev/null
    sleep "${settle}"
    xcrun simctl io "${udid}" screenshot "${prefix}-${screen}-${mode}.png" >/dev/null 2>&1
    echo "${prefix}-${screen}-${mode}.png"
  done
done
