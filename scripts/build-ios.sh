#!/usr/bin/env bash
# 出 iOS 发布包（App Store Connect / TestFlight 用），版本号跟发版统一。规则见 docs/release.md
# 的「版本号」和「移动端」。
#
#   scripts/build-ios.sh            在发版 tag 上出 mobile/build/ios/ipa/*.ipa，再用 Transporter 或 Xcode 上传
#   scripts/build-ios.sh --upload   出包后直接传到 App Store Connect（发版流水线用这个）
#
# 版本号取发版 tag（scripts/version.sh），去掉 -rc.N：Apple 只认纯数字的 x.y.z，同一次发版的 rc 和
# 正式版共用一个版本号，在 TestFlight 里靠构建号区分。构建号默认取 UTC 时间（两位年、一年中的
# 第几天、时、分），只增不减，又在 32 位整数以内；要指定就设 SMELT_IOS_BUILD_NUMBER。
#
# 签名是自动签名：本地用 Xcode 里登录的账号；没登录账号的机器（CI）设 SMELT_ASC_KEY_PATH、
# SMELT_ASC_KEY_ID、SMELT_ASC_ISSUER_ID，用 App Store Connect API 密钥代替账号。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

die() { echo "✗ $*" >&2; exit 1; }

upload=false
case "${1:-}" in
  "") ;;
  --upload) upload=true ;;
  *) die "不认识的参数：$1（只有 --upload）" ;;
esac

version="$(scripts/version.sh)"

# 包必须正好出自这次发版的代码：当前提交就是 tag，代码也对得上。
[[ "${version}" != *+* ]] \
  || die "当前提交不在发版 tag 上（${version}）：先用 scripts/release.sh 发版，再 git checkout 那个 tag 出包"
tag="v${version}"
git rev-parse -q --verify "refs/tags/${tag}" >/dev/null \
  || die "没有 tag ${tag}：先用 scripts/release.sh 发版，再出 iOS 包"
[[ "$(git rev-parse HEAD)" == "$(git rev-parse "${tag}^{commit}")" ]] \
  || die "当前提交不是 ${tag}：先 git checkout ${tag} 再出包"
git diff --quiet HEAD -- mobile crates || die "mobile/ 或 crates/ 有未提交的改动，出的包会对不上 ${tag}"

build_name="${version%%-*}"
build_number="${SMELT_IOS_BUILD_NUMBER:-$(date -u +%y%j%H%M)}"
[[ "${build_number}" =~ ^[0-9]+$ ]] || die "构建号要是整数：${build_number}"

# 不用 flutter build ipa：它没法把 API 密钥传给 xcodebuild，没登录账号的机器上签不了名。
# --config-only 只写好版本号等工程配置、装好 Pods，归档和导出交给 xcodebuild。
signing=(-allowProvisioningUpdates)
if [[ -n "${SMELT_ASC_KEY_PATH:-}" ]]; then
  [[ -n "${SMELT_ASC_KEY_ID:-}" && -n "${SMELT_ASC_ISSUER_ID:-}" ]] \
    || die "设了 SMELT_ASC_KEY_PATH 就还要设 SMELT_ASC_KEY_ID 和 SMELT_ASC_ISSUER_ID"
  signing+=(
    -authenticationKeyPath "${SMELT_ASC_KEY_PATH}"
    -authenticationKeyID "${SMELT_ASC_KEY_ID}"
    -authenticationKeyIssuerID "${SMELT_ASC_ISSUER_ID}"
  )
fi

archive="mobile/build/ios/archive/Runner.xcarchive"
export_dir="mobile/build/ios/ipa"
export_options="mobile/ios/ExportOptions.plist"
if ${upload}; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT
  export_options="${tmp}/ExportOptions.plist"
  cp mobile/ios/ExportOptions.plist "${export_options}"
  plutil -replace destination -string upload "${export_options}"
fi

echo "▶ iOS ${build_name} (${build_number})，出自 ${tag}"
(cd mobile && flutter build ios --release --config-only \
  --build-name "${build_name}" --build-number "${build_number}")
rm -rf "${archive}" "${export_dir}"
xcodebuild archive -quiet \
  -workspace mobile/ios/Runner.xcworkspace -scheme Runner -configuration Release \
  -destination generic/platform=iOS -archivePath "${archive}" "${signing[@]}"
xcodebuild -exportArchive -quiet \
  -archivePath "${archive}" -exportPath "${export_dir}" \
  -exportOptionsPlist "${export_options}" "${signing[@]}"

if ${upload}; then
  echo "✓ 已传到 App Store Connect，处理完会出现在 TestFlight"
else
  echo "✓ 产物：$(ls "${export_dir}"/*.ipa)"
  echo "  用 Transporter 或 Xcode 传到 App Store Connect，或者加 --upload 重新出包直接上传"
fi
