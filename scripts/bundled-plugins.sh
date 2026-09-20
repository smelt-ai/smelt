#!/usr/bin/env bash
# 列出随宿主分发的插件包。打包脚本和 Makefile 共用，避免两处各写一份名单——
# 名单写死过一次，新增插件就会悄悄漏出产物。
#
#   bundled-plugins.sh dirs                # 每行一个插件源目录
#   bundled-plugins.sh newer-than <path>  # 每行一个内容比 <path> 新的 bundled 包目录
#
# manifest 里 "bundled": false 的测试夹具不算在内。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON="${SMELT_PYTHON:-python3}"
mode="${1:-dirs}"
reference="${2:-}"

for manifest in "$ROOT"/plugins/*/plugin.json; do
  [[ -f "$manifest" ]] || continue
  if ! manifest_info="$("$PYTHON" "$ROOT/scripts/plugin-manifest-info.py" "$manifest")"; then
    exit 1
  fi
  read -r _plugin_id entrypoint bundled \
    <<<"$manifest_info"
  [[ "$bundled" == "true" ]] || continue
  dir="$(dirname "$manifest")"
  if [[ ! -f "$dir/$entrypoint" || -L "$dir/$entrypoint" ]]; then
    echo "✗ 缺少或非法的 package entrypoint：$dir/$entrypoint" >&2
    exit 1
  fi
  case "$mode" in
    dirs) echo "$dir" ;;
    newer-than)
      [[ -n "$reference" && -e "$reference" ]] || {
        echo "用法: $(basename "$0") newer-than <已有路径>" >&2
        exit 2
      }
      changed=0
      package_files=(
        "$manifest"
        "$dir/plugin-ui.json"
        "$dir/plugin-input.json"
        "$dir/plugin-agent.json"
      )
      package_files+=("$dir/$entrypoint")
      entrypoint_dir="$(dirname "$dir/$entrypoint")"
      if [[ -d "$entrypoint_dir" ]]; then
        for module in "$entrypoint_dir"/*; do
          [[ -f "$module" ]] || continue
          base="$(basename "$module")"
          case "$base" in
            .*|*.test.ts|*.test.js|*.spec.ts|*.spec.js) continue ;;
          esac
          package_files+=("$module")
        done
      fi
      for package_file in "${package_files[@]}"; do
        [[ -f "$package_file" && "$package_file" -nt "$reference" ]] && changed=1
      done
      for asset_dir in "$dir/web" "$dir/assets"; do
        if [[ -d "$asset_dir" ]] \
          && find "$asset_dir" -type f -newer "$reference" -print -quit | grep -q .; then
          changed=1
        fi
      done
      if [[ "$changed" == "1" ]]; then
        echo "$dir"
      fi
      ;;
    *) echo "用法: $(basename "$0") [dirs|newer-than <路径>]" >&2; exit 2 ;;
  esac
done
