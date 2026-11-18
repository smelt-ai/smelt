#!/usr/bin/env bash
# crate 分层（见 AGENTS.md「crate 按两条轴分」）：依赖只能从功能指向能力。
#   1. 插件（crates/plugins/*）在本仓库里只依赖 smelt-plugin-api，不依赖宿主、别的插件或
#      其它本仓库 crate；第三方库不管。
#   2. 只有组装插件的应用 smelt 能依赖插件，能力 crate 不认识任何功能。
#   3. 应用 smelt 依赖插件只为组装：源码里点名插件只能是调它的 `register`，不写插件的视图、
#      命令、布局 id（插件 `OWNER` 开头的字符串）。删掉任何一个插件，宿主只少一行登记。
# 只读 cargo metadata 和源码，不编译。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

violations="$(
  cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml" | jq -r '
    def plugin: .manifest_path | test("/crates/plugins/[^/]+/Cargo.toml$");
    [.packages[] | select(plugin) | .name] as $plugins
    | .packages[]
    | . as $pkg
    | .dependencies[]
    | select(.path != null)
    | if ($pkg | plugin) then
        select(.name != "smelt-plugin-api")
        | "插件 \($pkg.name) 依赖了本仓库的 \(.name)：插件只经 smelt-plugin-api 拿宿主的东西"
      else
        select(.name as $dep | $plugins | index($dep))
        | select($pkg.name != "smelt")
        | "\($pkg.name) 依赖了插件 \(.name)：只有应用 smelt 组装插件，能力 crate 不认识功能"
      end
  ' | sort -u
)"

# 3. 宿主源码里的插件：插件 crate 只许调 `register`，插件来源（`const OWNER`）不许出现在
#    字符串里。
host_src="$root/crates/smelt/src"
host_violations=""
for manifest in "$root"/crates/plugins/*/Cargo.toml; do
  dir="$(dirname "$manifest")"
  crate="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$manifest" | head -1 | tr - _)"
  owner="$(grep -rhoE 'const OWNER: &str = "[^"]+"' "$dir/src" | head -1 | sed -E 's/.*"([^"]+)"/\1/' || true)"
  uses="$(grep -rnE "(^|[^a-z0-9_])${crate}::" "$host_src" --include='*.rs' | grep -vE "(^|[^a-z0-9_])${crate}::register\(" || true)"
  if [ -n "$uses" ]; then
    host_violations+="宿主点名了插件 ${crate} 的内部（只能调它的 register）："$'\n'"${uses}"$'\n'
  fi
  if [ -n "$owner" ]; then
    ids="$(grep -rnF "\"${owner}/" "$host_src" --include='*.rs' || true)"
    if [ -n "$ids" ]; then
      host_violations+="宿主写死了插件 ${owner} 的 id（视图、命令、布局归插件自己）："$'\n'"${ids}"$'\n'
    fi
  fi
done

if [ -n "$violations" ]; then
  echo "✗ crate 依赖越过了分层："
  echo "$violations"
fi
if [ -n "$host_violations" ]; then
  echo "✗ 宿主源码认识具体功能："
  printf '%s' "$host_violations"
fi
if [ -n "$violations" ] || [ -n "$host_violations" ]; then
  exit 1
fi

echo "✓ crate 依赖符合分层，宿主源码不认识具体功能"
