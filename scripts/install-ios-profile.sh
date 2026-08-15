#!/usr/bin/env bash
# 把 profile 版 Smelt Mobile 重新构建并装到 iPhone 上。
#
# 解决的问题：用免费 Apple ID 签名时，provisioning profile 只有 7 天有效期
# （证书本身是一年，过期的从来不是它）。到期后 app 在手机上直接起不来，只能
# 重签一次——也就是重新构建 + 重装。这个循环一周一次，不该每次都去翻命令。
#
# 用法：
#   ./scripts/install-ios-profile.sh              # 自动挑唯一一台已连接的 iPhone
#   ./scripts/install-ios-profile.sh -d <UDID>    # 接了多台时指定设备
#   ./scripts/install-ios-profile.sh --check      # 只看当前装的还剩几天，不构建
#   ./scripts/install-ios-profile.sh --release    # 构建 release 而不是 profile
#
# 说明：profile 版保留 DevTools 接入能力（可以连 profiler），日常调试用它；
# release 版性能一致但没有这些通道。默认 profile 是因为这个脚本的使用场景
# 就是"续期"，而不是出包。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOBILE_DIR="$REPO_ROOT/mobile"
BUNDLE_ID="ai.smelt.smeltMobile"
BUILD_MODE="profile"
DEVICE_ID=""
CHECK_ONLY=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    -d|--device) DEVICE_ID="${2:-}"; shift 2 ;;
    --check) CHECK_ONLY=true; shift ;;
    --release) BUILD_MODE="release"; shift ;;
    --profile) BUILD_MODE="profile"; shift ;;
    -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "未知参数：$1（--help 看用法）" >&2; exit 2 ;;
  esac
done

# 报告 .app 里嵌的 profile 还有多久到期。这是整个脚本存在的理由，所以
# 装完一定要打出来——否则用户还是不知道下次什么时候会被打断。
report_expiry() {
  local app="$1"
  local mp="$app/embedded.mobileprovision"
  [[ -f "$mp" ]] || { echo "（找不到 embedded.mobileprovision，跳过有效期检查）"; return; }

  local plist expires
  plist=$(security cms -D -i "$mp" 2>/dev/null) || { echo "（profile 解析失败，跳过）"; return; }
  expires=$(printf '%s' "$plist" | plutil -extract ExpirationDate raw -o - - 2>/dev/null) || return

  local expires_epoch now_epoch days
  # profile 里是 ISO8601 UTC（2026-08-22T06:37:02Z）。
  expires_epoch=$(date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$expires" "+%s" 2>/dev/null) || return
  now_epoch=$(date "+%s")
  days=$(( (expires_epoch - now_epoch) / 86400 ))

  echo
  echo "签名有效期至：$(date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$expires" "+%Y-%m-%d %H:%M UTC" 2>/dev/null)"
  if (( days < 0 )); then
    echo "⚠️  已过期 $(( -days )) 天，app 现在起不来——重跑本脚本（不带 --check）续期。"
  elif (( days <= 2 )); then
    echo "⚠️  只剩 ${days} 天。免费账号的 profile 就是 7 天一轮，到期重跑本脚本。"
  else
    echo "还剩 ${days} 天。到期后重跑本脚本即可。"
  fi
}

APP_PATH="$MOBILE_DIR/build/ios/iphoneos/Runner.app"

if [[ "$CHECK_ONLY" == true ]]; then
  [[ -d "$APP_PATH" ]] || { echo "本地还没有构建产物：$APP_PATH" >&2; exit 1; }
  report_expiry "$APP_PATH"
  exit 0
fi

# 没显式指定就自己找：真机 iOS 设备（排掉模拟器/macOS/Chrome）。恰好一台才自动选，
# 多台时要求指定——装错设备比报错更烦人。
if [[ -z "$DEVICE_ID" ]]; then
  echo "正在查找已连接的 iPhone…"
  DEVICES=$(cd "$MOBILE_DIR" && flutter devices --machine 2>/dev/null | python3 -c '
import json, sys
try:
    devices = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for d in devices:
    if d.get("targetPlatform", "").startswith("ios") and not d.get("emulator", True):
        print(d.get("id", ""), d.get("name", ""), sep="\t")
') || true

  count=$(printf '%s' "$DEVICES" | grep -c . || true)
  if [[ "$count" -eq 0 ]]; then
    echo "没找到已连接的 iPhone。插上数据线并在手机上点「信任此电脑」后重试。" >&2
    exit 1
  fi
  if [[ "$count" -gt 1 ]]; then
    echo "连着多台 iOS 设备，请用 -d <UDID> 指定：" >&2
    printf '%s\n' "$DEVICES" | sed 's/^/  /' >&2
    exit 1
  fi
  DEVICE_ID=$(printf '%s' "$DEVICES" | cut -f1)
  DEVICE_NAME=$(printf '%s' "$DEVICES" | cut -f2)
  echo "→ $DEVICE_NAME ($DEVICE_ID)"
fi

cd "$MOBILE_DIR"

echo
echo "构建 ${BUILD_MODE} 版（首次或改过 Rust 代码时要几分钟）…"
flutter build ios "--${BUILD_MODE}"

echo
echo "安装到设备…"
# flutter install 会先卸载旧版再装。旧版签名已失效时这一步是必须的，
# 覆盖安装在签名变化时会被系统拒绝。
flutter install "--${BUILD_MODE}" -d "$DEVICE_ID"

# flutter install 偶尔会在装成功后仍然静默退出，实际有没有装上以设备为准。
echo
if xcrun devicectl device info apps --device "$DEVICE_ID" 2>/dev/null | grep -q "$BUNDLE_ID"; then
  echo "✓ 已安装到设备：$BUNDLE_ID"
else
  echo "⚠️  设备上没查到 $BUNDLE_ID，安装可能没成功。" >&2
  echo "    手机解锁着的话可以重跑一次；若提示不受信任的开发者，去" >&2
  echo "    设置 → 通用 → VPN与设备管理 里信任该开发者证书。" >&2
  exit 1
fi

report_expiry "$APP_PATH"
