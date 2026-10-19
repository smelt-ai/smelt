#!/usr/bin/env bash
# 在一台 Linux 机器上装 Smelt 运行时（smeltd）。随 tar.gz 一起分发，解包后在包目录里
# 执行。这是唯一需要用户自己 SSH 上去做的一步（设计文档 D7）。
#
#   ./install.sh             装好、启动，打印配对串
#   ./install.sh --no-pair   只装好、启动
#
# 做的事：
#   1. smelt-notify、smelt-agent-mcp 装进 ~/.smelt/bin（SMELT_HOME 可改），先写临时
#      文件再 rename，不原地覆盖正在被执行的文件。
#   2. smeltd install：把 smeltd 发布成不可变映像、拨启动名（已经在跑就交接到新版本，
#      会话保留），写 systemd 用户单元并启动；没开 linger 时提示。
#   3. smeltd pair：开启远程访问，打印一次性配对串和二维码。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SMELT_HOME="${SMELT_HOME:-$HOME/.smelt}"
BIN="$SMELT_HOME/bin"
PROGRAMS=(smeltd smelt-notify smelt-agent-mcp)

PAIR=1
case "${1:-}" in
  "") ;;
  --no-pair) PAIR=0 ;;
  *) echo "用法：$0 [--no-pair]" >&2; exit 2 ;;
esac

[[ "$(uname -s)" == "Linux" ]] || { echo "这个包只能装在 Linux 上" >&2; exit 1; }
command -v systemctl >/dev/null 2>&1 || {
  echo "需要 systemd：运行时以 systemd 用户服务常驻" >&2
  exit 1
}
for program in "${PROGRAMS[@]}"; do
  [[ -x "$HERE/$program" ]] || { echo "包里缺少 $program" >&2; exit 1; }
done

mkdir -p "$BIN"
for program in smelt-notify smelt-agent-mcp; do
  install -m 0755 "$HERE/$program" "$BIN/.$program.new"
  mv -f "$BIN/.$program.new" "$BIN/$program"
done

"$HERE/smeltd" install
echo "已装到 $BIN"

if [[ "$PAIR" == 1 ]]; then
  "$BIN/smeltd" pair
fi
