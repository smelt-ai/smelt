#!/usr/bin/env bash
# 把 profile 版 Smelt Mobile 重新构建并装到 iPhone 上。
#
# 免费 Apple ID 签名的 provisioning profile 只有 7 天有效期；Xcode 默认会复用
# 尚未过期的 profile，所以单纯重建并不会延长有效期。本脚本每次安装前清除该 App
# 的 Xcode 自动签名缓存，让 Xcode 重新申请 profile，并在未确认续期时拒绝安装。
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

profile_expiration() {
  local plist
  plist=$(security cms -D -i "$1" 2>/dev/null) || return 1
  printf '%s' "$plist" | plutil -extract ExpirationDate raw -o - - 2>/dev/null
}

profile_expiration_epoch() {
  date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$1" "+%s" 2>/dev/null
}

# Xcode 会复用还有效的自动签名 profile。只删除这个 App 的 Xcode 管理缓存，
# 不碰手动 profile 或其他 App；清缓存不会撤销已安装 App 正在使用的签名。
clear_cached_xcode_profiles() {
  local profile_dir profile plist managed team_id app_identifier
  local removed=0
  local -a profile_dirs=(
    "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
    "$HOME/Library/MobileDevice/Provisioning Profiles"
  )

  for profile_dir in "${profile_dirs[@]}"; do
    [[ -d "$profile_dir" ]] || continue
    while IFS= read -r -d '' profile; do
      plist=$(security cms -D -i "$profile" 2>/dev/null) || continue
      managed=$(printf '%s' "$plist" | plutil -extract IsXcodeManaged raw -o - - 2>/dev/null) || continue
      [[ "$managed" == "true" ]] || continue
      team_id=$(printf '%s' "$plist" | plutil -extract TeamIdentifier.0 raw -o - - 2>/dev/null) || continue
      app_identifier=$(printf '%s' "$plist" | plutil -extract Entitlements.application-identifier raw -o - - 2>/dev/null) || continue

      case "$app_identifier" in
        "$team_id.$BUNDLE_ID"|"$team_id.$BUNDLE_ID".*)
          rm -f "$profile"
          removed=$((removed + 1))
          ;;
      esac
    done < <(find "$profile_dir" -maxdepth 1 -type f \
      \( -name '*.mobileprovision' -o -name '*.provisionprofile' \) -print0)
  done

  if (( removed > 0 )); then
    echo "已清除 ${removed} 个 $BUNDLE_ID 的 Xcode 自动签名缓存，构建时将重新申请 profile。"
  else
    echo "未找到 $BUNDLE_ID 的 Xcode 自动签名缓存，继续由 Xcode 自动签名。"
  fi
}

verify_profile_refresh() {
  local previous_expiry="$1"
  local mp="$APP_PATH/embedded.mobileprovision"
  [[ -f "$mp" ]] || {
    echo "构建产物缺少 embedded.mobileprovision，取消安装。" >&2
    exit 1
  }

  local expires expires_epoch now_epoch previous_epoch
  expires=$(profile_expiration "$mp") || {
    echo "无法解析构建产物中的 provisioning profile，取消安装。" >&2
    exit 1
  }
  expires_epoch=$(profile_expiration_epoch "$expires") || {
    echo "无法读取新 profile 的到期时间，取消安装。" >&2
    exit 1
  }
  now_epoch=$(date "+%s")

  if [[ -n "$previous_expiry" ]]; then
    previous_epoch=$(profile_expiration_epoch "$previous_expiry") || {
      echo "无法读取旧 profile 的到期时间，取消安装。" >&2
      exit 1
    }
    if (( expires_epoch <= previous_epoch )); then
      echo "⚠️  profile 有效期未刷新（旧：${previous_expiry}；新：${expires}），取消安装。" >&2
      echo "    请确认 Xcode 已登录 Apple ID、网络可用后重试。" >&2
      exit 1
    fi
  fi

  # 免费 Apple ID 的 profile 有效期为 7 天；允许构建耗时带来最多 1 天误差。
  if (( expires_epoch - now_epoch < 6 * 86400 )); then
    echo "⚠️  新 profile 剩余有效期不足 6 天（到期：${expires}），未能确认续成新的 7 天，取消安装。" >&2
    exit 1
  fi
  echo "✓ profile 有效期已刷新至：$expires"
}

# 报告 .app 里嵌的 profile 还有多久到期。
report_expiry() {
  local app="$1"
  local mp="$app/embedded.mobileprovision"
  [[ -f "$mp" ]] || { echo "（找不到 embedded.mobileprovision，跳过有效期检查）"; return; }

  local expires
  expires=$(profile_expiration "$mp") || { echo "（profile 解析失败，跳过）"; return; }

  local expires_epoch now_epoch remaining days expired_days
  # profile 里是 ISO8601 UTC（2026-08-22T06:37:02Z）。
  expires_epoch=$(profile_expiration_epoch "$expires") || return
  now_epoch=$(date "+%s")
  remaining=$((expires_epoch - now_epoch))
  days=$(( (remaining + 86399) / 86400 ))

  echo
  echo "签名有效期至：$(date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$expires" "+%Y-%m-%d %H:%M UTC" 2>/dev/null)"
  if (( remaining < 0 )); then
    expired_days=$(( (-remaining + 86399) / 86400 ))
    echo "⚠️  已过期 ${expired_days} 天，app 现在起不来——重跑本脚本（不带 --check）续期。"
  elif (( days <= 2 )); then
    echo "⚠️  只剩 ${days} 天。免费账号的 profile 有效期为 7 天，重跑本脚本即可续期。"
  else
    echo "还剩 ${days} 天。重跑本脚本即可重新续期。"
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

# 记录上次构建的到期时间，构建后验证有效期确实延长，避免静默装回旧 profile。
PREVIOUS_PROFILE_EXPIRY=""
if [[ -f "$APP_PATH/embedded.mobileprovision" ]]; then
  PREVIOUS_PROFILE_EXPIRY=$(profile_expiration "$APP_PATH/embedded.mobileprovision" 2>/dev/null || true)
fi
clear_cached_xcode_profiles

echo
echo "构建 ${BUILD_MODE} 版（首次或改过 Rust 代码时要几分钟）…"
flutter build ios "--${BUILD_MODE}"
verify_profile_refresh "$PREVIOUS_PROFILE_EXPIRY"

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
