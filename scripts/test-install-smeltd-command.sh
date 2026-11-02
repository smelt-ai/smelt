#!/usr/bin/env bash
# 验证 install-smeltd.sh 生成的 /usr/local/bin/smeltd：root 先切用户再执行、参数原样传、
# 其他用户拿到明确提示。用假的 id、runuser，不需要 root。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# 只取生成函数，不执行安装脚本的主体。
ROOT_COMMAND_MARKER="由 Smelt 的 install.sh 生成"
eval "$(sed -n '/^root_command_script() {$/,/^}$/p' "$ROOT/scripts/install-smeltd.sh")"

HOME_DIR="$WORK/home dir"
mkdir -p "$HOME_DIR/.smelt/bin" "$WORK/fake"
cat >"$HOME_DIR/.smelt/bin/smeltd" <<'SCRIPT'
#!/bin/bash
echo "real-smeltd user=${USER:-} home=${HOME:-} xdg=${XDG_RUNTIME_DIR:-} cwd=$PWD args=$#"
printf 'arg=[%s]\n' "$@"
SCRIPT
cat >"$WORK/fake/id" <<'SCRIPT'
#!/bin/bash
echo "$FAKE_UID"
SCRIPT
cat >"$WORK/fake/runuser" <<'SCRIPT'
#!/bin/bash
[[ "$1" == -u && "$3" == -- ]] || { echo "runuser 用法不对：$*" >&2; exit 9; }
echo "switched-to=$2"
shift 3
exec "$@"
SCRIPT
chmod +x "$HOME_DIR/.smelt/bin/smeltd" "$WORK/fake/id" "$WORK/fake/runuser"

COMMAND="$WORK/smeltd"
root_command_script smelt 1001 "$HOME_DIR" /run/user/1001 >"$COMMAND"
chmod +x "$COMMAND"
export PATH="$WORK/fake:$PATH"

expect() {
  local output="$1" wanted="$2" what="$3"
  [[ "$output" == *"$wanted"* ]] || { echo "FAIL: $what：没有「$wanted」，实际：$output" >&2; exit 1; }
}

output="$(cd "$WORK" && FAKE_UID=0 "$COMMAND" relay 'a b')"
expect "$output" "switched-to=smelt" "root 要先切用户"
expect "$output" "user=smelt home=$HOME_DIR xdg=/run/user/1001" "切用户后的环境"
expect "$output" "cwd=$HOME_DIR" "root 调用要换到用户目录"
expect "$output" "arg=[a b]" "带空格的参数原样传"

output="$(FAKE_UID=1001 "$COMMAND" status)"
[[ "$output" != *switched-to* ]] || { echo "FAIL: 本来就是这个用户时不该切" >&2; exit 1; }
expect "$output" "arg=[status]" "同一用户直接执行"

if output="$(FAKE_UID=1002 "$COMMAND" pair 2>&1)"; then
  echo "FAIL: 其他用户应该失败" >&2
  exit 1
fi
expect "$output" "请用 sudo smeltd pair" "其他用户的提示"

mv "$HOME_DIR/.smelt/bin/smeltd" "$WORK/gone"
if output="$(FAKE_UID=0 "$COMMAND" status 2>&1)"; then
  echo "FAIL: 程序不在时应该失败" >&2
  exit 1
fi
expect "$output" "可能已经卸载" "卸载后的提示"

expect "$(cat "$COMMAND")" "$ROOT_COMMAND_MARKER" "带生成标记，重装才认得出"
echo "ok"
