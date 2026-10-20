#!/usr/bin/env bash
# 把钉死的 Bun 1.4.3 放进 target/bundled-bun/<id>/bun。已有且可执行文件哈希相符就复用。
# 用法：scripts/fetch-bun.sh [darwin-arm64|darwin-x64|linux-x64|linux-arm64]
# `--managed-only` 只是不走 PATH，和默认行为一样。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUN_VERSION=1.4.3

host_id() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
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
    darwin-arm64|darwin-x64|linux-x64|linux-arm64) ID="$arg" ;;
    *) echo "用法：$0 [--managed-only] [darwin-arm64|darwin-x64|linux-x64|linux-arm64]" >&2; exit 2 ;;
  esac
done
ID="${ID:-$(host_id)}"

case "$ID" in
  darwin-arm64)
    ZIP=bun-darwin-aarch64.zip
    ARCHIVE_SHA=80afd4c069b46a6fa8f3ec520bebc750a7843e4a8d5b9583d29ad436c00a3403
    EXE_SHA=28c7dd0a06916aba284d570ef44a2d70e9c538eaf2ba58dfe0910525563b3f79
    ;;
  darwin-x64)
    ZIP=bun-darwin-x64.zip
    ARCHIVE_SHA=62660e6438eb872278877acca77e1152d5d031324df368d4a81b428211489d2d
    EXE_SHA=c2a945a9d33f9088b32ac3550d1d1fd485a7fc10d71c66f91ef0d33b41cefa7c
    ;;
  linux-x64)
    ZIP=bun-linux-x64-baseline.zip
    ARCHIVE_SHA=1fc2edac843102909e3a1be1d8d9802cc6071cf074e67e88231f4ffe0f8b397b
    EXE_SHA=7ce7d6b654eddeec20afddf2396317fba6dc19ab52788a27e894e1806bf8f24b
    ;;
  linux-arm64)
    ZIP=bun-linux-aarch64.zip
    ARCHIVE_SHA=efa9813da5ed72423bf847f916e8d2c47c0d776add972354026a75e10da9aa21
    EXE_SHA=029ece746ac9f970c76d8f48bdd1b1d0c52f3636b74d19f546f5e7c1165c0abe
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
EXE="$DEST/bun"
if [[ -x "$EXE" ]]; then
  current="$(sha256_file "$EXE")"
  if [[ "$current" == "$EXE_SHA" ]]; then
    printf '%s\n' "$EXE"
    exit 0
  fi
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
found="$(find "$tmp/out" -type f -name bun -print -quit)"
if [[ -z "$found" ]]; then
  echo "✗ 压缩包里没有 bun" >&2
  exit 1
fi
exe="$(sha256_file "$found")"
if [[ "$exe" != "$EXE_SHA" ]]; then
  echo "✗ bun 可执行文件哈希不符：$exe" >&2
  exit 1
fi
install -m 0755 "$found" "$EXE"
printf '%s\n' "$EXE"
