#!/usr/bin/env bash
# 把仓库里的自带 JS 插件（plugins/<名字>/，有 smelt-plugin.json 的才算）预编译成 QuickJS 字节码并
# 放进安装包目录（macOS 是 Smelt.app/Contents/Resources/plugins，Windows 是安装目录下的 plugins）：
# - 脚本插件产出 bundle.nshc + bundle.meta.json，并保留插件源码与 locales/、themes/（不带 tests/、
#   *.test.*、dotfiles、node_modules），让发版包冷启零打包加载，同时支持 `smelt plugin fork`；
# - 纯主题插件产出 smelt-plugin.json + locales/ + 主题 json；
# 发版的 App 从那里只读、零打包加载（crates/smelt/src/plugins/builtin.rs）。
#
# 用法：scripts/stage-plugins.sh <目标 plugins 目录>（整个替换）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:?用法：scripts/stage-plugins.sh <目标 plugins 目录>}"

SMELT_BIN="${SMELT_BIN:-}"
if [[ -z "$SMELT_BIN" ]]; then
  TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
  if [[ -x "$TARGET_DIR/release/smelt" ]]; then
    SMELT_BIN="$TARGET_DIR/release/smelt"
  elif [[ -f "$TARGET_DIR/release/smelt.exe" ]]; then
    SMELT_BIN="$TARGET_DIR/release/smelt.exe"
  fi
fi

if [[ -z "$SMELT_BIN" || ! -e "$SMELT_BIN" ]]; then
  echo "✗ 缺少 release 版 smelt 可执行文件（用于预编译内置插件字节码），先跑 make build 或 cargo build --release --bin smelt" >&2
  exit 1
fi

"$SMELT_BIN" plugin stage "$ROOT/plugins" "$DEST" >/dev/null
for dir in "$DEST"/*/; do
  if [[ -d "$dir" ]]; then
    name="$(basename "$dir")"
    echo "  … 预编译插件：${name}"
  fi
done
