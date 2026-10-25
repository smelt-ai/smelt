#!/usr/bin/env bash
# 把 packages/pi-agent 编成单个 smelt-pi。
# 用法：scripts/build-smelt-pi.sh [--target bun-...] [--out dir]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/target/smelt-pi"
TARGET=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    *) echo "用法：$0 [--target bun-...] [--out dir]" >&2; exit 2 ;;
  esac
done

is_windows_host() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) return 0 ;;
  esac
  [[ "${OS:-}" == "Windows_NT" ]]
}
host_target() {
  if is_windows_host; then
    echo bun-windows-x64
    return
  fi
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo bun-darwin-arm64 ;;
    Darwin-x86_64) echo bun-darwin-x64 ;;
    Linux-x86_64) echo bun-linux-x64-baseline ;;
    Linux-aarch64) echo bun-linux-arm64 ;;
    *) echo "不支持的编译宿主" >&2; exit 1 ;;
  esac
}
host_id() {
  if is_windows_host; then
    echo windows-x64
    return
  fi
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo darwin-arm64 ;;
    Darwin-x86_64) echo darwin-x64 ;;
    Linux-x86_64) echo linux-x64 ;;
    Linux-aarch64) echo linux-arm64 ;;
  esac
}
native_layout() {
  case "$1" in
    bun-darwin-arm64) echo darwin arm64 "" ;;
    bun-darwin-x64) echo darwin x64 "" ;;
    bun-linux-x64|bun-linux-x64-baseline) echo linux x64 -x11 ;;
    bun-linux-arm64) echo linux arm64 -x11 ;;
    bun-windows-x64) echo win32 x64 "" ;;
    *) echo "不认识的 bun target：$1" >&2; exit 1 ;;
  esac
}

TARGET="${TARGET:-$(host_target)}"
BUN="$("$ROOT/scripts/fetch-bun.sh")"
mkdir -p "$OUT"
OUT_BIN="$OUT/smelt-pi"
if [[ "$TARGET" == bun-windows-x64 ]]; then
  OUT_BIN="$OUT/smelt-pi.exe"
fi

(
  cd "$ROOT/packages/pi-agent"
  "$BUN" install --frozen-lockfile
)
# --splitting 在 compile-smelt-pi.ts 里打开。Bun 1.4.3 上图片 worker 和
# codemode worker 都过了冒烟。说明符改写也在那个脚本里：相对的 ./src 路径仍然找不到。
"$BUN" "$ROOT/scripts/compile-smelt-pi.ts" "$OUT_BIN" "$TARGET"

WASM="$ROOT/packages/pi-agent/node_modules/@silvia-odwyer/photon-node/photon_rs_bg.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "✗ 找不到 photon_rs_bg.wasm" >&2
  exit 1
fi
cp "$WASM" "$OUT/photon_rs_bg.wasm"
if [[ "$TARGET" != bun-windows-x64 ]]; then
  chmod +x "$OUT_BIN"
fi
# 内置技能跟 smelt-pi 一起发布，智能体勾选了才用 --skill 传进去，不进 Pi 的自动发现。
rm -rf "$OUT/skills"
cp -R "$ROOT/packages/pi-agent/skills" "$OUT/skills"

read -r platform arch suffix <<<"$(native_layout "$TARGET")"
node_src="$ROOT/packages/pi-agent/node_modules/@earendil-works/pi-tui/native/${platform}/prebuilds/${platform}-${arch}/${platform}-platform${suffix}.node"
native_dest="$OUT/native/${platform}/prebuilds/${platform}-${arch}"
if [[ -f "$node_src" ]]; then
  mkdir -p "$native_dest"
  cp "$node_src" "$native_dest/"
fi

host="$(host_target)"
if [[ "$TARGET" == "$host" ]]; then
  "$BUN" "$ROOT/scripts/compiled-smoke.ts" "$OUT_BIN"
fi
printf '%s\n' "$OUT_BIN"
