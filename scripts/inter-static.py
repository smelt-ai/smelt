# 从 Inter 可变字体切出界面用的几个静态字重，产物在 assets/fonts/Inter-*.ttf。
#
# 为什么不直接带可变字体：GPUI（gpui-pre 0.3.11，macOS 走 font-kit / CoreText）按 CSS
# 字重在同族字面里挑最近的一个，从不设可变轴，可变字体只画得出默认实例（400）。所以每个
# 用到的字重都要一份静态字面：400、440、500、530、600、700（界面字 ui 是 440、
# ui_medium 是 530）。opsz 定在 14，接近界面 11–15 的字号。
#
# 源文件：Inter 4.001 的 InterVariable.ttf
# （sha256 0be2399ea925f1f83ff974764761da9860ec50742ed29a5d4c1ffd0c5c7ac3a8）。许可证 OFL 1.1，
# 无保留字体名，改过的字体可以继续叫 Inter；许可证原文在 assets/fonts/Inter-OFL.txt。
#
# 用法（要 fontTools）：python3 scripts/inter-static.py InterVariable.ttf assets/fonts
# 改字重列表后，kit.rs 的 UI_FONTS 一起改。
import sys

from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

WEIGHTS = [400, 440, 500, 530, 600, 700]
NAMES = {400: "Regular", 500: "Medium", 600: "SemiBold", 700: "Bold"}


def main(src: str, out: str) -> None:
    for weight in WEIGHTS:
        font = instancer.instantiateVariableFont(
            TTFont(src), {"wght": weight, "opsz": 14}, inplace=False
        )
        style = NAMES.get(weight, f"W{weight}")
        name = font["name"]
        # 去掉排版族名，让每个字面的族名都是 Inter，GPUI 按族名找到同一族再按字重挑。
        for name_id in (16, 17, 21, 22, 25):
            name.removeNames(nameID=name_id)
        name.setName("Inter", 1, 3, 1, 0x409)
        name.setName("Inter", 1, 1, 0, 0)
        for platform in [(3, 1, 0x409), (1, 0, 0)]:
            name.setName(style, 2, *platform)
            name.setName(f"Inter {style}", 4, *platform)
            name.setName(f"Inter-{style}", 6, *platform)
            name.setName(
                f"Inter {style}; static instance wght={weight} opsz=14", 3, *platform
            )
        os2 = font["OS/2"]
        os2.usWeightClass = weight
        bold = weight >= 700
        os2.fsSelection = (os2.fsSelection & ~0b1100001) | (0b100000 if bold else 0b1000000)
        font["head"].macStyle = 1 if bold else 0
        if "STAT" in font:
            del font["STAT"]
        path = f"{out}/Inter-{style}.ttf"
        font.save(path)
        print(path)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
