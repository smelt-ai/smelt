#!/usr/bin/env bash
# 不安装 App。只验证本地安装回收：没人用的事务目录删掉，仍占用、无法证明、
# 符号链接、在线更新目录和安装锁都留下。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck disable=SC1091
source "$ROOT/scripts/install-mac-app-atomically.sh"

fixture="$(mktemp -d "${TMPDIR:-/tmp}/smelt-install-gc.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

parent="$fixture/Applications"
mkdir -p "$parent"

unused="$parent/.Smelt.smelt-local.AAAAAA"
in_use="$parent/.Smelt.smelt-local.BBBBBB"
unknown="$parent/.Smelt.smelt-local.CCCCCC"
mkdir -p "$unused/Smelt.app" "$in_use/Smelt.app" "$unknown/Smelt.app"

update="$parent/.Smelt.smelt-update-job.app"
mkdir -p "$update"
lock="$parent/.Smelt.smelt-local.lock"
printf 'lock\n' >"$lock"

linked_target="$fixture/elsewhere"
mkdir -p "$linked_target"
ln -s "$linked_target" "$parent/.Smelt.smelt-local.DDDDDD"

lsof="$fixture/lsof"
cat >"$lsof" <<'EOF'
#!/usr/bin/env bash
dir=""
for arg in "$@"; do
  dir="$arg"
done
case "$dir" in
  *BBBBBB*)
    printf 'p1\n'
    exit 0
    ;;
  *CCCCCC*)
    echo "warn" >&2
    exit 1
    ;;
  *)
    exit 1
    ;;
esac
EOF
chmod +x "$lsof"

SMELT_LSOF="$lsof" gc_retired_local_apps "$parent" Smelt

[[ ! -e "$unused" ]] || { echo "✗ 没人用的旧 App 还在：$unused" >&2; exit 1; }
[[ -d "$in_use" ]] || { echo "✗ 仍被使用的旧 App 被删了" >&2; exit 1; }
[[ -d "$unknown" ]] || { echo "✗ 无法证明空闲的旧 App 被删了" >&2; exit 1; }
[[ -d "$update" ]] || { echo "✗ 在线更新目录被误删了" >&2; exit 1; }
[[ -f "$lock" ]] || { echo "✗ 安装锁被误删了" >&2; exit 1; }
[[ -L "$parent/.Smelt.smelt-local.DDDDDD" ]] || { echo "✗ 符号链接事务目录被误删了" >&2; exit 1; }

echo "✓ 本地安装只回收 lsof 证明没人用的旧 App"
