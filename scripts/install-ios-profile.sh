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

# 自动签名要求 Xcode 里登录着 Apple ID，否则 flutter build 会跑满一轮再抛
# "No Accounts: Add a new account in Accounts settings."。Keychain 里有证书也不
# 够——免费账号的 profile 七天一过就被清掉，重新申请必须走账号。提前拦一下，
# 省掉那次白等的构建。
#
# 这个 key 属于 Xcode 内部实现，换版本可能改名；读不到就当检查不适用直接放行，
# 不能因为探测手段失效而挡住本来能跑的构建。
preflight_xcode_account() {
  local json
  json=$(defaults export com.apple.dt.Xcode - 2>/dev/null \
    | plutil -extract DVTDeveloperAccountManagerAppleIDLists json -o - - 2>/dev/null) || return 0
  [[ -n "$json" ]] || return 0

  # 结构是 {"IDE.Identifiers.Prod": ["someone@example.com", ...]}，登录过才有条目。
  # 解析不出来（换版本改了结构）就放行，不拿探测失败去挡构建。
  local state
  state=$(printf '%s' "$json" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("unknown"); raise SystemExit
if not isinstance(data, dict):
    print("unknown"); raise SystemExit
print("empty" if not any(data.values()) else "ok")
' 2>/dev/null) || return 0
  [[ "$state" == "empty" ]] || return 0

  echo "Xcode 里没有登录 Apple ID，自动签名无法申请 provisioning profile。" >&2
  echo >&2
  echo "  打开 Xcode → Settings → Accounts → 左下角 + → Apple ID → 登录，" >&2
  echo "  然后重跑本脚本。" >&2
  echo >&2
  echo "（免费账号的 profile 只有 7 天，过期后要靠登录着的账号重新申请；" >&2
  echo "  Keychain 里留着证书并不够。）" >&2
  exit 1
}

APP_PATH="$MOBILE_DIR/build/ios/iphoneos/Runner.app"

if [[ "$CHECK_ONLY" == true ]]; then
  [[ -d "$APP_PATH" ]] || { echo "本地还没有构建产物：$APP_PATH" >&2; exit 1; }
  report_expiry "$APP_PATH"
  exit 0
fi

preflight_xcode_account

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
  echo "⚠️  设备上没查到 ${BUNDLE_ID}，安装可能没成功。" >&2
  echo "    手机解锁着的话可以重跑一次；若提示不受信任的开发者，去" >&2
  echo "    设置 → 通用 → VPN与设备管理 里信任该开发者证书。" >&2
  exit 1
fi

report_expiry "$APP_PATH"
