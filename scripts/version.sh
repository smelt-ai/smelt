#!/usr/bin/env bash
# 算出这次构建的版本号，打印到标准输出。打包脚本拿它设 SMELT_BUILD_VERSION 编进程序
# （crates/smelt-version）。规则见 docs/release.md 的「版本号」：
#   提交正好是发版 tag v<版本>：就是 <版本>，如 2026.1027.0-rc.1
#   tag 之后的提交：最近的发版 tag 加构建信息，如 2026.1027.0-rc.1+g32c3172（32c3172 是那个提交）
#
#   scripts/version.sh            按当前提交算
#   scripts/version.sh <提交>      按指定提交算（下游仓库按它合进来的那个提交算，见 docs/release.md）
#
# 已设 SMELT_BUILD_VERSION 时只校验、原样打印（发版流水线按 tag 名传入，不依赖 git 历史）。
# 算不出来（不在 git 仓库里、浅克隆、没有 tag）直接失败，不退回占位版本：打出 0.0.0 的包比打不出包更糟。
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

die() { echo "✗ $*" >&2; exit 1; }

# semver 2.0：x.y.z，可带 -预发布 和 +构建信息，不带 v 前缀。数字段不能有前导零，标识符不能为空。
num='(0|[1-9][0-9]*)'
pre_id="($num|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
build_id='[0-9A-Za-z-]+'
semver_re="^$num\\.$num\\.$num(-$pre_id(\\.$pre_id)*)?(\\+$build_id(\\.$build_id)*)?\$"
valid() { [[ "$1" =~ $semver_re ]]; }

if [[ -n "${SMELT_BUILD_VERSION:-}" ]]; then
  valid "$SMELT_BUILD_VERSION" || die "SMELT_BUILD_VERSION 不是合法的版本号：${SMELT_BUILD_VERSION}（不带 v 前缀）"
  echo "$SMELT_BUILD_VERSION"
  exit 0
fi

rev="${1:-HEAD}"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || die "不在 git 仓库里，算不出版本号：用 SMELT_BUILD_VERSION 指定"
# 不用 rev-parse --is-shallow-repository：git 2.15 才有，旧版会把参数原样打出来。
[[ ! -f "$(git rev-parse --git-dir)/shallow" ]] \
  || die "浅克隆算不出版本号：先 git fetch --unshallow --tags"
# 只认发版 tag（v 加数字开头），plugin-sdk-v0.1.0 这类不算。
described="$(git describe --tags --long --abbrev=7 --match 'v[0-9]*' "$rev" 2>/dev/null)" \
  || die "从 $rev 往前找不到发版 tag（v*）：先 git fetch --tags"

# --long 一律是 v<版本>-<距离>-g<哈希>。版本自己可能带 -rc.N，所以从右边拆。
hash="${described##*-g}"
rest="${described%-g*}"
distance="${rest##*-}"
version="${rest%-*}"
version="${version#v}"
if [[ "$distance" == 0 ]]; then
  out="$version"
else
  out="$version+g$hash"
fi
valid "$out" || die "从 ${described} 算出的版本号不合法：$out"
echo "$out"
