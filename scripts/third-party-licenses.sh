#!/usr/bin/env bash
# 往 <输出目录> 写随发行包分发的许可证文件：LICENSE、NOTICE、THIRD_PARTY_LICENSES.txt。
# 用法：scripts/third-party-licenses.sh <输出目录>
#
# THIRD_PARTY_LICENSES.txt 分三段：
#   1. Rust 依赖：cargo-about 按整个 workspace 列，桌面、守护、Linux、Windows 包共用一份，宁多不漏。
#      允许的许可证只认 deny.toml 的 [licenses] allow，这里不另存一份。
#   2. 编进 smelt-agent 的 npm 依赖：agent/scripts/third-party-licenses.ts。
#   3. 编进 smelt-agent 的 Bun 运行时。
# 需要 cargo-about（cargo install cargo-about --locked）和 agent/node_modules（make smelt-agent 会装）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:?用法：$0 <输出目录>}"

command -v cargo-about >/dev/null || {
  echo "✗ 缺 cargo-about：cargo install cargo-about --locked" >&2
  exit 1
}
[[ -d "$ROOT/agent/node_modules" ]] || {
  echo "✗ 缺 agent/node_modules：先 make smelt-agent" >&2
  exit 1
}

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

accepted="$(awk '/^\[licenses\]/{s=1} s && /^allow = \[/{a=1; next} a && /^\]/{exit} a' "$ROOT/deny.toml" \
  | grep -o '"[^"]*"' | paste -sd, -)"
[[ -n "$accepted" ]] || { echo "✗ 没从 deny.toml 读到 [licenses] allow" >&2; exit 1; }
cat > "$work/about.toml" <<EOF
accepted = [$accepted]
# 构建脚本和测试用的依赖不进产物。
ignore-build-dependencies = true
ignore-dev-dependencies = true
# 本仓库自己的 crate（publish = false）不算第三方。
private = { ignore = true }
# 这几个 crate 的许可证文件 cargo-about 认不准，用它内置的修正。
workarounds = ["chrono", "ring", "rustls", "wasmtime"]
EOF

(cd "$ROOT" && cargo about generate --fail -c "$work/about.toml" \
  -o "$work/rust.txt" "$ROOT/scripts/third-party-licenses.hbs")

bun="$("$ROOT/scripts/fetch-bun.sh")"
"$bun" "$ROOT/agent/scripts/third-party-licenses.ts" > "$work/npm.txt"

bun_version="$(sed -n 's/^BUN_VERSION=//p' "$ROOT/scripts/fetch-bun.sh")"
bun_revision="$(sed -n 's/^BUN_REVISION=//p' "$ROOT/scripts/fetch-bun.sh")"

mkdir -p "$OUT"
cp "$ROOT/LICENSE" "$OUT/LICENSE"
cp "$ROOT/NOTICE" "$OUT/NOTICE"
{
  cat <<EOF
Third-party software included in Smelt

Smelt itself is licensed under the Apache License 2.0 (see LICENSE and NOTICE).
It includes the third-party software listed below, each under its own license.

################################################################################
Part 1: Rust crates
################################################################################
EOF
  cat "$work/rust.txt"
  cat <<EOF

################################################################################
Part 2: npm packages compiled into smelt-agent
################################################################################
EOF
  cat "$work/npm.txt"
  cat <<EOF

################################################################################
Part 3: Bun runtime compiled into smelt-agent
################################################################################

Bun ${bun_version}
License: MIT, with bundled components under their own licenses, including
JavaScriptCore and WebKit (LGPL-2.1 / BSD).
Full license: https://github.com/oven-sh/bun/blob/${bun_revision}/LICENSE.md
WebKit / JavaScriptCore source: https://github.com/oven-sh/WebKit
EOF
} > "$OUT/THIRD_PARTY_LICENSES.txt"
echo "✓ 许可证文件：$OUT"
