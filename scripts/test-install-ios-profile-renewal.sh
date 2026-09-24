#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/smelt-ios-profile-renewal.XXXXXX")"
trap 'rm -rf "$FIXTURE"' EXIT

BUNDLE_ID="ai.smelt.smeltMobile"
TEAM_ID="H3H5ULV8KR"
OLD_EXPIRY="2026-09-25T08:30:34Z"
NEW_EXPIRY="2026-10-02T08:30:34Z"
export OLD_EXPIRY NEW_EXPIRY

mkdir -p \
  "$FIXTURE/scripts" \
  "$FIXTURE/mobile/build/ios/iphoneos/Runner.app" \
  "$FIXTURE/home/Library/Developer/Xcode/UserData/Provisioning Profiles" \
  "$FIXTURE/home/Library/MobileDevice/Provisioning Profiles" \
  "$FIXTURE/bin"
cp "$ROOT/scripts/install-ios-profile.sh" "$FIXTURE/scripts/install-ios-profile.sh"

PROFILE_DIR="$FIXTURE/home/Library/Developer/Xcode/UserData/Provisioning Profiles"
LEGACY_PROFILE_DIR="$FIXTURE/home/Library/MobileDevice/Provisioning Profiles"
TARGET_PROFILE="$PROFILE_DIR/target.mobileprovision"
LEGACY_TARGET_PROFILE="$LEGACY_PROFILE_DIR/target.mobileprovision"
UNRELATED_PROFILE="$PROFILE_DIR/unrelated.mobileprovision"
MANUAL_PROFILE="$LEGACY_PROFILE_DIR/manual.mobileprovision"
OLD_PROFILE="$FIXTURE/old.mobileprovision"
NEW_PROFILE="$FIXTURE/new.mobileprovision"
APP_PATH="$FIXTURE/mobile/build/ios/iphoneos/Runner.app"
CALL_LOG="$FIXTURE/calls.log"
export PROFILE_DIR LEGACY_PROFILE_DIR TARGET_PROFILE LEGACY_TARGET_PROFILE
export UNRELATED_PROFILE MANUAL_PROFILE OLD_PROFILE NEW_PROFILE APP_PATH CALL_LOG

write_profile() {
  local path="$1"
  local managed="$2"
  local bundle_id="$3"
  local expiry="$4"
  local team_id="${5:-$TEAM_ID}"
  python3 - "$path" "$managed" "$bundle_id" "$expiry" "$team_id" <<'PY'
import datetime
import plistlib
import sys

path, managed, bundle_id, expiry, team_id = sys.argv[1:]
expiration = datetime.datetime.strptime(expiry, "%Y-%m-%dT%H:%M:%SZ").replace(
    tzinfo=datetime.timezone.utc
)
profile = {
    "TeamIdentifier": [team_id],
    "ApplicationIdentifierPrefix": [team_id],
    "Entitlements": {"application-identifier": f"{team_id}.{bundle_id}"},
    "IsXcodeManaged": managed == "true",
    "ExpirationDate": expiration,
}
with open(path, "wb") as output:
    plistlib.dump(profile, output, fmt=plistlib.FMT_XML)
PY
}

reset_profiles() {
  write_profile "$TARGET_PROFILE" true "$BUNDLE_ID" "$OLD_EXPIRY"
  write_profile "$LEGACY_TARGET_PROFILE" true "$BUNDLE_ID" "$OLD_EXPIRY"
  write_profile "$UNRELATED_PROFILE" true "ai.smelt.other" "$OLD_EXPIRY"
  write_profile "$MANUAL_PROFILE" false "$BUNDLE_ID" "$OLD_EXPIRY"
  write_profile "$OLD_PROFILE" true "$BUNDLE_ID" "$OLD_EXPIRY"
  write_profile "$NEW_PROFILE" true "$BUNDLE_ID" "$NEW_EXPIRY"
  cp "$OLD_PROFILE" "$APP_PATH/embedded.mobileprovision"
}

cat >"$FIXTURE/bin/defaults" <<'EOF'
#!/usr/bin/env bash
printf 'fixture\n'
EOF

cat >"$FIXTURE/bin/security" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == cms && "$2" == -D && "$3" == -i ]]
cat "$4"
EOF

cat >"$FIXTURE/bin/plutil" <<'PY'
#!/usr/bin/env python3
import datetime
import json
import plistlib
import sys

args = sys.argv[1:]
key = args[args.index("-extract") + 1]
if key == "DVTDeveloperAccountManagerAppleIDLists":
    print(json.dumps({"IDE.Identifiers.Prod": ["dev@example.com"]}))
    raise SystemExit(0)

value = plistlib.loads(sys.stdin.buffer.read())
for part in key.split("."):
    value = value[int(part)] if isinstance(value, list) else value[part]
if isinstance(value, datetime.datetime):
    value = value.strftime("%Y-%m-%dT%H:%M:%SZ")
elif isinstance(value, bool):
    value = str(value).lower()
print(value)
PY

cat >"$FIXTURE/bin/date" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
last_arg=""
for arg in "$@"; do last_arg="$arg"; done
if [[ "${1:-}" == "-j" ]]; then
  if [[ "$last_arg" == "+%s" ]]; then
    case "${5:-}" in
      "$OLD_EXPIRY") echo 432000 ;;
      "$NEW_EXPIRY") echo 604801 ;;
      *) echo "unrecognized fixture date: ${5:-}" >&2; exit 1 ;;
    esac
  else
    echo "${5:-}"
  fi
elif [[ "${1:-}" == "+%s" ]]; then
  echo 0
else
  echo "unsupported fixture date arguments: $*" >&2
  exit 1
fi
EOF

cat >"$FIXTURE/bin/flutter" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}:${2:-}" in
  build:ios)
    if [[ -e "$TARGET_PROFILE" || -e "$LEGACY_TARGET_PROFILE" ]]; then
      echo "matching Xcode-managed profile was not removed before build" >&2
      exit 50
    fi
    [[ -f "$UNRELATED_PROFILE" ]] || { echo "unrelated profile was removed" >&2; exit 51; }
    [[ -f "$MANUAL_PROFILE" ]] || { echo "manual profile was removed" >&2; exit 52; }
    if [[ "${REUSE_OLD_PROFILE:-false}" == true ]]; then
      cp "$OLD_PROFILE" "$APP_PATH/embedded.mobileprovision"
    else
      cp "$NEW_PROFILE" "$APP_PATH/embedded.mobileprovision"
      cp "$NEW_PROFILE" "$PROFILE_DIR/renewed.mobileprovision"
    fi
    ;;
  install:*)
    printf 'install\n' >>"$CALL_LOG"
    ;;
  *)
    echo "unexpected flutter invocation: $*" >&2
    exit 53
    ;;
esac
EOF

cat >"$FIXTURE/bin/xcrun" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'ai.smelt.smeltMobile'
EOF
chmod +x "$FIXTURE/bin/"*

run_script() {
  set +e
  OUTPUT="$(HOME="$FIXTURE/home" PATH="$FIXTURE/bin:$PATH" \
    "$FIXTURE/scripts/install-ios-profile.sh" "$@" 2>&1)"
  STATUS=$?
  set -e
}

reset_profiles
run_script --check
if [[ "$STATUS" -ne 0 || ! -f "$TARGET_PROFILE" || ! -f "$LEGACY_TARGET_PROFILE" ]]; then
  echo "✗ --check 不应清理 provisioning profile 缓存" >&2
  echo "$OUTPUT" >&2
  exit 1
fi

run_script -d fixture-device
if [[ "$STATUS" -ne 0 ]]; then
  echo "✗ 安装构建应清理旧的 Xcode profile 并生成新有效期" >&2
  echo "$OUTPUT" >&2
  exit 1
fi
if [[ -e "$TARGET_PROFILE" || -e "$LEGACY_TARGET_PROFILE" ]]; then
  echo "✗ 旧的自动签名 profile 缓存没有全部清理" >&2
  exit 1
fi
if [[ ! -f "$UNRELATED_PROFILE" || ! -f "$MANUAL_PROFILE" ]]; then
  echo "✗ 刷新不应删除其他 App 或手动管理的 profile" >&2
  exit 1
fi
if [[ "$OUTPUT" != *"2026-10-02T08:30:34Z"* ]]; then
  echo "✗ 输出未显示新 profile 的有效期" >&2
  echo "$OUTPUT" >&2
  exit 1
fi
if [[ "$(grep -c '^install$' "$CALL_LOG")" -ne 1 ]]; then
  echo "✗ 新 profile 验证通过后应恰好安装一次" >&2
  exit 1
fi

# 即使 Xcode 构建成功，如果产物仍嵌着旧 profile，也必须阻止安装。
reset_profiles
: >"$CALL_LOG"
export REUSE_OLD_PROFILE=true
run_script -d fixture-device
unset REUSE_OLD_PROFILE
if [[ "$STATUS" -eq 0 || "$OUTPUT" != *"profile 有效期未刷新"* ]]; then
  echo "✗ 未续期的构建产物必须在安装前失败" >&2
  echo "$OUTPUT" >&2
  exit 1
fi
if [[ -s "$CALL_LOG" ]]; then
  echo "✗ profile 未刷新时不应安装" >&2
  exit 1
fi

echo "✓ iOS 安装脚本会清理并验证 Xcode 自动签名 profile 刷新"
