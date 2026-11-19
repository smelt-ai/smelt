#!/usr/bin/env bash
# 把 agent/ 编成单个 smelt-agent，只编本机平台（不交叉编译）。
# 用法：agent/scripts/build-smelt-agent.sh [--out dir]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/target/smelt-agent"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    *) echo "用法：$0 [--out dir]" >&2; exit 2 ;;
  esac
done

is_windows_host() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) return 0 ;;
  esac
  [[ "${OS:-}" == "Windows_NT" ]]
}
# pi-tui 原生模块的目录布局：平台、架构、文件名后缀。
native_layout() {
  if is_windows_host; then
    echo win32 x64 ""
    return
  fi
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo darwin arm64 "" ;;
    Darwin-x86_64) echo darwin x64 "" ;;
    Linux-x86_64) echo linux x64 -x11 ;;
    Linux-aarch64) echo linux arm64 -x11 ;;
    *) echo "不支持的编译宿主" >&2; exit 1 ;;
  esac
}

BUN="$("$ROOT/scripts/fetch-bun.sh")"
mkdir -p "$OUT"
OUT_BIN="$OUT/smelt-agent"
if is_windows_host; then
  OUT_BIN="$OUT/smelt-agent.exe"
fi

(
  cd "$ROOT/agent"
  "$BUN" install --frozen-lockfile
)
# --splitting 在 compile-smelt-agent.ts 里打开。图片 worker 和 codemode worker
# 都要过冒烟（compiled-smoke.ts）。说明符改写也在那个脚本里：相对的 ./src 路径仍然找不到。
"$BUN" "$ROOT/agent/scripts/compile-smelt-agent.ts" "$OUT_BIN"

WASM="$ROOT/agent/node_modules/@silvia-odwyer/photon-node/photon_rs_bg.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "✗ 找不到 photon_rs_bg.wasm" >&2
  exit 1
fi
cp "$WASM" "$OUT/photon_rs_bg.wasm"
if ! is_windows_host; then
  chmod +x "$OUT_BIN"
fi
# 内置技能跟 smelt-agent 一起发布，智能体勾选了才用 --skill 传进去，不进 Pi 的自动发现。
rm -rf "$OUT/skills"
cp -R "$ROOT/agent/skills" "$OUT/skills"

read -r platform arch suffix <<<"$(native_layout)"
node_src="$ROOT/agent/node_modules/@earendil-works/pi-tui/native/${platform}/prebuilds/${platform}-${arch}/${platform}-platform${suffix}.node"
native_dest="$OUT/native/${platform}/prebuilds/${platform}-${arch}"
if [[ -f "$node_src" ]]; then
  mkdir -p "$native_dest"
  cp "$node_src" "$native_dest/"
fi

"$BUN" "$ROOT/agent/scripts/compiled-smoke.ts" "$OUT_BIN"
printf '%s\n' "$OUT_BIN"
