#!/usr/bin/env bash
# 发版：按日期取号，在当前提交上打 tag。不改文件、不提交、不推送，最后打印推送命令。
# 版本号只在 tag 上：打包时由 scripts/version.sh 从 tag 算出来编进程序。取号规则见 docs/release.md
# 的「版本号」。
#
#   scripts/release.sh             正式版：今天有进行中的 rc 就去掉 -rc.N 发它，否则取今天的新号
#   scripts/release.sh --rc        预发布：今天有进行中的 rc 就 rc.N+1，否则今天的新号加 -rc.1
#
# 日期一律是打 tag 那天：前几天没发完的 rc 不再接着用旧日期，取今天的新号。
#   scripts/release.sh --dry-run   只算出版本号，什么都不改（可与 --rc 同用）
#
# 「当前版本」是当前提交往前最近的发版 tag，脚本靠它判断有没有进行中的 rc。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

die() { echo "✗ $*" >&2; exit 1; }

rc=0 dry_run=0
for arg in "$@"; do
  case "$arg" in
    --rc) rc=1 ;;
    --dry-run) dry_run=1 ;;
    *) die "未知参数：${arg}（可用 --rc、--dry-run）" ;;
  esac
done

# 比较 x.y.z 三段整数：$1 > $2 返回 0。
core_gt() {
  local a b i
  IFS=. read -r -a a <<<"$1"
  IFS=. read -r -a b <<<"$2"
  for i in 0 1 2; do
    ((a[i] > b[i])) && return 0
    ((a[i] < b[i])) && return 1
  done
  return 1
}

# tag 以 GitHub 上的为准：找指向 smelt-ai/smelt 的远端。
remote="$(git remote -v | awk '$2 ~ /github\.com[:\/]smelt-ai\/smelt(\.git)?$/ && $3 == "(push)" {print $1; exit}')"
[[ -n "$remote" ]] || die "没有指向 github.com/smelt-ai/smelt 的远端"

[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || die "只在 main 上发版"
git fetch --quiet --tags "$remote" main
git merge-base --is-ancestor "$remote/main" HEAD || die "本地 main 落后于 $remote/main，先 pull"

# 只认发版 tag（v 加数字开头），plugin-sdk-v0.1.0 这类不算。
current="$(git describe --tags --abbrev=0 --match 'v[0-9]*' HEAD 2>/dev/null)" \
  || die "找不到发版 tag（v*）"
current="${current#v}"

today="$(date +%Y).$((10#$(date +%m)))$(date +%d)"

# 今天的新号：当天已有的号（含 rc）里最大的 N 加一，没有就从 0 起。
next_today() {
  local n=-1 tag rest
  while read -r tag; do
    rest="${tag#"v$today."}"
    rest="${rest%%-*}"
    if [[ "$rest" =~ ^[0-9]+$ ]] && ((rest > n)); then n=$rest; fi
  done < <(git tag --list "v$today.*")
  echo "$today.$((n + 1))"
}

if [[ "$current" == *-rc.* && "${current%%-*}" == "$today".* ]]; then
  base="${current%%-*}"
  if ((rc)); then
    version="$base-rc.$((${current##*-rc.} + 1))"
  else
    version="$base"
  fi
else
  base="$(next_today)"
  core_gt "$base" "$current" || die "算出的 $base 不比当前的 $current 新，检查系统日期"
  version="$base"
  if ((rc)); then version="$base-rc.1"; fi
fi

git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "tag v$version 已存在"
echo "▶ $current → $version（$(git rev-parse --short HEAD)）"
if ((dry_run)); then exit 0; fi

git tag -a "v$version" -m "v$version"
echo "✓ 已打 tag v${version}。确认无误后推送："
if [[ "$(git rev-parse HEAD)" == "$(git rev-parse "$remote/main")" ]]; then
  echo "  git push $remote v$version"
else
  # 本地 main 领先：tag 指向的提交要和 main 一起上去。
  echo "  git push --atomic $remote main v$version"
fi
