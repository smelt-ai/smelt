"""dmgbuild 配置：Smelt.dmg 挂载窗口的外观（窗口尺寸 / 背景 / 图标摆位）。

由 scripts/package-mac.sh 调用。dmgbuild 用 exec() 执行本文件，这里拿不到 __file__，
所以路径一律走环境变量传入：
  SMELT_APP     —— 要打进 dmg 的 Smelt.app 路径
  SMELT_DMG_BG  —— 背景图（retina tiff）

图标坐标必须与 scripts/make-dmg-bg.py 画的背景图对齐：那边把标签文字画在
app 图标中心 (160, 210) 和应用程序 (480, 210) 的正下方，两边对不上就会错位。
"""

import os

application = os.environ["SMELT_APP"]
appname = os.path.basename(application)

# 只读压缩映像，zlib 最高压缩
format = "UDZO"
compression_level = 9

files = [application]
symlinks = {"Applications": "/Applications"}

background = os.environ["SMELT_DMG_BG"]

# 640×428：内容区正好容下 640×400 的背景图，余下 28 是标题栏。
window_rect = ((200, 120), (640, 428))
icon_size = 128
icon_locations = {
    appname: (160, 210),
    "Applications": (480, 210),
}
