#!/usr/bin/env bash
# 解析出一个可用的 bun，打印其路径。
#
# 优先受管 bun（~/.smelt/runtime/bun-<版本>/bun）：那正是插件在用户机器上被
# exec 的那一个，版本由 crates/smelt-core/src/acp_conn.rs 的 BUN_VERSION 锁定。
# 拿 PATH 上那个随便什么版本去验插件，验的就不是同一件事。
#
# 找不到时退出码 1 且不打印路径，调用方据此决定跳过还是报错。
set -euo pipefail

for candidate in "${HOME}"/.smelt/runtime/bun-*/bun; do
  if [[ -x "$candidate" ]]; then
    echo "$candidate"
    exit 0
  fi
done

if command -v bun >/dev/null 2>&1; then
  command -v bun
  exit 0
fi

exit 1
