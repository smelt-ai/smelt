#!/usr/bin/env bash
# 出 iOS 发布包（传 App Store Connect / TestFlight 用），版本号跟发版统一。规则见 docs/release.md
# 的「版本号」和「移动端」。
#
#   scripts/build-ios.sh     在发版 tag 上出 mobile/build/ios/ipa/*.ipa，上传仍用 Transporter 或 Xcode
#
# 版本号取工作区版本（Cargo.toml），去掉 -rc.N：Apple 只认纯数字的 x.y.z，同一次发版的 rc 和
# 正式版共用一个版本号，在 TestFlight 里靠构建号区分。构建号默认取 UTC 时间（两位年、一年中的
# 第几天、时、分），只增不减，又在 32 位整数以内；要指定就设 SMELT_IOS_BUILD_NUMBER。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

die() { echo "✗ $*" >&2; exit 1; }

version="$(awk -F'"' '/^\[workspace.package\]/{inside=1; next} /^\[/{inside=0} inside && /^version *=/{print $2; exit}' Cargo.toml)"
[[ -n "${version}" ]] || die "读不到工作区版本号"

# 包必须正好出自这次发版的代码：版本号对得上 tag，代码也对得上。
tag="v${version}"
git rev-parse -q --verify "refs/tags/${tag}" >/dev/null \
  || die "工作区版本 ${version} 还没有 tag ${tag}：先用 scripts/release.sh 发版，再出 iOS 包"
[[ "$(git rev-parse HEAD)" == "$(git rev-parse "${tag}^{commit}")" ]] \
  || die "当前提交不是 ${tag}：先 git checkout ${tag} 再出包"
git diff --quiet HEAD -- mobile crates || die "mobile/ 或 crates/ 有未提交的改动，出的包会对不上 ${tag}"

build_name="${version%%-*}"
build_number="${SMELT_IOS_BUILD_NUMBER:-$(date -u +%y%j%H%M)}"
[[ "${build_number}" =~ ^[0-9]+$ ]] || die "构建号要是整数：${build_number}"

echo "▶ iOS ${build_name} (${build_number})，出自 ${tag}"
(cd mobile && flutter build ipa --release \
  --build-name "${build_name}" --build-number "${build_number}" \
  --export-options-plist ios/ExportOptions.plist)
echo "✓ 产物：$(ls mobile/build/ios/ipa/*.ipa)"
echo "  用 Transporter 或 Xcode 传到 App Store Connect"
