#!/usr/bin/env bash
# 给已经拷进 .app 的 AppIcon.icns 加一枚角标。工具或字体库缺失时失败，调用方继续用原图标。
# 不改仓库里的 assets/AppIcon.icns。
set -euo pipefail

icon="${1:?需要 icns 路径}"
if ! command -v iconutil >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
iconutil -c iconset "$icon" -o "$work/AppIcon.iconset"

python3 - "$work/AppIcon.iconset" <<'PY'
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit(1)

folder = Path(sys.argv[1])
pngs = list(folder.glob("*.png"))
if not pngs:
    sys.exit(1)
for path in pngs:
    image = Image.open(path).convert("RGBA")
    draw = ImageDraw.Draw(image)
    width, _height = image.size
    badge = max(8, width // 5)
    margin = max(2, width // 32)
    box = (width - badge - margin, margin, width - margin, badge + margin)
    draw.rounded_rectangle(box, radius=max(2, badge // 4), fill=(196, 92, 18, 255))
    image.save(path)
PY

iconutil -c icns "$work/AppIcon.iconset" -o "$icon"
