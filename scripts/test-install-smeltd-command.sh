#!/usr/bin/env bash
# 验证 install-smeltd.sh 生成的 /usr/local/bin/smeltd、/usr/local/bin/smelt：root 先切用户再执行、参数原样传、
# 其他用户拿到明确提示。用假的 id、runuser，不需要 root。再验证以 root 执行时选哪个运行用户，
# 以及放软链接时换掉以前生成的切用户脚本、不动别人的文件。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# 取真实路径：macOS 的 /var 是指向 /private/var 的链接，cd 之后 $PWD 对不上。
WORK="$(cd "$(mktemp -d)" && pwd -P)"
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
root_command_script smeltd smelt 1001 "$HOME_DIR" /run/user/1001 >"$COMMAND"
chmod +x "$COMMAND"
export PATH="$WORK/fake:$PATH"

expect() {
  local output="$1" wanted="$2" what="$3"
  [[ "$output" == *"$wanted"* ]] || { echo "FAIL: ${what}：没有「${wanted}」，实际：$output" >&2; exit 1; }
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

# 命令行 smelt 用同一个生成函数，执行的是 ~/.smelt/bin/smelt。
cat >"$HOME_DIR/.smelt/bin/smelt" <<'SCRIPT'
#!/bin/bash
echo "real-smelt user=${USER:-} args=$*"
SCRIPT
chmod +x "$HOME_DIR/.smelt/bin/smelt"
CLI="$WORK/smelt"
root_command_script smelt smelt 1001 "$HOME_DIR" "" >"$CLI"
chmod +x "$CLI"
output="$(FAKE_UID=0 "$CLI" agent list)"
expect "$output" "real-smelt user=smelt args=agent list" "root 跑 smelt 切到运行用户"
if output="$(FAKE_UID=1002 "$CLI" agent list 2>&1)"; then
  echo "FAIL: 其他用户跑 smelt 应该失败" >&2
  exit 1
fi
expect "$output" "请用 sudo smelt agent list" "smelt 给其他用户的提示"

# ── 运行用户 ──
eval "$(sed -n '/^runtime_user() {$/,/^}$/p' "$ROOT/scripts/install-smeltd.sh")"
SMELT_HOME_OF_SMELT="$WORK/smelt-home"
cat >"$WORK/fake/getent" <<SCRIPT
#!/bin/bash
echo "smelt:x:1001:1001::$SMELT_HOME_OF_SMELT:/bin/bash"
SCRIPT
# 假 id：FAKE_SMELT_EXISTS=1 时认得 smelt 这个用户。
cat >"$WORK/fake/id" <<'SCRIPT'
#!/bin/bash
if [[ "${1:-}" == -u && -n "${2:-}" ]]; then
  [[ "$2" == smelt && "${FAKE_SMELT_EXISTS:-0}" == 1 ]] && { echo 1001; exit 0; }
  exit 1
fi
echo "$FAKE_UID"
SCRIPT
chmod +x "$WORK/fake/getent" "$WORK/fake/id"

unset SMELT_USER
[[ "$(runtime_user)" == root ]] || { echo "FAIL: 默认 root" >&2; exit 1; }
[[ "$(SMELT_USER=dev runtime_user)" == dev ]] || { echo "FAIL: SMELT_USER 指定普通用户" >&2; exit 1; }
mkdir -p "$SMELT_HOME_OF_SMELT/.smelt/bin"
touch "$SMELT_HOME_OF_SMELT/.smelt/bin/smeltd"
[[ "$(FAKE_SMELT_EXISTS=1 runtime_user)" == smelt ]] \
  || { echo "FAIL: 已经装在 smelt 名下的要接着用" >&2; exit 1; }
[[ "$(FAKE_SMELT_EXISTS=0 runtime_user)" == root ]] \
  || { echo "FAIL: 没有 smelt 用户时不看它的目录" >&2; exit 1; }
[[ "$(FAKE_SMELT_EXISTS=1 SMELT_USER=root runtime_user)" == root ]] \
  || { echo "FAIL: SMELT_USER 优先" >&2; exit 1; }

# ── 放软链接 ──
eval "$(sed -n '/^link_commands() {$/,/^}$/p' "$ROOT/scripts/install-smeltd.sh")"
say() { :; }
warn() { echo "$*" >&2; }
COMMANDS=(smeltd smelt)
HOME="$WORK/root-home"
SMELT_HOME="$HOME/.smelt"
BIN="$SMELT_HOME/bin"
LINKS="$WORK/usr-local-bin"
mkdir -p "$BIN" "$LINKS"
cp "$COMMAND" "$LINKS/smeltd"
echo "别人的 smelt" >"$LINKS/smelt"
output="$(link_commands "$LINKS" 2>&1)"
[[ "$(readlink "$LINKS/smeltd")" == "$BIN/smeltd" ]] \
  || { echo "FAIL: 以前生成的切用户脚本要换成软链接" >&2; exit 1; }
[[ ! -L "$LINKS/smelt" && "$(cat "$LINKS/smelt")" == "别人的 smelt" ]] \
  || { echo "FAIL: 别人的文件不能动" >&2; exit 1; }
expect "$output" "$LINKS/smelt 已存在且不是安装脚本放的" "不覆盖时要说"
echo "ok"
