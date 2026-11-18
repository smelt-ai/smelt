#!/usr/bin/env bash
# 把钉死的 Bun 放进 target/bundled-bun/<id>/bun。已有且可执行文件哈希相符就复用。
# 从 npm 取 Bun 官方发的 @oven/bun-<平台> 包：正式版和 canary 都有，版本号发出去就不会再变。
# GitHub 的 canary 标签每天被新构建覆盖，钉不住。
# 用法：scripts/fetch-bun.sh [darwin-arm64|darwin-x64|linux-x64|linux-arm64|windows-x64]
# `--managed-only` 只是不走 PATH，和默认行为一样。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# canary：Bun 自报的 Node 版本要 26.12.1 以上，REA 6.8 起才跑得起来（1.4.4 报 26.3.0）。
# 1.4.4 之后的正式版发出来就换回正式版。
BUN_VERSION=1.4.4-canary.20261117.1
# 这个版本对应的提交，第三方许可证清单链到它的 LICENSE.md。
BUN_REVISION=67d1f55fb37905fca35379daa7a2ca07595c552d

host_id() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  # GitHub windows-latest 的 bash 是 MINGW/MSYS；有的环境只设了 OS=Windows_NT。
  case "$os" in
    MINGW*|MSYS*|CYGWIN*) echo windows-x64; return ;;
  esac
  if [[ "${OS:-}" == "Windows_NT" ]]; then
    echo windows-x64
    return
  fi
  case "$os-$arch" in
    Darwin-arm64) echo darwin-arm64 ;;
    Darwin-x86_64) echo darwin-x64 ;;
    Linux-x86_64) echo linux-x64 ;;
    Linux-aarch64) echo linux-arm64 ;;
    *) echo "不支持的 Bun 宿主：$os-$arch" >&2; exit 1 ;;
  esac
}

ID=""
for arg in "$@"; do
  case "$arg" in
    --managed-only) ;;
    darwin-arm64|darwin-x64|linux-x64|linux-arm64|windows-x64) ID="$arg" ;;
    *) echo "用法：$0 [--managed-only] [darwin-arm64|darwin-x64|linux-x64|linux-arm64|windows-x64]" >&2; exit 2 ;;
  esac
done
ID="${ID:-$(host_id)}"

case "$ID" in
  darwin-arm64)
    PKG=darwin-aarch64
    ARCHIVE_SHA=96a926bd444eb97c191faecf1d722bffb0787ed6a92fd7c6c9a1266851e85f8e
    EXE_SHA=f4dc3ab64a976bf0ef434d3cf0370902ff34dba11ad5b914c33c250c17428823
    ;;
  darwin-x64)
    PKG=darwin-x64
    ARCHIVE_SHA=97ea5d3338366d8184045b18414c29f9a90f77222fe855116b7f1f7c68612006
    EXE_SHA=7b6d42af986ac61516351e08296ddd985561dec962f443f086c99017dbd7e8eb
    ;;
  linux-x64)
    PKG=linux-x64-baseline
    ARCHIVE_SHA=82bd54499b6d03dbe02122cd19895b1c24b8043e734f789aed6f74cd2a44a512
    EXE_SHA=7ffbea5287a829735bea3926a0ca9e09e6e94513ac9e9b668daf399797ccd54f
    ;;
  linux-arm64)
    PKG=linux-aarch64
    ARCHIVE_SHA=eae6c5acb5c57a0ad4faa9daf8c7d0c947defe5fbc3e6fb20366977d57e9efec
    EXE_SHA=657c4833a441ee3721b44bfb5f293fbafeb43b3089d5c7346127ab4372b59f18
    ;;
  windows-x64)
    PKG=windows-x64
    ARCHIVE_SHA=07fd69b8b1c78e951148aafd827a3611ff05f35c26262010627d59cd1dc8f118
    # 包里是 package/bin/bun.exe。
    EXE_SHA=e54341c155c12956bc7169a8e3de6ccd013319673cf4a95bb4364618375a19e4
    BIN_NAME=bun.exe
    ;;
esac

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

DEST="$ROOT/target/bundled-bun/$ID"
BIN_NAME="${BIN_NAME:-bun}"
EXE="$DEST/$BIN_NAME"
if [[ -n "$EXE_SHA" && -x "$EXE" ]]; then
  current="$(sha256_file "$EXE")"
  if [[ "$current" == "$EXE_SHA" ]]; then
    printf '%s\n' "$EXE"
    exit 0
  fi
elif [[ -z "$EXE_SHA" && -f "$EXE" ]]; then
  printf '%s\n' "$EXE"
  exit 0
fi

mkdir -p "$DEST"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
TGZ="bun-${PKG}-${BUN_VERSION}.tgz"
url="https://registry.npmjs.org/@oven/bun-${PKG}/-/${TGZ}"
curl -fsSL --retry 3 -o "$tmp/$TGZ" "$url"
archive="$(sha256_file "$tmp/$TGZ")"
if [[ "$archive" != "$ARCHIVE_SHA" ]]; then
  echo "✗ Bun 压缩包哈希不符：$archive" >&2
  exit 1
fi
mkdir "$tmp/out"
tar -xzf "$tmp/$TGZ" -C "$tmp/out"
found="$(find "$tmp/out" -type f -name "$BIN_NAME" -print -quit)"
if [[ -z "$found" ]]; then
  echo "✗ 压缩包里没有 $BIN_NAME" >&2
  exit 1
fi
if [[ -n "$EXE_SHA" ]]; then
  exe="$(sha256_file "$found")"
  if [[ "$exe" != "$EXE_SHA" ]]; then
    echo "✗ bun 可执行文件哈希不符：$exe" >&2
    exit 1
  fi
fi
if command -v install >/dev/null 2>&1; then
  install -m 0755 "$found" "$EXE"
else
  cp "$found" "$EXE"
fi
printf '%s\n' "$EXE"
