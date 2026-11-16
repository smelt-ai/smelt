#!/usr/bin/env python3
"""生成 Linux 远程桌面（install.sh --with-desktop）的图片，产物提交进 scripts/desktop/：

- xfwm4/*.png：窗口主题 Smelt。深色标题栏、上方圆角、左边红黄绿三个按钮，配色同 macOS
  深色模式。themerc 是手写的，不在这里生成。
- wallpaper.jpg：深色底上几团柔和的光，加一点噪点打散色带（深色渐变只有几十级灰，不加
  会一圈一圈的）。
- menu-icon.png：开始菜单按钮，吉祥物的白色剪影（眼睛和嘴挖空），放在顶栏。

用法：make-desktop-theme.py <吉祥物 PNG，256px 以上> —— make-icon.sh 渲染好 assets/icon.svg
后调它。只依赖 Pillow。
"""

import math
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

OUT = Path(__file__).resolve().parent / "desktop"

# 标题栏高 28、上角半径 10，同 macOS 的窗口。
H = 28
R = 10
SS = 4  # 超采样倍数：圆和圆角先画大再缩小，边缘才平滑。

# xfwm4 拿图片的 alpha 当窗框形状，只留 alpha 正好 255 的像素（mypixmap.c 的
# xfwmPixmapDrawFromGdkPixbuf），半透明一律挖掉。所以窗框全用不透明色，圆角在下采样后
# 按一半覆盖率取舍。
#
# 有焦点和没焦点的窗口用同一种标题栏底色，只靠文字颜色和按钮区分：xfwm4 鼠标移上去的
# prelight 图只有一张，焦点不在的窗口也用它，底色不同就会在按钮后面露出一块。
TITLE = (44, 44, 48)
BORDER = (14, 14, 16, 255)  # 窗口外圈 1px，开着合成器时外面还有阴影
HIGHLIGHT = 0.07  # 标题栏顶上一道亮边的强度
SEPARATOR = (24, 24, 26)  # 标题栏和内容之间的分隔线

BUTTONS = {
    # 名字同 xfwm4 的按钮：close、hide（最小化）、maximize。
    "close": ((255, 95, 87), (224, 68, 62)),
    "hide": ((254, 188, 46), (222, 161, 35)),
    "maximize": ((40, 200, 64), (26, 171, 41)),
}
BUTTON_W = 20  # 每个按钮的宽；圆直径 12，所以圆和圆之间空 8
DOT = 12
INACTIVE_DOT = ((78, 78, 82), (64, 64, 67))
GLYPH = (0, 0, 0, 150)


def mix(rgb, toward, t):
    return tuple(round(a + (b - a) * t) for a, b in zip(rgb, toward))


def title_column():
    """标题栏的一列（自上而下）：外框、亮边、底色……、分隔线。"""
    bg = TITLE
    col = [BORDER, mix(bg, (255, 255, 255), HIGHLIGHT) + (255,)]
    col += [bg + (255,)] * (H - 3)
    col.append(SEPARATOR + (255,))
    return col


def title_strip(width):
    img = Image.new("RGBA", (width, H))
    px = img.load()
    for y, c in enumerate(title_column()):
        for x in range(width):
            px[x, y] = c
    return img


def corner(side):
    """上方的圆角：画一段带圆角的标题栏，切下左边或右边 R 列。"""
    w = R * 3
    big = Image.new("RGBA", (w * SS, H * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(big)
    bg = TITLE
    # 外框、亮边、底色三层圆角矩形，下边伸出画面，只留上方的圆角。
    d.rounded_rectangle([0, 0, w * SS - 1, (H + R) * SS], R * SS, fill=BORDER)
    d.rounded_rectangle([SS, SS, w * SS - 1 - SS, (H + R) * SS], (R - 1) * SS,
                        fill=mix(bg, (255, 255, 255), HIGHLIGHT) + (255,))
    d.rounded_rectangle([SS, 2 * SS, w * SS - 1 - SS, (H + R) * SS], (R - 1) * SS,
                        fill=bg + (255,))
    img = big.resize((w, H), Image.LANCZOS)
    px = img.load()
    for y in range(H):
        for x in range(w):
            r, g, b, a = px[x, y]
            px[x, y] = (r, g, b, 255) if a >= 128 else (0, 0, 0, 0)
    # 分隔线和左右的外框不需要圆角，直接按像素补上。
    for x in range(w):
        px[x, H - 1] = SEPARATOR + (255,)
    for y in range(R, H):
        px[0, y] = BORDER
        px[w - 1, y] = BORDER
    return img.crop((0, 0, R, H)) if side == "left" else img.crop((w - R, 0, w, H))


def button(name, state):
    img = title_strip(BUTTON_W)
    big = Image.new("RGBA", (BUTTON_W * SS, H * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(big)
    fill, stroke = BUTTONS[name]
    if state == "inactive":
        fill, stroke = INACTIVE_DOT
    if state == "pressed":
        fill, stroke = mix(fill, (0, 0, 0), 0.18), mix(stroke, (0, 0, 0), 0.18)
    cx, cy = BUTTON_W * SS / 2, (H - 1) * SS / 2
    r = DOT * SS / 2
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=stroke + (255,))
    r2 = r - 0.5 * SS
    d.ellipse([cx - r2, cy - r2, cx + r2, cy + r2], fill=fill + (255,))
    if state in ("prelight", "pressed"):
        # 鼠标移上去才显示符号，同 macOS。
        a = 3 * SS
        w = round(1.3 * SS)
        if name == "close":
            d.line([cx - a, cy - a, cx + a, cy + a], fill=GLYPH, width=w)
            d.line([cx - a, cy + a, cx + a, cy - a], fill=GLYPH, width=w)
        elif name == "hide":
            d.line([cx - a - SS / 2, cy, cx + a + SS / 2, cy], fill=GLYPH, width=w)
        else:
            d.line([cx - a - SS / 2, cy, cx + a + SS / 2, cy], fill=GLYPH, width=w)
            d.line([cx, cy - a - SS / 2, cx, cy + a + SS / 2], fill=GLYPH, width=w)
    img.alpha_composite(big.resize((BUTTON_W, H), Image.LANCZOS))
    return img


def xfwm4_theme():
    out = OUT / "xfwm4"
    out.mkdir(exist_ok=True)
    strip = title_strip(1)
    edge = Image.new("RGBA", (1, 1), BORDER)
    for state in ("active", "inactive"):
        for i in range(1, 6):
            strip.save(out / f"title-{i}-{state}.png")
        corner("left").save(out / f"top-left-{state}.png")
        corner("right").save(out / f"top-right-{state}.png")
        # 左右和下边只有 1px 的外框。
        for part in ("left", "right", "bottom", "bottom-left", "bottom-right"):
            edge.save(out / f"{part}-{state}.png")
    for name in BUTTONS:
        for state in ("active", "inactive", "prelight", "pressed"):
            img = button(name, state)
            img.save(out / f"{name}-{state}.png")
            # 最大化之后用 toggled 那套；macOS 上两种状态长得一样。
            if name == "maximize":
                img.save(out / f"{name}-toggled-{state}.png")


def wallpaper():
    W, Hh = 2560, 1600
    w, h = 320, 200  # 先在小图上算光斑，再平滑放大，省得逐像素算 400 万次
    top, bottom = (16, 17, 23), (8, 8, 11)
    # 光是叠加上去的（同光照，越叠越亮），不是往某个颜色混：两团颜色混在一起会发浑。
    # （中心 x、中心 y、半径，都按宽度算比例；叠加的颜色）
    glows = [
        ((0.12, 0.05, 0.75), (20, 32, 96)),    # 左上大片的蓝
        ((0.92, 0.18, 0.50), (6, 42, 60)),     # 右上青蓝
        ((0.78, 0.95, 0.50), (84, 38, 10)),    # 右下一点暖光，同吉祥物的橙
    ]
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        base = mix(top, bottom, y / (h - 1))
        for x in range(w):
            c = list(base)
            for (gx, gy, gr), color in glows:
                dx = x / w - gx
                dy = (y - gy * h) / w
                t = max(0.0, 1 - math.hypot(dx, dy) / gr)
                t = t * t * (3 - 2 * t)  # smoothstep，边缘没有硬线
                c = [ci + col * t for ci, col in zip(c, color)]
            px[x, y] = tuple(min(255, round(v)) for v in c)
    img = img.resize((W, Hh), Image.BICUBIC)
    # 噪点打散色带：均值 128、标准差 1.5 的灰度噪声，减回 128 叠上去。
    noise = Image.effect_noise((W, Hh), 1.5).convert("RGB")
    img = ImageChops.add(img, noise, scale=1.0, offset=-128)
    img.save(OUT / "wallpaper.jpg", quality=92, optimize=True, subsampling=0)


def menu_icon(src):
    icon = Image.open(src).convert("RGBA").resize((64, 64), Image.LANCZOS)
    out = Image.new("RGBA", icon.size, (0, 0, 0, 0))
    ip, op = icon.load(), out.load()
    for y in range(icon.height):
        for x in range(icon.width):
            r, g, b, a = ip[x, y]
            lum = (r * 299 + g * 587 + b * 114) / 1000
            # 身体和眼白留白，深色的瞳孔和嘴挖空。
            keep = min(1.0, max(0.0, (lum - 70) / 60))
            op[x, y] = (255, 255, 255, round(a * keep))
    out.save(OUT / "menu-icon.png", optimize=True)


def main():
    if len(sys.argv) != 2:
        sys.exit(f"用法：{sys.argv[0]} <吉祥物 PNG>")
    xfwm4_theme()
    wallpaper()
    menu_icon(sys.argv[1])


if __name__ == "__main__":
    main()
