#!/usr/bin/env bash
# 把钉死的 Bun 1.4.4 放进 target/bundled-bun/<id>/bun。已有且可执行文件哈希相符就复用。
# 用法：scripts/fetch-bun.sh [darwin-arm64|darwin-x64|linux-x64|linux-arm64|windows-x64]
# `--managed-only` 只是不走 PATH，和默认行为一样。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUN_VERSION=1.4.4

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
    ZIP=bun-darwin-aarch64.zip
    ARCHIVE_SHA=c240d7f0ccb90c263a849fcdb09b7ee62630d921348ed6aafda86b9d577bc510
    EXE_SHA=0a2237cdcfa0d16ef501d9acca9f0026309ef348bea7393a64e8d1e3609565cc
    ;;
  darwin-x64)
    ZIP=bun-darwin-x64.zip
    ARCHIVE_SHA=50a3491f73abc2fe81673171a85762d3bc2cc1680ccecadd983de99adf77977e
    EXE_SHA=c35307c7d45d65c5a385a58c4a6cb0bb9499e4e626abdee207c177a77ba666ab
    ;;
  linux-x64)
    ZIP=bun-linux-x64-baseline.zip
    ARCHIVE_SHA=e0446eb55995772eb9894bebae9a6be368d985a8b9271ee124486f9946eedd76
    EXE_SHA=10702245766b225e70bbf6235fd6b47214fd94b4760d00ffaed8ab0df57ac235
    ;;
  linux-arm64)
    ZIP=bun-linux-aarch64.zip
    ARCHIVE_SHA=baa6bb36f51d6e58561e02bbea825038a9df454f673d6d776e8bbf86f8c7850e
    EXE_SHA=354c2b049c716aca07b2e5cddb887928ac58950675d1258bb6312083917793b6
    ;;
  windows-x64)
    ZIP=bun-windows-x64.zip
    ARCHIVE_SHA=cd2c0fccc78f3ede1f50ca827a15f063eda13d4d150a3d0674f4f6e323d864a5
    # 压缩包里是 bun-windows-x64/bun.exe。
    EXE_SHA=cb1d8e6564e655e49d30cead359750a1cbbd12f42651caa4b62244d7bd93c244
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
url="https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/${ZIP}"
curl -fsSL --retry 3 -o "$tmp/$ZIP" "$url"
archive="$(sha256_file "$tmp/$ZIP")"
if [[ "$archive" != "$ARCHIVE_SHA" ]]; then
  echo "✗ Bun 压缩包哈希不符：$archive" >&2
  exit 1
fi
unzip -q "$tmp/$ZIP" -d "$tmp/out"
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
