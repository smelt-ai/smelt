#!/usr/bin/env bash
# 打印 macOS 26 SDK 路径。
# xcrun --sdk macosx 选磁盘上版本号最高的 SDK。旁边有 27 时，26 的链接器读不懂 arm64e.x1。
set -euo pipefail

dev="$(xcode-select -p 2>/dev/null || true)"
for sdk in \
  "${dev}/SDKs/MacOSX26.sdk" \
  "${dev}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk"
do
  if [[ -n "$dev" && -d "$sdk" ]]; then
    printf '%s\n' "$sdk"
    exit 0
  fi
done

echo "✗ 未找到 MacOSX26.sdk（xcode-select -p = ${dev:-未设置}）" >&2
exit 1
