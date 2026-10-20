#!/usr/bin/env bash
# 把钉死的 Bun 1.4.0 放进 target/bundled-bun/<id>/bun。已有且可执行文件哈希相符就复用。
# 用法：scripts/fetch-bun.sh [darwin-arm64|darwin-x64|linux-x64|linux-arm64]
# `--managed-only` 只是不走 PATH，和默认行为一样。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUN_VERSION=1.4.0

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
    ARCHIVE_SHA=c669e97f6164e1c96e0701748db98dfa77492908cbd8394c7557134a735de381
    EXE_SHA=539598c775882420b9d8deb7dc14d845f20f7d26f5600c50ab067dde6ac3f3bf
    ;;
  darwin-x64)
    ZIP=bun-darwin-x64.zip
    ARCHIVE_SHA=1d0211b8f1dc991182344687ad15e72ee86f154845a5f7fa477994cd341dd9b0
    EXE_SHA=ca8a18d0116d7b6b19f53bb0d8c48e487c0757cab4dc3f4f8cc5e43a44cd75d8
    ;;
  linux-x64)
    ZIP=bun-linux-x64-baseline.zip
    ARCHIVE_SHA=184fb4595f0d401a217cf7c78c1bc430ba83314dab7a8b94805babbf7fa7097f
    EXE_SHA=33d56b070be6a9e3da0ab013038b43d1645d0534ca811ecdba4472599117eb4b
    ;;
  linux-arm64)
    ZIP=bun-linux-aarch64.zip
    ARCHIVE_SHA=4b1a332ee861983eb93bcfe6f770fff94e3e31b2c388bdaea3c8ed35e58eed0e
    EXE_SHA=086c4121c8738a8e0f5ed730e8a461bc3973b4444e372ddb77aef9a747fa2ae9
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
