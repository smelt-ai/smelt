#!/usr/bin/env bash
# 手型光标只留给真正的链接（`smelt_ui::Link` 在组件内设置）；
# 本仓库其它地方出现 `.cursor_pointer()` 就是把按钮、行、页签画成了网页。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

matches="$(
  { grep -R -n --include='*.rs' -E '\.cursor_pointer\(\)' "$root/crates" || true; } \
    | grep -v -F "$root/crates/smelt-ui/src/link.rs:" || true
)"
if [ -n "$matches" ]; then
  echo "✗ 发现 .cursor_pointer()。按钮、行、页签、菜单用默认箭头；手型只给链接。"
  echo "$matches"
  exit 1
fi

echo "✓ 没有手型光标"
