#!/usr/bin/env bash
# crate 分层（见 AGENTS.md「crate 按两条轴分」）：依赖只能从功能指向能力。
#   1. 插件（crates/plugins/*）在本仓库里只依赖 smelt-plugin-api，不依赖宿主、别的插件或
#      其它本仓库 crate；第三方库不管。
#   2. 只有组装插件的应用 smelt 能依赖插件，能力 crate 不认识任何功能。
# 只读 cargo metadata，不编译。
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

if [ -n "$violations" ]; then
  echo "✗ crate 依赖越过了分层："
  echo "$violations"
  exit 1
fi

echo "✓ crate 依赖符合分层"
