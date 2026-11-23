#!/usr/bin/env bash
# 把仓库里的自带 JS 插件（plugins/<名字>/，有 smelt-plugin.json 的才算）放进安装包：
# macOS 是 Smelt.app/Contents/Resources/plugins，Windows 是安装目录下的 plugins。
# 发版的 App 从那里只读加载（crates/smelt/src/plugins/builtin.rs）。
#
# 拷哪些文件按 .gitignore 定（编辑器生成的类型声明、node_modules 都在里面），另外跳过 tests/：
# 测试和它用的宿主模块替身只在仓库里跑。
#
# 用法：scripts/stage-plugins.sh <目标 plugins 目录>（整个替换）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:?用法：scripts/stage-plugins.sh <目标 plugins 目录>}"

rm -rf "$DEST"
mkdir -p "$DEST"
cd "$ROOT"
git ls-files -co --exclude-standard -z -- plugins | while IFS= read -r -d '' file; do
  # 已经在工作区删掉的跟踪文件。
  [[ -f "$file" ]] || continue
  rel="${file#plugins/}"
  name="${rel%%/*}"
  [[ -f "plugins/${name}/smelt-plugin.json" ]] || continue
  case "${rel#*/}" in
    tests/*) continue ;;
  esac
  mkdir -p "$DEST/$(dirname "$rel")"
  cp "$file" "$DEST/$rel"
done
for dir in "$DEST"/*/; do
  [[ -d "$dir" ]] && echo "  … 打包插件：$(basename "$dir")"
done
