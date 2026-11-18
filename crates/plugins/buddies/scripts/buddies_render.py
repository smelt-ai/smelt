# SPDX-License-Identifier: GPL-3.0-or-later
# 这个文件按 GPL：它 `import bpy`，Blender 认为调它接口的脚本是 Blender 的衍生作品。渲出来的图
# 不受此约束；仓库里别的文件（含打包脚本 buddies_pack.py）照旧是仓库的许可。
"""用 Blender 渲会话行首 3D 小人的零件，再由 buddies_pack.py 打包编进插件，插件在运行时按姿势拼。

不渲整段动画：动作由插件里的代码现场算（src/buddy/），这里只出零件和拼装用的位置。
gpui 转不了彩色图，会转的零件（胳膊、手里的道具、方向盘）只渲 KEYS 个关键朝向，插件在运行时取最近
的一张在平面上补转（绕朝着镜头的轴转、正交镜头，补转出的形状和直接渲的一样，只有高光跟着偏一点）。
转身（绕竖轴）在平面上补不出来：身子、腮红、眼睛、嘴、戴的每 360 / FACINGS 度渲一个朝向，插件
把夹着的两张五官往对方挪着混起来；肩按朝向记下位置和在不在身子后面，胳膊照旧在平面上转。
身子和胳膊只渲一套中性灰，插件按颜色乘上去。颜色、曝光和色调映射见 COLORS、EXPOSURE、TONE。

小人是糯米团子：压扁的蛋形身子（皮有一点透光），两片小鳍当手，两只扁脚，大黑眼睛。

  身子    中性灰，含脚，每个朝向一张，锚点在脚底。
  腮红    身子当遮挡（holdout）只留露出来的部分，框和锚点跟身子一样，叠在身子上。每个朝向一张。
  胳膊    中性灰的小鳍，KEYS 个朝向，锚点在肩。
  道具    每样 KEYS 个朝向，锚点在握的地方。
  脸      眼睛（睁、笑眯、闭，左右各一）、嘴（笑、平、张），每个朝向一张，和腮红一样拿身子当
          遮挡，转到背面的被挡掉。
  戴的    头顶（帽子、头发、花）、脸上（眼镜、墨镜）、脖子（领结、围巾）各一组，按种子各自落。
          可换色的主料和固定色的点缀分两张渲（base、trim），插件给主料乘色再叠上点缀。都拿
          身子当遮挡，框和锚点跟身子一样，跟着身子压扁、转身。
  身前的  码农的电脑、写手的本子，各按亮着几行出 5 张。
  卡丁车  赛车手的：车头、车尾各按左右倾（-30° 到 30°）出 7 张，方向盘 KEYS 个朝向。
  特效    火星、泡泡、水滴、音符、灰、点、感叹号、灯泡、星星、爱心、速度线、彩纸、嫩芽。

运行（要装 Blender，`brew install --cask blender`）：

  blender --background --factory-startup --python crates/plugins/buddies/scripts/buddies_render.py \\
      -- target/buddies

输出 <目录>/sprites/<序号>.png、sprites.json（每张的名字、大小、锚点，按成图像素）和 rig.json
（脚底在画布上的位置、每个朝向的肩、头顶上方、身前物件、道具尖端相对脚底或握点的位置）。渲图是成图的两倍，
打包时缩下去。

场景约定：Blender 的 z 朝上，小人面朝 -y 再往右转一点（YAW），脚底在原点；正交镜头从前上方看，
整个小人的画布是 36pt（成图 72px）。画面里的角度从正上方起顺时针算。
"""
import json
import math
import os
import sys

import bmesh
import bpy
from mathutils import Matrix, Quaternion, Vector

SAMPLES = 32

def oklch(lightness, chroma, hue):
    """OKLCH 转 sRGB（0 到 1，出色域的截断）。"""
    a, b = chroma * math.cos(math.radians(hue)), chroma * math.sin(math.radians(hue))
    l_ = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3
    m_ = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3
    s_ = (lightness - 0.0894841775 * a - 1.2914855480 * b) ** 3
    rgb = (4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
           -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
           -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_)
    encode = lambda v: 1.055 * v ** (1 / 2.4) - 0.055 if v > 0.0031308 else v * 12.92
    return tuple(round(min(max(encode(max(v, 0)), 0), 1), 4) for v in rgb)


# 两套颜色都在 OKLCH 里定：同一套亮度、彩度一样，只换色相，摆在一起不会有哪个特别跳。
# 身子色是糯米团子那样又浅又淡的、色相均分一圈；戴的颜色深一档、彩度高一档，戴在浅身子上
# 分得开，另加一个几乎不带色相的深灰（浅色的配在浅身子上分不开，不要）。都不按颜色渲，打包
# 脚本按 OKLCH 算插件乘的系数、排除撞色的。
COLORS = [(name, (0.85, 0.075, hue)) for name, hue in [
    ("rose", 0), ("salmon", 30), ("apricot", 60), ("lemon", 95), ("lime", 125), ("green", 155),
    ("mint", 180), ("aqua", 205), ("sky", 235), ("periwinkle", 270), ("lilac", 300), ("orchid", 330),
]]
WEAR_COLORS = [
    ("red", (0.58, 0.16, 25)), ("amber", (0.74, 0.14, 75)), ("green", (0.56, 0.12, 150)),
    ("teal", (0.58, 0.1, 200)), ("blue", (0.5, 0.13, 260)), ("plum", (0.5, 0.12, 320)),
    ("charcoal", (0.38, 0.015, 260)),
]

INK = (0.13, 0.12, 0.14)


def srgb(color):
    return tuple(((v + 0.055) / 1.055) ** 2.4 if v > 0.04045 else v / 12.92 for v in color)


def rad(*degrees):
    return tuple(math.radians(d) for d in degrees)


# ---- 材质 ----

MATS = {}


def principled(name):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    return mat, mat.node_tree.nodes["Principled BSDF"]


def new_clay(color, rough, grain, coat=0.0, sheen=0.0):
    mat, bsdf = principled("clay")
    bsdf.inputs["Base Color"].default_value = (*srgb(color), 1)
    bsdf.inputs["Roughness"].default_value = rough
    if coat:
        bsdf.inputs["Coat Weight"].default_value = coat
        bsdf.inputs["Coat Roughness"].default_value = 0.05
    if sheen:
        bsdf.inputs["Sheen Weight"].default_value = sheen
        bsdf.inputs["Sheen Roughness"].default_value = 0.4
    if grain:
        nodes, links = mat.node_tree.nodes, mat.node_tree.links
        noise = nodes.new("ShaderNodeTexNoise")
        noise.inputs["Scale"].default_value = 60.0
        noise.inputs["Detail"].default_value = 4.0
        bump = nodes.new("ShaderNodeBump")
        bump.inputs["Strength"].default_value = grain
        bump.inputs["Distance"].default_value = 0.01
        links.new(noise.outputs["Fac"], bump.inputs["Height"])
        links.new(bump.outputs["Normal"], bsdf.inputs["Normal"])
    return mat


def clay(color, rough=0.7, grain=0.0, coat=0.0, sheen=0.0):
    """塑胶玩具似的料：`grain` 是表面颗粒（织物、扫帚头），`coat` 是一层清漆（亮面），`sheen`
    是绒面的边缘泛光（毛线、布）。同参数的共用一份。"""
    key = ("clay", color, rough, grain, coat, sheen)
    if key not in MATS:
        MATS[key] = new_clay(color, rough, grain, coat, sheen)
    return MATS[key]


def glow(color, strength):
    key = ("glow", color, strength)
    if key not in MATS:
        mat, bsdf = principled("glow")
        bsdf.inputs["Base Color"].default_value = (*srgb(color), 1)
        bsdf.inputs["Emission Color"].default_value = (*srgb(color), 1)
        bsdf.inputs["Emission Strength"].default_value = strength
        MATS[key] = mat
    return MATS[key]


def holdout():
    """挡住后面、自己渲成透明的料。"""
    if "holdout" not in MATS:
        mat = bpy.data.materials.new("holdout")
        mat.use_nodes = True
        nodes, links = mat.node_tree.nodes, mat.node_tree.links
        nodes.remove(nodes["Principled BSDF"])
        links.new(nodes.new("ShaderNodeHoldout").outputs[0], nodes["Material Output"].inputs["Surface"])
        MATS["holdout"] = mat
    return MATS["holdout"]


# 戴的东西分两遍渲，框和锚点一样，插件叠起来：base 那遍只出可换色的主料（中性灰，插件乘色），
# 点缀挡掉；trim 那遍只出固定色的点缀，主料挡掉。互相挡住的地方都渲成透明，叠起来正好拼回整件。
PASS = None


def wear(rough=0.8, grain=0.0, coat=0.0, sheen=0.0):
    """可换色的主料。"""
    if PASS == "trim":
        return holdout()
    key = ("wear", rough, grain, coat, sheen)
    if key not in MATS:
        MATS[key] = new_clay(NEUTRAL, rough, grain, coat, sheen)
    return MATS[key]


def trim(mat):
    """固定色的点缀。"""
    return holdout() if PASS == "base" else mat


def glass(tint=(0.85, 0.94, 1.0)):
    key = ("glass", tint)
    if key not in MATS:
        mat, bsdf = principled("glass")
        bsdf.inputs["Base Color"].default_value = (*srgb(tint), 1)
        bsdf.inputs["Transmission Weight"].default_value = 1.0
        bsdf.inputs["Roughness"].default_value = 0.05
        bsdf.inputs["IOR"].default_value = 1.3
        MATS[key] = mat
    return MATS[key]


# 身子、手、脚共用 SKIN，按中性灰 NEUTRAL 渲，插件乘上「身子色 / NEUTRAL」（线性）的系数，再做
# 色调映射（见 TONE）。这些零件不做映射、照线性亮度存（Standard），而且比别的零件暗 HEADROOM 档：
# 高光顶到白就截掉了，乘多少都回不来；打包时系数再乘回 2^HEADROOM。也别压太暗，8 位存不下暗处的
# 层次。
SKIN = "skin"
NEUTRAL = (0.6, 0.6, 0.6)
HEADROOM = 1.5

# 曝光（档）：受光面的亮度约等于底色，底色在画面上就是定的那个颜色，高光再往上交给色调映射压。
EXPOSURE = -0.3
# 色调映射用 Khronos PBR Neutral：底色在正常光照下照原样出，只把高光平滑地压向白色。不乘色的零件
# 由 Blender 直接按它出图；乘色的按线性出，插件乘完色用同一套公式压，两边一致。
TONE = "Khronos PBR Neutral"


def tinted(name):
    """插件要乘色的零件：身子、胳膊、戴的东西的主料。"""
    return name.startswith(("body/", "arm/")) or "/base/" in name


def body_material():
    """糯米团子的皮：光从皮下透出一点暖色（次表面散射），边缘有层绒光，表面光滑。"""
    if SKIN not in MATS:
        mat, bsdf = principled(SKIN)
        bsdf.inputs["Base Color"].default_value = (*srgb(NEUTRAL), 1)
        bsdf.inputs["Roughness"].default_value = 0.48
        bsdf.inputs["Subsurface Weight"].default_value = 0.25
        bsdf.inputs["Subsurface Radius"].default_value = (1.0, 0.45, 0.3)
        bsdf.inputs["Subsurface Scale"].default_value = 0.08
        bsdf.inputs["Sheen Weight"].default_value = 0.3
        bsdf.inputs["Sheen Roughness"].default_value = 0.4
        MATS[SKIN] = mat
    return MATS[SKIN]


# ---- 基本件：都挂在 parent 下，坐标是 parent 里的 ----

CHAR = None  # 这一帧小人的所有物体都进这个集合，下一帧整个清掉。


def link(obj, mat, parent, smooth=True):
    if mat is not None:
        obj.data.materials.append(mat)
    if smooth and hasattr(obj.data, "polygons"):
        for poly in obj.data.polygons:
            poly.use_smooth = True
    if parent is not None:
        obj.parent = parent
    for col in obj.users_collection:
        col.objects.unlink(obj)
    CHAR.objects.link(obj)
    return obj


def empty(loc=(0, 0, 0), parent=None, rot=(0, 0, 0)):
    obj = bpy.data.objects.new("pivot", None)
    obj.location = loc
    obj.rotation_euler = rot
    CHAR.objects.link(obj)
    if parent is not None:
        obj.parent = parent
    return obj


def trimmed(obj, keep):
    """只留 `keep(顶点坐标)` 为真的顶点（网格自己的坐标，缩放前）。"""
    bm = bmesh.new()
    bm.from_mesh(obj.data)
    bmesh.ops.delete(bm, geom=[v for v in bm.verts if not keep(v.co)], context="VERTS")
    bm.to_mesh(obj.data)
    bm.free()


def sphere(loc, scale, mat, parent, rot=(0, 0, 0), keep=None, fine=False):
    """椭球。`keep` 见 `trimmed`，切成壳（帽子）；`fine` 用密网格，大件的轮廓才圆。"""
    segments, rings = (2 * RINGS, RINGS) if fine or keep else (24, 12)
    bpy.ops.mesh.primitive_uv_sphere_add(segments=segments, ring_count=rings, location=loc, rotation=rot)
    obj = bpy.context.object
    obj.scale = scale
    if keep:
        trimmed(obj, keep)
    return link(obj, mat, parent)


def box(loc, size, mat, parent, bevel=0.0, rot=(0, 0, 0)):
    bpy.ops.mesh.primitive_cube_add(size=1, location=loc, rotation=rot)
    obj = bpy.context.object
    obj.scale = size
    bpy.ops.object.transform_apply(scale=True)
    if bevel:
        mod = obj.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 6
        mod.limit_method = "NONE"
    return link(obj, mat, parent)


def cylinder(loc, radius, depth, mat, parent, rot=(0, 0, 0), bevel=0.0, scale=(1, 1, 1)):
    bpy.ops.mesh.primitive_cylinder_add(vertices=40, radius=radius, depth=depth,
                                        location=loc, rotation=rot)
    obj = bpy.context.object
    obj.scale = scale
    if bevel:
        mod = obj.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 4
        mod.limit_method = "ANGLE"
    bpy.ops.object.shade_auto_smooth(angle=math.radians(40))
    return link(obj, mat, parent, smooth=False)


def cone(loc, r1, r2, depth, mat, parent, rot=(0, 0, 0)):
    bpy.ops.mesh.primitive_cone_add(vertices=32, radius1=r1, radius2=r2, depth=depth,
                                    location=loc, rotation=rot)
    obj = bpy.context.object
    bpy.ops.object.shade_auto_smooth(angle=math.radians(40))
    return link(obj, mat, parent, smooth=False)


def torus(loc, major, minor, mat, parent, rot=(0, 0, 0), scale=(1, 1, 1), keep=None):
    bpy.ops.mesh.primitive_torus_add(major_radius=major, minor_radius=minor,
                                     major_segments=64, minor_segments=12,
                                     location=loc, rotation=rot)
    obj = bpy.context.object
    obj.scale = scale
    if keep:
        trimmed(obj, keep)
    return link(obj, mat, parent)


def tube(points, radius, mat, parent):
    curve = bpy.data.curves.new("tube", "CURVE")
    curve.dimensions = "3D"
    curve.bevel_depth = radius
    curve.bevel_resolution = 4
    curve.use_fill_caps = True
    spline = curve.splines.new("POLY")
    spline.points.add(len(points) - 1)
    for point, co in zip(spline.points, points):
        point.co = (*co, 1)
    curve.materials.append(mat)
    obj = bpy.data.objects.new("tube", curve)
    CHAR.objects.link(obj)
    obj.parent = parent
    return obj


# ---- 身子 ----

# 糯米团子：压扁的蛋，上半圆、下半更扁，底边略鼓。中心高 CZ，半宽 A、半深 B，中心往上 UT、往下 DN。
A, B, UT, DN, CZ = 0.5, 0.45, 0.62, 0.32, 0.34
TOP = CZ + UT


def surface_y(x, z):
    """身子正面 (x, z) 处的 y（朝镜头是 -y）。"""
    h = UT if z >= CZ else DN
    return -B * math.sqrt(max(1 - (x / A) ** 2 - ((z - CZ) / h) ** 2, 0))


def tangent(x, z):
    """贴在身子正面 (x, z) 处的东西绕 z 转多少度才顺着表面。"""
    return math.degrees(math.atan2(x / A ** 2, -surface_y(x, z) / B ** 2))


def ring(z):
    """身子在高度 z 处一圈的半宽、半深。"""
    h = UT if z >= CZ else DN
    k = math.sqrt(max(0, 1 - ((z - CZ) / h) ** 2))
    return A * k, B * k


RINGS = 32  # 细网格球的环数


def rim(cut):
    """`shell` 切到 `cut` 时实际的下沿（同样按比例）：网格按环切，下沿是高过 `cut` 的最低一环。"""
    return min(z for z in (math.cos(k * math.pi / RINGS) for k in range(RINGS)) if z > cut)


def shell(grow, cut, mat, root, back=0.0):
    """贴着身子的一层壳（帽子），比身子大 `grow`，只留高过 `cut` 的（按身子上半的比例，0 是腰、
    1 是头顶），后脑勺那边再往下 `back`。"""
    return sphere((0, 0, CZ), (A + grow, B + grow, UT + grow), mat, root,
                  keep=lambda co: co.z > cut - back * max(0.0, co.y))


# ---- 镜头和投影 ----

YAW = -14
PPU = 72 / 1.5          # 成图每单位多少像素：画布 72px 框住 1.5 个单位，小人占满画布，道具、特效可以伸出去
TARGET = Vector((0, 0, 0.46))  # 整个小人的画布中心
CAM = Vector((0, -6, 2.2))
FORWARD = (TARGET - CAM).normalized()
RIGHT = Vector((1, 0, 0))
UP = RIGHT.cross(FORWARD).normalized()
MARGIN = 0.03


def placed(point):
    """角色坐标里的点放到世界里（转过 YAW）。"""
    return Matrix.Rotation(math.radians(YAW), 3, "Z") @ Vector(point)


def on_screen(offset):
    """世界里的一段位移在成图上是多少像素（x 朝右，y 朝下）。"""
    return (round(offset.dot(RIGHT) * PPU, 2), round(-offset.dot(UP) * PPU, 2))


def spun(pivot, angle):
    """绕过 `pivot`、朝着镜头的轴在画面里顺时针转 `angle` 度。返回的空物体下按世界坐标搭。"""
    spin = empty(tuple(pivot))
    spin.rotation_mode = "QUATERNION"
    spin.rotation_quaternion = Quaternion(-FORWARD, math.radians(-angle))
    return empty(tuple(-pivot), spin)


# 跟着身子转身的零件（身子、腮红、眼睛、嘴、戴的）每 360 / FACINGS 度渲一个朝向，插件转身时把
# 夹着的两个混起来。帽檐、眼镜腿这类转过去形状会变，靠挪补不出来，所以要渲得够密。0 是正面，
# 往画面右边转为正。
FACINGS = 16
FACING = 0  # 正在渲的朝向（度），character() 按它转


def facing(degrees, build):
    """按朝向 `degrees` 搭 `build`。"""
    def make(p):
        global FACING
        FACING = degrees
        try:
            build(p)
        finally:
            FACING = 0
    return make


def turned_to(point, degrees):
    """角色坐标里的点转到朝向 `degrees`、再放到世界里。"""
    return Matrix.Rotation(math.radians(YAW + degrees), 3, "Z") @ Vector(point)


def character(parent=None):
    """角色坐标的根：转过 YAW 和正在渲的朝向。"""
    return empty((0, 0, 0), parent, rot=rad(0, 0, YAW + FACING))


# ---- 身子和脸 ----

ARM_LEN = 0.2
SHOULDER_R = (0.45, -0.1, 0.42)
# 行首只有 36pt：眼睛往大里做、亮点要亮，小尺寸下才看得出是在看人。
EYE = (0.175, surface_y(0.175, 0.42), 0.42)
MOUTH = (0.0, surface_y(0.0, 0.33), 0.33)
CHEEK = (0.3, 0.33)


def body(root, skin=None):
    """身子和脚。`skin` 不给就用中性灰的身子料。"""
    skin = skin or body_material()
    obj = sphere((0, 0, CZ), (1, 1, 1), skin, root, fine=True)
    for vertex in obj.data.vertices:
        x, y, z = vertex.co
        bulge = 1 + 0.07 * max(0, -z)
        vertex.co = (x * A * bulge, y * B * bulge, z * (UT if z >= 0 else DN))
    for x in (-0.2, 0.2):
        sphere((x, -0.3, 0.05), (0.13, 0.13, 0.065), skin, root)


def blush(root):
    """腮红：身子用 holdout 搭上，挡住的部分渲成透明，只留露在脸上的。"""
    body(root, holdout())
    pink = clay(oklch(0.8, 0.09, 15))
    for side in (-1, 1):
        x, z = side * CHEEK[0], CHEEK[1]
        sphere((x, surface_y(x, z) + 0.012, z), (0.07, 0.016, 0.04), pink, root, rot=rad(0, 0, tangent(x, z)))


def eye(root, kind, side):
    """一只眼（`side` 是 -1 左、1 右），贴着脸的弧面。"""
    ink = clay(INK, rough=0.15, coat=1.0)
    x, z = side * EYE[0], EYE[2]
    holder = empty((x, surface_y(x, z), z), root, rot=rad(0, 0, tangent(x, z)))
    if kind == "open":
        sphere((0, 0.012, 0), (0.074, 0.03, 0.094), ink, holder)
        shine = glow((1, 1, 1), 3.0)
        sphere((0.025, -0.02, 0.038), (0.026, 0.008, 0.028), shine, holder)
        sphere((-0.027, -0.017, -0.04), (0.012, 0.006, 0.012), shine, holder)
    elif kind == "happy":
        points = [(t * 0.07, -0.006, -0.025 + 0.05 * (1 - t * t)) for t in (-1, -0.5, 0, 0.5, 1)]
        tube(points, 0.02, ink, holder)
    else:  # "shut"：眨眼
        tube([(-0.065, -0.006, -0.012), (0.065, -0.006, -0.012)], 0.018, ink, holder)


def mouth(root, kind):
    ink = clay(INK, rough=0.3)
    y, z = MOUTH[1], MOUTH[2]
    if kind == "smile":
        points = [(x * 0.032, y - 0.005, z + 0.008 - 0.024 * (1 - x * x)) for x in (-1, -0.5, 0, 0.5, 1)]
        tube(points, 0.009, ink, root)
    elif kind == "flat":
        tube([(-0.03, y - 0.005, z), (0.03, y - 0.005, z)], 0.009, ink, root)
    else:  # "open"
        sphere((0, y + 0.006, z - 0.006), (0.03, 0.012, 0.03), clay(oklch(0.42, 0.12, 20), rough=0.4), root)


def arm(parent):
    """右肩往画面正下方伸的一片小鳍。"""
    shoulder = placed(SHOULDER_R)
    holder = empty(tuple(shoulder - UP * ARM_LEN / 2), parent)
    holder.rotation_mode = "QUATERNION"
    holder.rotation_quaternion = Matrix((RIGHT, FORWARD, UP)).transposed().to_quaternion()
    sphere((0, 0, 0), (0.075, 0.06, ARM_LEN / 2 + 0.05), body_material(), holder)


# ---- 戴的：头顶、脸上、脖子各一件，按种子各自落。可换色的用 wear()，固定色的套 trim() ----
# 都贴着身子搭：帽子是比身子大一圈的壳，脸上的顺着脸的弧面转。

CREAM = oklch(0.96, 0.02, 90)
DARK = oklch(0.32, 0.02, 250)


def beanie(root):
    knit = wear(rough=0.95, grain=0.4, sheen=0.6)
    shell(0.025, 0.45, knit, root)
    z = CZ + rim(0.45) * (UT + 0.025)
    a, b = ring(z)
    torus((0, 0, z), 1.0, 0.05, knit, root, scale=(a + 0.03, b + 0.03, 1))
    sphere((0, 0, TOP + 0.07), (0.08, 0.08, 0.08), trim(clay(CREAM, rough=1.0, sheen=1.0)), root)


def cap(root):
    cloth = wear(rough=0.6)
    shell(0.02, 0.5, cloth, root)
    z = CZ + rim(0.5) * (UT + 0.02)
    brim = sphere((0, -ring(z)[1] - 0.1, z), (0.24, 0.17, 0.025), cloth, root, fine=True)
    brim.rotation_euler = rad(-10, 0, 0)
    sphere((0, 0, TOP + 0.02), (0.035, 0.035, 0.025), cloth, root)


def fedora(root):
    felt = wear(rough=0.9, sheen=0.3)
    z = CZ + 0.8 * UT  # 帽檐卡在头顶这一圈
    cylinder((0, 0, z), 0.5, 0.03, felt, root, bevel=0.012, scale=(1, 0.9, 1))
    cylinder((0, 0, z + 0.12), 0.31, 0.24, felt, root, bevel=0.06, scale=(1, 0.9, 1))
    cylinder((0, 0, z + 0.05), 0.315, 0.06, trim(clay(INK)), root, scale=(1, 0.9, 1))


def beret(root):
    wool = wear(rough=0.9, sheen=0.5)
    tilt = empty((0.05, 0.02, TOP - 0.05), root, rot=rad(0, 16, 0))
    sphere((0, 0, 0), (0.4, 0.36, 0.11), wool, tilt, fine=True)
    cylinder((0, 0, 0.12), 0.022, 0.05, wool, tilt)


def goggles(root, z=0.66, lift=0.0, base=True):
    """护目镜推在额头上。`base` 是镜框可不可换色（头盔上那副跟着头盔，固定色）。"""
    strap = trim(clay(DARK, rough=0.6))
    rim = wear(rough=0.25, coat=0.8) if base else trim(clay(oklch(0.75, 0.04, 80), rough=0.25, coat=0.8))
    lens = trim(clay(oklch(0.62, 0.1, 220), rough=0.05, coat=1.0))
    a, b = ring(z)
    torus((0, 0, z), 1.0, 0.026, strap, root, scale=(a + 0.02 + lift, b + 0.02 + lift, 1))
    for side in (-1, 1):
        x = side * 0.15
        y = surface_y(x, z) - 0.03 - lift
        turn = tangent(x, z)
        torus((x, y, z), 0.085, 0.026, rim, root, rot=rad(90, 0, turn))
        sphere((x, y + 0.004, z), (0.075, 0.02, 0.075), lens, root, rot=rad(0, 0, turn))


def straw_hat(root):
    straw = trim(clay(oklch(0.86, 0.08, 85), rough=0.85, grain=0.5))
    cylinder((0, 0, TOP - 0.06), 0.58, 0.025, straw, root, bevel=0.01)
    sphere((0, 0, TOP - 0.07), (0.33, 0.3, 0.16), straw, root, keep=lambda co: co.z > 0)
    cylinder((0, 0, TOP - 0.03), 0.325, 0.045, wear(rough=0.5), root)


def bandana(root):
    cloth = wear(rough=0.85)
    shell(0.018, 0.4, cloth, root, back=0.3)
    dots = trim(clay(CREAM))
    for x, z in ((-0.2, 0.7), (0.05, 0.75), (0.27, 0.68)):
        sphere((x, surface_y(x, z) - 0.015, z), (0.03, 0.012, 0.03), dots, root, rot=rad(0, 0, tangent(x, z)))
    for dz, rot in ((0.0, 30), (-0.1, -20)):
        sphere((0.36, 0.3, 0.68 + dz), (0.09, 0.05, 0.05), cloth, root).rotation_euler = rad(0, rot, 0)


def helmet(root):
    """赛车头盔：亮面壳，正中一道白条，护目镜推在额头上，露着脸。"""
    cut = lambda co: co.z > 0.42 - 0.3 * max(0.0, co.y)
    sphere((0, 0, CZ), (A + 0.04, B + 0.04, UT + 0.04), wear(rough=0.18, coat=1.0), root, keep=cut)
    sphere((0, 0, CZ), (0.07, B + 0.048, UT + 0.048), trim(clay(CREAM, rough=0.2, coat=1.0)), root, keep=cut)
    goggles(root, z=0.62, lift=0.045, base=False)


def slick_hair(root):
    """头顶一片往前翘的黑头发。"""
    hair = trim(clay(INK, rough=0.45, coat=0.5))
    shell(0.012, 0.72, hair, root)
    sphere((0.06, surface_y(0.06, 0.85) - 0.02, 0.88), (0.2, 0.1, 0.07), hair, root).rotation_euler = rad(-20, -18, 0)


def sprig(root):
    """头顶冒出一棵两片叶子的小芽。"""
    green = trim(clay(oklch(0.7, 0.14, 140), rough=0.5))
    tube([(0, 0, TOP - 0.02), (0.01, 0, TOP + 0.09)], 0.012, green, root)
    for side in (-1, 1):
        sphere((side * 0.07, 0, TOP + 0.11), (0.08, 0.03, 0.04), green, root, rot=rad(0, side * -25, 0))


def flower(root):
    """别在头顶一侧的一朵花。"""
    x, z = 0.3, 0.74
    holder = empty((x, surface_y(x, z) - 0.02, z), root, rot=rad(-20, 0, tangent(x, z)))
    petal = wear(rough=0.6)
    for k in range(5):
        a = math.radians(90 + k * 72)
        sphere((0.08 * math.cos(a), 0, 0.08 * math.sin(a)), (0.065, 0.035, 0.065), petal, holder)
    sphere((0, -0.03, 0), (0.05, 0.035, 0.05), trim(clay(oklch(0.86, 0.15, 90), rough=0.5)), holder)


def headphones(root):
    band = trim(clay(DARK, rough=0.4))
    torus((0, 0, CZ + 0.02), 0.6, 0.028, band, root, rot=rad(90, 0, 0), keep=lambda co: co.y > -0.15)
    cup = wear(rough=0.35, coat=0.4)
    for side in (-1, 1):
        cylinder((side * 0.56, 0, 0.36), 0.12, 0.1, cup, root, rot=rad(0, 90, 0), bevel=0.03)


def antenna(root):
    tube([(0.02, 0.02, TOP - 0.02), (0.03, 0.02, TOP + 0.12), (0.08, 0.01, TOP + 0.2)], 0.012,
         trim(clay(DARK, rough=0.6)), root)
    sphere((0.08, 0.01, TOP + 0.22), (0.045, 0.045, 0.045), wear(rough=0.3, coat=0.6), root)


def chef_hat(root):
    cloth = trim(clay(oklch(0.97, 0.01, 90), rough=0.9, sheen=0.5))
    cylinder((0, 0, TOP - 0.02), 0.26, 0.14, cloth, root)
    for x, y in ((-0.13, 0), (0.13, 0), (0, -0.08), (0, 0.08), (0, 0)):
        sphere((x, y, TOP + 0.12), (0.17, 0.17, 0.13), cloth, root)


def bow(root):
    """头顶一侧的蝴蝶结。"""
    ribbon = wear(rough=0.45, coat=0.3)
    x, z = 0.3, 0.7
    y = surface_y(x, z)
    holder = empty((x, y, z), root, rot=rad(0, 0, tangent(x, z)))
    for side in (-1, 1):
        sphere((side * 0.09, 0.0, 0.02), (0.1, 0.04, 0.075), ribbon, holder, rot=rad(0, side * 20, 0))
    sphere((0, -0.02, 0.02), (0.04, 0.035, 0.04), ribbon, holder)


def glasses(root, lens):
    """一副眼镜，框可换色。`lens` 给镜片料就是有色镜片（墨镜），不给是透明的只看见框。"""
    frame = wear(rough=0.3, coat=0.5)
    z, r = EYE[2] - 0.01, 0.1
    for side in (-1, 1):
        x = side * EYE[0]
        y = surface_y(x, z) - 0.04
        turn = tangent(x, z)
        torus((x, y, z), r, 0.016, frame, root, rot=rad(90, 0, turn))
        if lens:
            sphere((x, y + 0.004, z), (r, 0.012, r), trim(lens), root, rot=rad(0, 0, turn))
        edge = side * (EYE[0] + r)
        tube([(edge, y + 0.01, z), (side * (ring(z)[0] + 0.01), 0.0, z + 0.02)], 0.014, frame, root)
    inner = EYE[0] - r
    tube([(-inner, surface_y(inner, z) - 0.045, z + 0.01), (inner, surface_y(inner, z) - 0.045, z + 0.01)],
         0.014, frame, root)


def round_glasses(root):
    glasses(root, None)


def sunglasses(root):
    glasses(root, clay(oklch(0.2, 0.01, 250), rough=0.1, coat=1.0))


NECK = 0.2  # 脖子上的戴在嘴下面这一圈


def bow_tie(root):
    silk = wear(rough=0.4, coat=0.3)
    y = surface_y(0, NECK)
    for side in (-1, 1):
        cone((side * 0.075, y - 0.01, NECK), 0.065, 0.016, 0.13, silk, root, rot=rad(0, side * -90, 0))
    sphere((0, y - 0.03, NECK), (0.03, 0.026, 0.03), silk, root)


def scarf(root):
    knit = wear(rough=0.9, grain=0.4, sheen=0.6)
    a, b = ring(NECK)
    torus((0, 0, NECK), 1.0, 0.055, knit, root, scale=(a + 0.035, b + 0.035, 1))
    x = 0.2
    tail = empty((x, surface_y(x, NECK) - 0.06, NECK - 0.03), root, rot=rad(0, -8, tangent(x, NECK)))
    box((0, 0, -0.07), (0.1, 0.035, 0.16), knit, tail, bevel=0.02)


def masked(build):
    """戴的东西：身子用 holdout 搭上，绕到身子后面的部分渲成透明。框和锚点跟身子一样，跟着身子
    压扁。"""
    def make(p):
        root = character(p)
        body(root, holdout())
        build(root)
    return make


# ---- 拿的：以手为原点、朝 +z 伸 ----

def laptop(hand, lines=4):
    """屏幕朝外端在肚子前：小尺寸下一块亮屏最好认，缩小一圈、放低，不挡嘴。`lines` 是屏上
    亮着几行代码。"""
    shell = clay((0.36, 0.38, 0.44), rough=0.4)
    base = empty((-0.3, -0.08, 0.02), hand, rot=rad(8, 0, 0))
    base.scale = (0.72, 0.72, 0.72)
    box((0, 0, 0), (0.5, 0.3, 0.035), shell, base, bevel=0.014)
    lid = empty((0, 0.15, 0.015), base, rot=rad(-12, 0, 0))
    box((0, 0, 0.16), (0.5, 0.025, 0.32), shell, lid, bevel=0.014)
    box((0, -0.014, 0.16), (0.44, 0.004, 0.26), glow((0.16, 0.2, 0.3), 1.2), lid)
    code = [(0.5, 0.85, 1.0), (1.0, 0.75, 0.35), (0.6, 0.95, 0.6), (0.5, 0.85, 1.0)]
    for i, (indent, width) in enumerate([(0, 0.22), (0.05, 0.16), (0.05, 0.24), (0, 0.12)][:lines]):
        box((-0.17 + indent + width / 2, -0.018, 0.25 - i * 0.06), (width, 0.004, 0.025),
            glow(code[i], 5.0), lid)


def wrench(hand):
    """返回扳手头，火星从这冒。"""
    steel = clay((0.52, 0.56, 0.63), rough=0.28)
    box((0, 0, 0.2), (0.085, 0.05, 0.48), steel, hand, bevel=0.022)
    head = cylinder((0, 0, 0.46), 0.14, 0.06, steel, hand, rot=rad(90, 0, 0), bevel=0.015)
    notch = box((0, 0, 0.57), (0.1, 0.3, 0.2), steel, hand)
    cut = head.modifiers.new("notch", "BOOLEAN")
    cut.object = notch
    cut.operation = "DIFFERENCE"
    notch.hide_render = True
    box((0, 0, 0.02), (0.11, 0.07, 0.18), clay((0.9, 0.32, 0.26)), hand, bevel=0.03)
    return empty((0, 0, 0.5), hand)


def magnifier(hand):
    brass = clay((0.86, 0.64, 0.3), rough=0.3)
    cylinder((0, 0, 0.1), 0.035, 0.24, clay((0.45, 0.28, 0.16)), hand, bevel=0.012)
    torus((0, 0, 0.4), 0.17, 0.032, brass, hand, rot=rad(90, 0, 0))
    cylinder((0, 0, 0.4), 0.16, 0.014, glass(), hand, rot=rad(90, 0, 0))


def pencil(hand):
    wood = clay((0.98, 0.78, 0.25), rough=0.6)
    cylinder((0, 0, 0.14), 0.04, 0.36, wood, hand, bevel=0.005)
    cone((0, 0, 0.36), 0.04, 0.0, 0.09, clay((0.93, 0.8, 0.62)), hand)
    cone((0, 0, 0.392), 0.0145, 0.0, 0.026, clay(INK), hand)
    cylinder((0, 0, -0.06), 0.042, 0.05, clay((0.75, 0.76, 0.8), rough=0.3), hand)
    cylinder((0, 0, -0.1), 0.04, 0.05, clay((0.95, 0.55, 0.6)), hand, bevel=0.012)


def notepad(hand, lines=0):
    paper = clay((0.97, 0.95, 0.88), rough=0.8)
    pad = empty(NOTEPAD_FROM_HAND, hand)
    pad.scale = (0.7, 0.7, 0.7)
    box((0, 0, 0), (0.34, 0.03, 0.42), paper, pad, bevel=0.01)
    box((0, 0.006, 0.2), (0.36, 0.035, 0.05), clay((0.35, 0.45, 0.7)), pad, bevel=0.01)
    for i in range(lines):
        width = 0.22 if i % 2 == 0 else 0.16
        box((-0.11 + width / 2, -0.017, 0.12 - i * 0.07), (width, 0.004, 0.018),
            clay((0.35, 0.4, 0.55)), pad)


def flask(hand):
    """返回瓶口，泡泡从这冒。"""
    holder = empty((0, -0.04, 0.18), hand)
    sphere((0, 0, 0), (0.16, 0.16, 0.16), glass((0.9, 1.0, 0.97)), holder)
    sphere((0, 0, -0.02), (0.135, 0.135, 0.12), glow((0.35, 0.9, 0.6), 1.5), holder)
    cylinder((0, 0, 0.18), 0.05, 0.16, glass((0.9, 1.0, 0.97)), holder)
    torus((0, 0, 0.26), 0.05, 0.012, glass((0.9, 1.0, 0.97)), holder)
    return empty((0, 0, 0.28), holder)


def watering_can(hand):
    """提手在手里，壶身吊在下面，嘴朝右。返回壶嘴尖，水从这洒。"""
    tin = clay((0.3, 0.62, 0.6), rough=0.35)
    can = empty((0, -0.02, -0.2), hand)
    cylinder((0, 0, 0), 0.15, 0.22, tin, can, bevel=0.02, scale=(1, 0.85, 1))
    torus((0, 0, 0.14), 0.11, 0.022, tin, can, rot=rad(90, 0, 0))
    spout_base = (0.12, 0, -0.04)
    tip = (0.26, 0, 0.1)
    tube([spout_base, tip], 0.025, tin, can)
    cone((tip[0] + 0.02, 0, tip[2] + 0.015), 0.025, 0.055, 0.05, tin, can, rot=rad(0, 54, 0))
    return empty((tip[0] + 0.04, 0, tip[2] + 0.03), can)


def baton(hand):
    cylinder((0, 0, 0.0), 0.03, 0.12, clay(INK, rough=0.5), hand, bevel=0.01)
    cylinder((0, 0, 0.26), 0.018, 0.42, clay((0.97, 0.97, 0.95), rough=0.4), hand)


def broom(hand):
    """返回扫帚头的位置，扬灰从这起。"""
    cylinder((0, 0, 0.18), 0.03, 0.56, clay((0.6, 0.42, 0.25)), hand)
    cylinder((0, 0, 0.47), 0.06, 0.04, clay((0.7, 0.25, 0.25)), hand)
    cone((0, 0, 0.58), 0.065, 0.16, 0.2, clay((0.9, 0.76, 0.42), rough=0.95, grain=0.8), hand)
    return empty((0, 0, 0.68), hand)


# ---- 特效：都搭在一个点上，锚点是这个点 ----

FX_AT = (0.5, -0.4, 0.8)


def fx_spark(root):
    sphere(FX_AT, (0.03, 0.03, 0.03), glow((1.0, 0.72, 0.15), 6.0), root)


def fx_bubble(root):
    sphere(FX_AT, (0.06, 0.06, 0.06), glow((0.45, 0.95, 0.7), 2.5), root)


def fx_drop(root):
    sphere(FX_AT, (0.035, 0.035, 0.05), glow((0.4, 0.7, 1.0), 1.5), root)


def fx_note(root):
    ink = clay((0.95, 0.55, 0.2), rough=0.5)
    note = empty(FX_AT, root)
    sphere((0, 0, 0), (0.055, 0.03, 0.042), ink, note).rotation_euler = rad(0, -25, 0)
    box((0.045, 0, 0.1), (0.018, 0.018, 0.2), ink, note)
    box((0.08, 0, 0.18), (0.07, 0.018, 0.03), ink, note, rot=rad(0, 30, 0))


def fx_puff(root):
    sphere(FX_AT, (0.08, 0.08, 0.07), clay((0.78, 0.75, 0.7), rough=1.0), root)


def fx_dot(root):
    sphere(FX_AT, (0.045, 0.045, 0.045), clay((0.62, 0.64, 0.7)), root)


def fx_dot_lit(root):
    sphere(FX_AT, (0.045, 0.045, 0.045), glow((0.4, 0.75, 1.0), 4.0), root)


def fx_bang(root):
    red = clay((0.95, 0.3, 0.25), rough=0.4)
    holder = empty(FX_AT, root)
    box((0, 0, 0.06), (0.07, 0.07, 0.2), red, holder, bevel=0.03)
    sphere((0, 0, -0.1), (0.045, 0.045, 0.045), red, holder)


def fx_bulb(root, lit):
    holder = empty(FX_AT, root)
    shell = glow((1.0, 0.86, 0.35), 4.0) if lit else glass((0.95, 0.95, 0.9))
    sphere((0, 0, 0.04), (0.09, 0.09, 0.1), shell, holder)
    cylinder((0, 0, -0.07), 0.045, 0.06, clay((0.6, 0.62, 0.66), rough=0.3), holder)


def fx_star(root):
    verts = []
    for k in range(10):
        r = 0.1 if k % 2 == 0 else 0.045
        a = math.radians(90 + k * 36)
        verts.append((r * math.cos(a), 0, r * math.sin(a)))
    mesh = bpy.data.meshes.new("star")
    mesh.from_pydata(verts, [], [list(range(10))])
    obj = bpy.data.objects.new("star", mesh)
    obj.location = FX_AT
    solid = obj.modifiers.new("solid", "SOLIDIFY")
    solid.thickness = 0.04
    solid.offset = 0
    bevel = obj.modifiers.new("bevel", "BEVEL")
    bevel.width = 0.012
    bevel.segments = 3
    bpy.context.scene.collection.objects.link(obj)
    link(obj, glow((1.0, 0.85, 0.3), 3.0), root, smooth=False)


def fx_heart(root):
    red = clay((0.95, 0.36, 0.45), rough=0.4)
    holder = empty(FX_AT, root)
    for side in (-1, 1):
        sphere((side * 0.04, 0, 0.03), (0.055, 0.04, 0.055), red, holder)
    cone((0, 0, -0.03), 0.075, 0.0, 0.1, red, holder, rot=rad(180, 0, 0)).scale = (1, 0.55, 1)


CONFETTI = [(0.95, 0.4, 0.4), (0.45, 0.7, 1.0), (1.0, 0.82, 0.3), (0.55, 0.85, 0.5)]


def fx_confetti(root, k):
    box(FX_AT, (0.07, 0.015, 0.04), clay(CONFETTI[k], rough=0.5), root,
        rot=rad(20 * k, 35 + 25 * k, 10 * k))


SPROUT_AT = (0.7, -0.3, 0.0)


def sprout(root, stage):
    """地上的苗，长到第几截（0 到 4）。锚点在根上。"""
    h = 0.06 + 0.06 * stage
    holder = empty(SPROUT_AT, root)
    cylinder((0, 0, h / 2), 0.02, h, clay((0.3, 0.55, 0.28)), holder)
    leaf = clay((0.4, 0.72, 0.36), rough=0.6)
    size = 0.5 + 0.12 * stage
    for side in (-1, 1):
        sphere((side * 0.07 * size, 0, h), (0.08 * size, 0.03, 0.04 * size), leaf, holder).rotation_euler = \
            rad(0, side * -25, 0)
    if stage == 4:
        sphere((0, -0.01, h + 0.05), (0.05, 0.04, 0.05), clay((0.98, 0.6, 0.7)), holder)


# ---- 赛车手的卡丁车：车尾垫在身子后面，车头和方向盘挡在身子前面，分开渲 ----

KART_RED = (0.86, 0.2, 0.22)
TYRE = (0.16, 0.16, 0.18)
WHEEL_AT = (0.06, -0.54, 0.2)       # 方向盘中心（角色坐标）：两手中间、嘴下面
REAR_TYRE = (0.56, 0.28, 0.02)      # 右后轮着地的地方，冒烟从这起
KART_LEANS = range(-30, 31, 10)     # 车身左右倾的角度，过弯时用


def kart_front(root):
    paint_ = clay(KART_RED, rough=0.25, coat=1.0)
    tyre = clay(TYRE, rough=0.85, grain=0.3)
    box((0, -0.5, 0.13), (0.78, 0.42, 0.16), paint_, root, bevel=0.07)
    box((0, -0.74, 0.1), (1.0, 0.12, 0.07), paint_, root, bevel=0.03)
    cylinder((0, -0.62, 0.215), 0.07, 0.02, clay((0.97, 0.97, 0.95), rough=0.4), root)
    for x in (-0.5, 0.5):
        cylinder((x, -0.52, 0.13), 0.13, 0.12, tyre, root, rot=rad(0, 90, 0), bevel=0.03)
        cylinder((x * 1.13, -0.52, 0.13), 0.06, 0.02, clay((0.8, 0.82, 0.86), rough=0.3),
                 root, rot=rad(0, 90, 0))
    tube([(0.03, -0.5, 0.2), (WHEEL_AT[0], WHEEL_AT[1] + 0.02, WHEEL_AT[2] - 0.02)], 0.022, tyre, root)


def kart_back(root):
    paint_ = clay(KART_RED, rough=0.25, coat=1.0)
    tyre = clay(TYRE, rough=0.85, grain=0.3)
    box((0, 0.18, 0.14), (0.9, 0.5, 0.16), paint_, root, bevel=0.06)
    for x in (-REAR_TYRE[0], REAR_TYRE[0]):
        cylinder((x, REAR_TYRE[1], 0.16), 0.16, 0.16, tyre, root, rot=rad(0, 90, 0), bevel=0.03)
    for x in (-0.32, 0.32):
        box((x, 0.44, 0.4), (0.04, 0.04, 0.3), clay(TYRE, rough=0.6), root)
    box((0, 0.46, 0.56), (1.24, 0.16, 0.05), paint_, root, bevel=0.02)


def steering_wheel(root):
    """方向盘：朝着人，顶上一块红，转起来看得出来。"""
    grip = clay(TYRE, rough=0.6)
    holder = empty(WHEEL_AT, root, rot=rad(70, 0, 0))
    torus((0, 0, 0), 0.12, 0.026, grip, holder)
    box((0, 0, 0), (0.22, 0.03, 0.026), grip, holder)
    sphere((0, 0, 0.01), (0.038, 0.038, 0.026), clay(KART_RED, rough=0.4), holder)
    box((0, 0.12, 0), (0.06, 0.045, 0.06), clay(KART_RED, rough=0.4), holder)


def fx_speed(root):
    sphere(FX_AT, (0.018, 0.012, 0.16), clay((0.72, 0.77, 0.86), rough=0.5), root)


# ---- 零件清单 ----

# 戴的清单：名字 → (搭法, 有没有可换色的主料, 有没有固定色的点缀, 占不占眼睛)。顺序就是插件里的
# 编号。占眼睛的（护目镜、眼镜）一个小人只戴一件。
HEADS = {
    "beanie": (beanie, True, True, False), "cap": (cap, True, False, False),
    "fedora": (fedora, True, True, False), "beret": (beret, True, False, False),
    "goggles": (goggles, True, True, True), "straw": (straw_hat, True, True, False),
    "bandana": (bandana, True, True, False), "helmet": (helmet, True, True, False),
    "slick": (slick_hair, False, True, False), "sprig": (sprig, False, True, False),
    "flower": (flower, True, True, False), "headphones": (headphones, True, True, False),
    "antenna": (antenna, True, True, False), "chef": (chef_hat, False, True, False),
    "bow": (bow, True, False, False),
}
FACES = {
    "glasses": (round_glasses, True, False, True), "sunglasses": (sunglasses, True, True, True),
}
NECKS = {
    "bow-tie": (bow_tie, True, False, False), "scarf": (scarf, True, False, False),
}
PROPS = {
    "wrench": (wrench, (0, 0, 0.5)),
    "magnifier": (magnifier, (0, 0, 0.4)),
    "pencil": (pencil, (0, 0, 0.405)),
    "flask": (flask, (0, -0.04, 0.46)),
    "can": (watering_can, (0.3, -0.02, -0.07)),
    "baton": (baton, (0, 0, 0.47)),
    "broom": (broom, (0, 0, 0.68)),
}
GRIP = (0.58, -0.24, 0.4)       # 搭道具时手在哪（角色坐标），只影响光照
LAPTOP_AT = (0.0, -0.66, 0.06)  # 电脑底座中心（角色坐标）
LAPTOP_HAND = (0.3, -0.58, 0.04)
NOTEPAD_FROM_HAND = (0.06, -0.08, 0.04)
LEFT_HAND = (-0.4, -0.4, 0.24)
EFFECTS = {
    "spark": fx_spark, "bubble": fx_bubble, "drop": fx_drop, "note": fx_note, "puff": fx_puff,
    "dot": fx_dot, "dot-lit": fx_dot_lit, "bang": fx_bang, "star": fx_star, "heart": fx_heart,
    "bulb": lambda root: fx_bulb(root, False), "bulb-lit": lambda root: fx_bulb(root, True),
    "speed": fx_speed,
}
KEYS = 8  # 会转的零件渲几个关键朝向，均分一圈
STEP = 360 // KEYS


def jobs():
    """（名字, 锚点（世界）, 转角, 搭法, 按半径取方框?）。按名字分组连着出。"""
    feet = Vector((0, 0, 0))
    turns = [(f"{k:02}", k * 360 / FACINGS) for k in range(FACINGS)]
    for k, degrees in turns:
        yield f"body/{k}", feet, 0, facing(degrees, lambda p: body(character(p))), False
    for k, degrees in turns:
        yield f"blush/{k}", feet, 0, facing(degrees, lambda p: blush(character(p))), False
    for k in range(KEYS):
        yield f"arm/{k:02}", placed(SHOULDER_R), k * STEP - 180, arm, True
    for name, (build, _) in PROPS.items():
        grip = placed(GRIP)
        for k in range(KEYS):
            def make(p, build=build):
                hand = empty(GRIP, character(p))
                build(hand)
            yield f"prop/{name}/{k:02}", grip, k * STEP, make, True
    # 眼睛、嘴、戴的都拿身子当遮挡，框和锚点跟身子一样，转过去被身子挡住的渲成透明。
    for kind in ("open", "happy", "shut"):
        for side, name in ((-1, "l"), (1, "r")):
            for k, degrees in turns:
                build = masked(lambda root, kind=kind, side=side: eye(root, kind, side))
                yield f"eye/{kind}-{name}/{k}", feet, 0, facing(degrees, build), False
    for kind in ("smile", "flat", "open"):
        for k, degrees in turns:
            build = masked(lambda root, kind=kind: mouth(root, kind))
            yield f"mouth/{kind}/{k}", feet, 0, facing(degrees, build), False
    # 戴的：可换色的主料、固定色的点缀各一组（名字里带 /base/、/trim/，main 按它定这一遍渲哪部分）。
    for kind, items in (("head", HEADS), ("face", FACES), ("neck", NECKS)):
        for name, (build, base, top, _) in items.items():
            for part in [p for p, has in (("base", base), ("trim", top)) if has]:
                for k, degrees in turns:
                    yield f"{kind}/{name}/{part}/{k}", feet, 0, facing(degrees, masked(build)), False
    for lines in range(5):
        def make_laptop(p, lines=lines):
            laptop(empty(LAPTOP_HAND, character(p)), lines)
        yield f"laptop/{lines}", placed(LAPTOP_AT), 0, make_laptop, False
    for lines in range(5):
        def make_pad(p, lines=lines):
            notepad(empty(LEFT_HAND, character(p)), lines)
        pad = placed(tuple(a + b for a, b in zip(LEFT_HAND, NOTEPAD_FROM_HAND)))
        yield f"notepad/{lines}", pad, 0, make_pad, False
    for name, build in EFFECTS.items():
        yield f"fx/{name}", placed(FX_AT), 0, lambda p, b=build: b(character(p)), False
    for k in range(len(CONFETTI)):
        yield f"confetti/{k}", placed(FX_AT), 0, lambda p, k=k: fx_confetti(character(p), k), False
    for stage in range(5):
        yield f"sprout/{stage}", placed(SPROUT_AT), 0, lambda p, s=stage: sprout(character(p), s), False
    # 赛车手专用的放最后，别的角色不用解。车绕脚底左右倾。
    for k, lean in enumerate(KART_LEANS):
        yield f"kart-front/{k:02}", feet, lean, lambda p: kart_front(character(p)), True
    for k, lean in enumerate(KART_LEANS):
        yield f"kart-back/{k:02}", feet, lean, lambda p: kart_back(character(p)), True
    for k in range(KEYS):
        yield f"wheel/{k:02}", placed(WHEEL_AT), k * STEP, lambda p: steering_wheel(character(p)), True


def rig():
    """拼装用的位置，成图像素。身上的相对脚底；道具尖端相对握点、朝上（0°）时。"""
    feet = Vector((0, 0, 0))
    canvas = on_screen(feet - TARGET)
    shoulder_l = (-SHOULDER_R[0], SHOULDER_R[1], SHOULDER_R[2])

    def shoulder(point, degrees):
        """肩在画面上的位置，和离镜头比同高的身子中轴远多少（正的在身子后面，胳膊垫在身子底下）。"""
        world = turned_to(point, degrees)
        x, y = on_screen(world)
        return [x, y, round(Vector((world.x, world.y, 0)).dot(FORWARD) * PPU, 2)]

    out = {
        "canvas": 72,
        "feet": [round(36 + canvas[0], 2), round(36 + canvas[1], 2)],
        "shoulders": [[shoulder(shoulder_l, k * 360 / FACINGS), shoulder(SHOULDER_R, k * 360 / FACINGS)]
                      for k in range(FACINGS)],
        "arm_len": round(ARM_LEN * PPU, 2),
        "overhead": on_screen(placed((0, 0, TOP + 0.3))),
        "laptop_at": on_screen(placed(LAPTOP_AT)),
        "notepad_at": on_screen(placed(NOTEPAD_FROM_HAND)),
        "sprout_at": on_screen(placed(SPROUT_AT)),
        "wheel_at": on_screen(placed(WHEEL_AT)),
        "tyre_l": on_screen(placed((-REAR_TYRE[0], REAR_TYRE[1], REAR_TYRE[2]))),
        "tyre_r": on_screen(placed(REAR_TYRE)),
    }
    for name, (_, tip) in PROPS.items():
        out[f"tip_{name}"] = on_screen(placed(tip))
    return out


# ---- 舞台 ----

def stage():
    global CHAR
    bpy.ops.wm.read_factory_settings(use_empty=True)
    MATS.clear()
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.samples = SAMPLES
    scene.cycles.use_denoising = True
    scene.render.film_transparent = True
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA"
    scene.view_settings.view_transform = TONE
    scene.view_settings.exposure = EXPOSURE

    world = bpy.data.worlds.new("world")
    world.use_nodes = True
    background = world.node_tree.nodes["Background"]
    background.inputs[0].default_value = (0.92, 0.94, 1.0, 1)
    background.inputs[1].default_value = 0.7
    scene.world = world

    target = bpy.data.objects.new("target", None)
    target.location = TARGET
    scene.collection.objects.link(target)

    cam_data = bpy.data.cameras.new("cam")
    cam_data.type = "ORTHO"
    cam = bpy.data.objects.new("cam", cam_data)
    scene.collection.objects.link(cam)
    basis = Matrix((RIGHT, UP, -FORWARD)).transposed()
    cam.matrix_world = Matrix.Translation(CAM) @ basis.to_4x4()
    scene.camera = cam

    for name, loc, energy, size, color in (
        ("key", (-2.2, -3, 4.5), 650, 4, (1.0, 0.95, 0.88)),
        ("fill", (3, -2.5, 1.2), 160, 4, (0.85, 0.9, 1.0)),
        ("rim", (1.2, 3.5, 3), 380, 2.5, (1, 1, 1)),
    ):
        data = bpy.data.lights.new(name, "AREA")
        data.energy, data.size, data.color = energy, size, color
        light = bpy.data.objects.new(name, data)
        light.location = loc
        scene.collection.objects.link(light)
        track = light.constraints.new("TRACK_TO")
        track.target = target
        track.track_axis = "TRACK_NEGATIVE_Z"
        track.up_axis = "UP_Y"

    CHAR = bpy.data.collections.new("character")
    scene.collection.children.link(CHAR)
    return scene, cam


def clear():
    for obj in list(CHAR.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    for blocks in (bpy.data.meshes, bpy.data.curves):
        for block in list(blocks):
            if block.users == 0:
                blocks.remove(block)


def extent(pivot):
    """搭好的东西在画面上相对锚点占多大：(左, 右, 下, 上, 最远多远)，单位。"""
    bpy.context.view_layer.update()
    graph = bpy.context.evaluated_depsgraph_get()
    xs, ys = [], []
    for obj in CHAR.objects:
        if obj.type not in ("MESH", "CURVE") or obj.hide_render:
            continue
        evaluated = obj.evaluated_get(graph)
        mesh = evaluated.to_mesh()
        for vertex in mesh.vertices:
            offset = evaluated.matrix_world @ vertex.co - pivot
            xs.append(offset.dot(RIGHT))
            ys.append(offset.dot(UP))
        evaluated.to_mesh_clear()
    reach = max(math.hypot(x, y) for x, y in zip(xs, ys))
    return min(xs) - MARGIN, max(xs) + MARGIN, min(ys) - MARGIN, max(ys) + MARGIN, reach + MARGIN


def frame(scene, cam, pivot, left, top, width, height):
    """镜头对准以 (left, top) 为左上角、成图 width × height 像素的一块，渲两倍大。"""
    w, h = width / PPU, height / PPU
    center = pivot + RIGHT * (left + w / 2) + UP * (top - h / 2)
    cam.data.ortho_scale = max(w, h)
    cam.location = center - FORWARD * 10
    scene.render.resolution_x = width * 2
    scene.render.resolution_y = height * 2


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    out = argv[0]
    os.makedirs(os.path.join(out, "sprites"), exist_ok=True)
    scene, cam = stage()
    sprites = []
    radius = {}
    global PASS
    for name, pivot, angle, build, square in jobs():
        clear()
        PASS = next((part for part in ("base", "trim") if f"/{part}/" in name), None)
        scene.view_settings.view_transform = "Standard" if tinted(name) else TONE
        scene.view_settings.exposure = EXPOSURE - HEADROOM if tinted(name) else EXPOSURE
        holder = spun(pivot, angle) if angle else empty()
        build(holder)
        group = name.rsplit("/", 1)[0] if square else name
        if square:
            # 一组转着的共用一个方框，边长按离锚点最远的那一点，转到哪都装得下。
            if group not in radius:
                radius[group] = extent(pivot)[4]
            r = radius[group]
            left, top = -r, r
            width = height = math.ceil(2 * r * PPU)
        else:
            left, right, bottom, top, _ = extent(pivot)
            width, height = math.ceil((right - left) * PPU), math.ceil((top - bottom) * PPU)
        frame(scene, cam, pivot, left, top, width, height)
        index = len(sprites)
        scene.render.filepath = os.path.join(out, "sprites", f"{index:04}.png")
        bpy.ops.render.render(write_still=True)
        sprites.append({"name": name, "width": width, "height": height,
                        "anchor": [round(-left * PPU, 2), round(top * PPU, 2)]})
        print(f"buddies: {name}", flush=True)
    with open(os.path.join(out, "sprites.json"), "w") as f:
        json.dump({"colors": [{"name": name, "rgb": oklch(*lch), "oklch": lch} for name, lch in COLORS],
                   "wear_colors": [{"name": name, "rgb": oklch(*lch), "oklch": lch} for name, lch in WEAR_COLORS],
                   "wear": {kind: [{"name": name, "base": base, "trim": top, "eyes": eyes}
                                   for name, (_, base, top, eyes) in items.items()]
                            for kind, items in (("head", HEADS), ("face", FACES), ("neck", NECKS))},
                   "neutral": NEUTRAL, "headroom": HEADROOM, "keys": KEYS, "facings": FACINGS,
                   "sprites": sprites}, f)
    with open(os.path.join(out, "rig.json"), "w") as f:
        json.dump(rig(), f, indent=1)


main()
