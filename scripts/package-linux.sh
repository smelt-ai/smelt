#!/usr/bin/env bash
# 打 Linux 运行时包：smeltd-<version>-linux-<arch>.tar.gz。
# 里面有 smeltd、smelt-notify、smelt-up、install.sh、desktop/，以及 smelt-pi、bun、photon wasm、native/。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|aarch64) ;;
  *) echo "不支持的架构：$ARCH" >&2; exit 1 ;;
esac

echo "▶ 编译 Linux 运行时 …"
cargo build --release --manifest-path "$ROOT/Cargo.toml" --bin smeltd --bin smelt-notify
"$ROOT/scripts/build-smelt-pi.sh"

NAME="smeltd-${VERSION}-linux-${ARCH}"
STAGE="$(mktemp -d)"
DIR="$STAGE/$NAME"
mkdir -p "$DIR/desktop"
cp "$ROOT/target/release/smeltd" "$DIR/smeltd"
cp "$ROOT/target/release/smelt-notify" "$DIR/smelt-notify"
cp "$ROOT/scripts/smelt-up.sh" "$DIR/smelt-up"
cp "$ROOT/scripts/install-smeltd.sh" "$DIR/install.sh"
cp -R "$ROOT/scripts/desktop/." "$DIR/desktop/"
cp "$ROOT/target/smelt-pi/smelt-pi" "$DIR/smelt-pi"
cp "$ROOT/target/smelt-pi/photon_rs_bg.wasm" "$DIR/photon_rs_bg.wasm"
cp "$ROOT/target/bundled-bun/$(
  case "$ARCH" in
    x86_64) echo linux-x64 ;;
    aarch64) echo linux-arm64 ;;
  esac
)/bun" "$DIR/bun"
if [[ -d "$ROOT/target/smelt-pi/native" ]]; then
  cp -R "$ROOT/target/smelt-pi/native" "$DIR/native"
fi
chmod +x "$DIR/smeltd" "$DIR/smelt-notify" "$DIR/smelt-up" "$DIR/install.sh" "$DIR/smelt-pi" "$DIR/bun"

mkdir -p "$ROOT/dist"
tar -C "$STAGE" -czf "$ROOT/dist/${NAME}.tar.gz" "$NAME"
rm -rf "$STAGE"
echo "✓ $ROOT/dist/${NAME}.tar.gz"
