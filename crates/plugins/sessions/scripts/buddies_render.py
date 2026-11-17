# SPDX-License-Identifier: GPL-3.0-or-later
# 这个文件按 GPL：它 `import bpy`，Blender 认为调它接口的脚本是 Blender 的衍生作品。渲出来的图
# 不受此约束；仓库里别的文件（含打包脚本 buddies_pack.py）照旧是仓库的许可。
"""用 Blender 渲会话行首 3D 小人的零件，再由 buddies_pack.py 打包编进插件，插件在运行时按姿势拼。

不渲整段动画：动作由插件里的代码现场算（src/list/buddy/），这里只出零件和拼装用的位置。
gpui 转不了彩色图，所以会转的零件（胳膊、手里的道具）每 10° 渲一张，共 36 张。

  身子    每种颜色一张，含脚和腮红，锚点在脚底。
  胳膊    每种颜色 36 个朝向，锚点在肩。
  道具    每样 36 个朝向，锚点在握的地方。
  脸      眼睛（睁、笑眯、闭）、嘴（笑、平、张），锚点在自己中心，拼的时候摆到脸上。
  戴的    每个职业一张，锚点在头顶。
  身前的  码农的电脑、写手的本子，各按亮着几行出 5 张。
  特效    火星、泡泡、水滴、音符、灰、点、感叹号、灯泡、星星、爱心、彩纸、嫩芽。

运行（要装 Blender，`brew install --cask blender`）：

  blender --background --factory-startup --python crates/plugins/sessions/scripts/buddies_render.py \\
      -- target/buddies

输出 <目录>/sprites/<序号>.png、sprites.json（每张的名字、大小、锚点，按成图像素）和 rig.json
（脚底在画布上的位置、肩、眼、嘴、头顶、身前物件、道具尖端相对脚底或握点的位置）。渲图是成图的两倍，
打包时缩下去。

场景约定：Blender 的 z 朝上，小人面朝 -y 再往右转一点（YAW），脚底在原点；正交镜头从前上方看，
整个小人的画布是 36pt（成图 72px）。画面里的角度从正上方起顺时针算。
"""
import json
import math
import os
import sys

import bpy
from mathutils import Matrix, Quaternion, Vector

SAMPLES = 32

# 身子色（sRGB）。避开大面积的橙黄。
COLORS = [
    ("blue", (0.40, 0.60, 0.80)),
    ("coral", (0.90, 0.53, 0.45)),
    ("green", (0.47, 0.72, 0.54)),
    ("purple", (0.62, 0.53, 0.82)),
    ("teal", (0.32, 0.68, 0.68)),
    ("pink", (0.91, 0.58, 0.70)),
    ("sand", (0.86, 0.76, 0.56)),
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


def new_clay(color, rough, grain):
    mat, bsdf = principled("clay")
    bsdf.inputs["Base Color"].default_value = (*srgb(color), 1)
    bsdf.inputs["Roughness"].default_value = rough
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


def clay(color, rough=0.7, grain=0.25):
    """哑光黏土，表面带一点颗粒。同参数的共用一份。"""
    key = ("clay", color, rough, grain)
    if key not in MATS:
        MATS[key] = new_clay(color, rough, grain)
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


# 身子和胳膊共用 SKIN，脚用深一档的 SHADE；换颜色只改这两份。
SKIN = "skin"
SHADE = "shade"


def body_material(which):
    if which not in MATS:
        MATS[which] = new_clay((1, 1, 1), 0.7, 0.25)
    return MATS[which]


def paint(color):
    for which, k in ((SKIN, 1.0), (SHADE, 0.72)):
        bsdf = body_material(which).node_tree.nodes["Principled BSDF"]
        bsdf.inputs["Base Color"].default_value = (*srgb(tuple(v * k for v in color)), 1)


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


def sphere(loc, scale, mat, parent):
    bpy.ops.mesh.primitive_uv_sphere_add(segments=24, ring_count=12, location=loc)
    obj = bpy.context.object
    obj.scale = scale
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


def torus(loc, major, minor, mat, parent, rot=(0, 0, 0), scale=(1, 1, 1)):
    bpy.ops.mesh.primitive_torus_add(major_radius=major, minor_radius=minor,
                                     major_segments=40, minor_segments=12,
                                     location=loc, rotation=rot)
    obj = bpy.context.object
    obj.scale = scale
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

BODY_W, BODY_D, BODY_H = 0.92, 0.66, 0.84
FOOT = 0.1
TOP = FOOT + BODY_H
FACE_Y = -BODY_D / 2


# ---- 镜头和投影 ----

YAW = -14
PPU = 72 / 1.7          # 成图每单位多少像素：画布 72px 框住 1.7 个单位
TARGET = Vector((0, 0, 0.62))  # 整个小人的画布中心
CAM = Vector((0, -6, 2.0))
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


def character(parent=None):
    """角色坐标的根：转过 YAW。"""
    return empty((0, 0, 0), parent, rot=rad(0, 0, YAW))


# ---- 身子和脸 ----

ARM_LEN = 0.24
SHOULDER_R = (0.4, -0.04, 0.46)
EYE = (0.17, FACE_Y, 0.6)
MOUTH = (0.0, FACE_Y, 0.49)


def body(root):
    box((0, 0, FOOT + BODY_H / 2), (BODY_W, BODY_D, BODY_H), body_material(SKIN), root, bevel=0.24)
    for x in (-0.2, 0.2):
        sphere((x, -0.04, 0.08), (0.15, 0.17, 0.09), body_material(SHADE), root)
    blush = clay((0.95, 0.55, 0.55), grain=0)
    for x in (-0.3, 0.3):
        sphere((x, FACE_Y + 0.03, 0.5), (0.07, 0.03, 0.045), blush, root)


def eye(root, kind):
    """一只眼，搭在 x=0 处，拼的时候摆两次。"""
    ink = clay(INK, rough=0.25, grain=0)
    z = EYE[2]
    if kind == "open":
        sphere((0, FACE_Y + 0.012, z), (0.06, 0.03, 0.075), ink, root)
        sphere((0.022, FACE_Y - 0.012, z + 0.025), (0.017, 0.01, 0.017), glow((1, 1, 1), 2.0), root)
    elif kind == "happy":
        points = [(x * 0.06, FACE_Y - 0.004, z - 0.02 + 0.035 * (1 - x * x)) for x in (-1, -0.5, 0, 0.5, 1)]
        tube(points, 0.016, ink, root)
    else:  # "shut"：眨眼
        tube([(-0.055, FACE_Y - 0.004, z - 0.01), (0.055, FACE_Y - 0.004, z - 0.01)], 0.014, ink, root)


def mouth(root, kind):
    ink = clay(INK, grain=0)
    z = MOUTH[2]
    if kind == "smile":
        points = [(x * 0.06, FACE_Y - 0.004, z + 0.01 - 0.035 * (1 - x * x)) for x in (-1, -0.5, 0, 0.5, 1)]
        tube(points, 0.011, ink, root)
    elif kind == "flat":
        tube([(-0.045, FACE_Y - 0.004, z), (0.045, FACE_Y - 0.004, z)], 0.011, ink, root)
    else:  # "open"
        sphere((0, FACE_Y + 0.005, z - 0.01), (0.045, 0.02, 0.05), clay((0.45, 0.12, 0.16), grain=0), root)


def arm(parent):
    """右肩往画面正下方伸的一条胳膊，手是个圆头。"""
    shoulder = placed(SHOULDER_R)
    hand = shoulder - UP * ARM_LEN
    tube([tuple(shoulder), tuple(hand)], 0.07, body_material(SKIN), parent)
    sphere(tuple(hand), (0.085, 0.085, 0.085), body_material(SKIN), parent)


# ---- 戴的 ----

def beanie(root):
    knit = clay((0.3, 0.32, 0.38), rough=0.9, grain=0.6)
    sphere((0, 0.02, TOP - 0.1), (0.42, 0.36, 0.3), knit, root)
    torus((0, 0.02, TOP - 0.1), 0.39, 0.075, knit, root, scale=(1, 0.86, 1))
    sphere((0, 0.02, TOP + 0.22), (0.1, 0.1, 0.1), clay((0.96, 0.94, 0.9), rough=1, grain=0.8), root)


def cap(root):
    cloth = clay((0.98, 0.76, 0.2), rough=0.6)
    sphere((0, 0.03, TOP - 0.06), (0.4, 0.34, 0.24), cloth, root)
    brim = cylinder((0, -0.3, TOP - 0.06), 0.22, 0.035, cloth, root, bevel=0.012, scale=(1.2, 1, 1))
    brim.rotation_euler = rad(-12, 0, 0)
    sphere((0, 0.03, TOP + 0.18), (0.04, 0.04, 0.03), cloth, root)


def fedora(root):
    felt = clay((0.45, 0.36, 0.28), rough=0.9, grain=0.4)
    cylinder((0, 0.02, TOP), 0.5, 0.04, felt, root, bevel=0.015, scale=(1, 0.82, 1))
    cylinder((0, 0.02, TOP + 0.13), 0.3, 0.26, felt, root, bevel=0.06, scale=(1, 0.82, 1))
    cylinder((0, 0.02, TOP + 0.05), 0.305, 0.07, clay(INK), root, scale=(1, 0.82, 1))


def beret(root):
    wool = clay((0.62, 0.18, 0.24), rough=0.9, grain=0.5)
    tilt = empty((0.04, 0.02, TOP - 0.02), root, rot=rad(0, 14, 0))
    sphere((0, 0, 0.04), (0.44, 0.38, 0.13), wool, tilt)
    cylinder((0, 0, 0.17), 0.025, 0.06, wool, tilt)


def goggles(root):
    strap = clay((0.25, 0.26, 0.3), rough=0.7, grain=0)
    rim = clay((0.95, 0.62, 0.2), rough=0.4, grain=0)
    torus((0, 0, 0.8), 0.47, 0.035, strap, root, scale=(1, 0.74, 1))
    for x in (-0.17, 0.17):
        torus((x, FACE_Y - 0.03, 0.8), 0.1, 0.03, rim, root, rot=rad(90, 0, 0))
        cylinder((x, FACE_Y - 0.03, 0.8), 0.09, 0.02, glass((0.7, 0.95, 0.9)), root, rot=rad(90, 0, 0))
    tube([(-0.07, FACE_Y - 0.03, 0.8), (0.07, FACE_Y - 0.03, 0.8)], 0.02, rim, root)


def straw_hat(root):
    straw = clay((0.92, 0.8, 0.5), rough=0.9, grain=0.7)
    cylinder((0, 0.02, TOP - 0.02), 0.58, 0.035, straw, root, bevel=0.015, scale=(1, 0.86, 1))
    sphere((0, 0.02, TOP + 0.02), (0.3, 0.27, 0.2), straw, root)
    cylinder((0, 0.02, TOP + 0.03), 0.3, 0.06, clay((0.85, 0.3, 0.3)), root, scale=(1, 0.9, 1))


def bow_tie(root):
    silk = clay((0.72, 0.16, 0.22), rough=0.4, grain=0)
    for side in (-1, 1):
        cone((side * 0.09, FACE_Y - 0.01, 0.34), 0.08, 0.02, 0.15, silk, root, rot=rad(0, side * -90, 0))
    sphere((0, FACE_Y - 0.03, 0.34), (0.035, 0.03, 0.035), silk, root)
    # 头顶一片往前翘的黑头发，远看也认得出是个讲究人。
    hair = clay(INK, rough=0.45, grain=0)
    sphere((0, 0.03, TOP - 0.06), (0.4, 0.33, 0.14), hair, root)
    sphere((0.08, -0.16, TOP + 0.04), (0.24, 0.14, 0.1), hair, root).rotation_euler = rad(0, -18, 0)


def bandana(root):
    cloth = clay((0.82, 0.22, 0.24), rough=0.85, grain=0.3)
    sphere((0, 0.02, TOP - 0.1), (0.43, 0.36, 0.2), cloth, root)
    dots = clay((0.95, 0.95, 0.95), grain=0)
    for x, z in ((-0.18, 0.02), (0.06, 0.07), (0.25, 0.0)):
        sphere((x, FACE_Y - 0.02, TOP - 0.12 + z), (0.03, 0.012, 0.03), dots, root)
    for dz, rot in ((0.0, 30), (-0.1, -20)):
        sphere((0.38, 0.22, TOP - 0.14 + dz), (0.09, 0.05, 0.05), cloth, root).rotation_euler = rad(0, rot, 0)


# ---- 拿的：以手为原点、朝 +z 伸 ----

def laptop(hand, lines=4):
    """屏幕朝外端在肚子前：小尺寸下一块亮屏最好认。`lines` 是屏上亮着几行代码。"""
    shell = clay((0.36, 0.38, 0.44), rough=0.4, grain=0)
    base = empty((-0.3, -0.08, 0.02), hand, rot=rad(8, 0, 0))
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
    steel = clay((0.52, 0.56, 0.63), rough=0.28, grain=0)
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
    brass = clay((0.86, 0.64, 0.3), rough=0.3, grain=0)
    cylinder((0, 0, 0.1), 0.035, 0.24, clay((0.45, 0.28, 0.16)), hand, bevel=0.012)
    torus((0, 0, 0.4), 0.17, 0.032, brass, hand, rot=rad(90, 0, 0))
    cylinder((0, 0, 0.4), 0.16, 0.014, glass(), hand, rot=rad(90, 0, 0))


def pencil(hand):
    wood = clay((0.98, 0.78, 0.25), rough=0.6, grain=0)
    cylinder((0, 0, 0.14), 0.04, 0.36, wood, hand, bevel=0.005)
    cone((0, 0, 0.36), 0.04, 0.0, 0.09, clay((0.93, 0.8, 0.62), grain=0), hand)
    cone((0, 0, 0.392), 0.0145, 0.0, 0.026, clay(INK, grain=0), hand)
    cylinder((0, 0, -0.06), 0.042, 0.05, clay((0.75, 0.76, 0.8), rough=0.3, grain=0), hand)
    cylinder((0, 0, -0.1), 0.04, 0.05, clay((0.95, 0.55, 0.6), grain=0), hand, bevel=0.012)


def notepad(hand, lines=0):
    paper = clay((0.97, 0.95, 0.88), rough=0.8, grain=0)
    pad = empty((0.12, -0.06, 0.12), hand)
    box((0, 0, 0), (0.34, 0.03, 0.42), paper, pad, bevel=0.01)
    box((0, 0.006, 0.2), (0.36, 0.035, 0.05), clay((0.35, 0.45, 0.7)), pad, bevel=0.01)
    for i in range(lines):
        width = 0.22 if i % 2 == 0 else 0.16
        box((-0.11 + width / 2, -0.017, 0.12 - i * 0.07), (width, 0.004, 0.018),
            clay((0.35, 0.4, 0.55), grain=0), pad)


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
    tin = clay((0.3, 0.62, 0.6), rough=0.35, grain=0)
    can = empty((0, -0.02, -0.2), hand)
    cylinder((0, 0, 0), 0.15, 0.22, tin, can, bevel=0.02, scale=(1, 0.85, 1))
    torus((0, 0, 0.14), 0.11, 0.022, tin, can, rot=rad(90, 0, 0))
    spout_base = (0.12, 0, -0.04)
    tip = (0.26, 0, 0.1)
    tube([spout_base, tip], 0.025, tin, can)
    cone((tip[0] + 0.02, 0, tip[2] + 0.015), 0.025, 0.055, 0.05, tin, can, rot=rad(0, 54, 0))
    return empty((tip[0] + 0.04, 0, tip[2] + 0.03), can)


def baton(hand):
    cylinder((0, 0, 0.0), 0.03, 0.12, clay(INK, rough=0.5, grain=0), hand, bevel=0.01)
    cylinder((0, 0, 0.26), 0.018, 0.42, clay((0.97, 0.97, 0.95), rough=0.4, grain=0), hand)


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
    ink = clay((0.95, 0.55, 0.2), rough=0.5, grain=0)
    note = empty(FX_AT, root)
    sphere((0, 0, 0), (0.055, 0.03, 0.042), ink, note).rotation_euler = rad(0, -25, 0)
    box((0.045, 0, 0.1), (0.018, 0.018, 0.2), ink, note)
    box((0.08, 0, 0.18), (0.07, 0.018, 0.03), ink, note, rot=rad(0, 30, 0))


def fx_puff(root):
    sphere(FX_AT, (0.08, 0.08, 0.07), clay((0.78, 0.75, 0.7), rough=1.0, grain=0), root)


def fx_dot(root):
    sphere(FX_AT, (0.045, 0.045, 0.045), clay((0.62, 0.64, 0.7), grain=0), root)


def fx_dot_lit(root):
    sphere(FX_AT, (0.045, 0.045, 0.045), glow((0.4, 0.75, 1.0), 4.0), root)


def fx_bang(root):
    red = clay((0.95, 0.3, 0.25), rough=0.4, grain=0)
    holder = empty(FX_AT, root)
    box((0, 0, 0.06), (0.07, 0.07, 0.2), red, holder, bevel=0.03)
    sphere((0, 0, -0.1), (0.045, 0.045, 0.045), red, holder)


def fx_bulb(root, lit):
    holder = empty(FX_AT, root)
    shell = glow((1.0, 0.86, 0.35), 4.0) if lit else glass((0.95, 0.95, 0.9))
    sphere((0, 0, 0.04), (0.09, 0.09, 0.1), shell, holder)
    cylinder((0, 0, -0.07), 0.045, 0.06, clay((0.6, 0.62, 0.66), rough=0.3, grain=0), holder)


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
    red = clay((0.95, 0.36, 0.45), rough=0.4, grain=0)
    holder = empty(FX_AT, root)
    for side in (-1, 1):
        sphere((side * 0.04, 0, 0.03), (0.055, 0.04, 0.055), red, holder)
    cone((0, 0, -0.03), 0.075, 0.0, 0.1, red, holder, rot=rad(180, 0, 0)).scale = (1, 0.55, 1)


CONFETTI = [(0.95, 0.4, 0.4), (0.45, 0.7, 1.0), (1.0, 0.82, 0.3), (0.55, 0.85, 0.5)]


def fx_confetti(root, k):
    box(FX_AT, (0.07, 0.015, 0.04), clay(CONFETTI[k], rough=0.5, grain=0), root,
        rot=rad(20 * k, 35 + 25 * k, 10 * k))


SPROUT_AT = (0.7, -0.3, 0.0)


def sprout(root, stage):
    """地上的苗，长到第几截（0 到 4）。锚点在根上。"""
    h = 0.06 + 0.06 * stage
    holder = empty(SPROUT_AT, root)
    cylinder((0, 0, h / 2), 0.02, h, clay((0.3, 0.55, 0.28), grain=0), holder)
    leaf = clay((0.4, 0.72, 0.36), rough=0.6, grain=0)
    size = 0.5 + 0.12 * stage
    for side in (-1, 1):
        sphere((side * 0.07 * size, 0, h), (0.08 * size, 0.03, 0.04 * size), leaf, holder).rotation_euler = \
            rad(0, side * -25, 0)
    if stage == 4:
        sphere((0, -0.01, h + 0.05), (0.05, 0.04, 0.05), clay((0.98, 0.6, 0.7), grain=0), holder)


# ---- 零件清单 ----

HATS = {
    "coder": beanie, "fixer": cap, "detective": fedora, "writer": beret,
    "chemist": goggles, "gardener": straw_hat, "conductor": bow_tie, "sweeper": bandana,
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
LAPTOP_AT = (0.0, -0.64, 0.18)  # 电脑底座中心（角色坐标）
LAPTOP_HAND = (0.3, -0.56, 0.16)
NOTEPAD_FROM_HAND = (0.12, -0.06, 0.12)
LEFT_HAND = (-0.32, -0.5, 0.2)
EFFECTS = {
    "spark": fx_spark, "bubble": fx_bubble, "drop": fx_drop, "note": fx_note, "puff": fx_puff,
    "dot": fx_dot, "dot-lit": fx_dot_lit, "bang": fx_bang, "star": fx_star, "heart": fx_heart,
    "bulb": lambda root: fx_bulb(root, False), "bulb-lit": lambda root: fx_bulb(root, True),
}
ANGLES = 36


def jobs():
    """（名字, 锚点（世界）, 转角, 搭法, 按半径取方框?, 要不要逐色渲）。按名字分组连着出。"""
    feet = Vector((0, 0, 0))
    yield "body", feet, 0, lambda p: body(character(p)), False, True
    for angle in range(ANGLES):
        yield f"arm/{angle:02}", placed(SHOULDER_R), angle * 10 - 180, arm, True, True
    for name, (build, _) in PROPS.items():
        grip = placed(GRIP)
        for angle in range(ANGLES):
            def make(p, build=build):
                hand = empty(GRIP, character(p))
                build(hand)
            yield f"prop/{name}/{angle:02}", grip, angle * 10, make, True, False
    for kind in ("open", "happy", "shut"):
        yield f"eye/{kind}", placed((0, EYE[1], EYE[2])), 0, lambda p, k=kind: eye(character(p), k), False, False
    for kind in ("smile", "flat", "open"):
        yield f"mouth/{kind}", placed(MOUTH), 0, lambda p, k=kind: mouth(character(p), k), False, False
    for role, build in HATS.items():
        yield f"hat/{role}", placed((0, 0, TOP)), 0, lambda p, b=build: b(character(p)), False, False
    for lines in range(5):
        def make_laptop(p, lines=lines):
            laptop(empty(LAPTOP_HAND, character(p)), lines)
        yield f"laptop/{lines}", placed(LAPTOP_AT), 0, make_laptop, False, False
    for lines in range(5):
        def make_pad(p, lines=lines):
            notepad(empty(LEFT_HAND, character(p)), lines)
        pad = placed(tuple(a + b for a, b in zip(LEFT_HAND, NOTEPAD_FROM_HAND)))
        yield f"notepad/{lines}", pad, 0, make_pad, False, False
    for name, build in EFFECTS.items():
        yield f"fx/{name}", placed(FX_AT), 0, lambda p, b=build: b(character(p)), False, False
    for k in range(len(CONFETTI)):
        yield f"confetti/{k}", placed(FX_AT), 0, lambda p, k=k: fx_confetti(character(p), k), False, False
    for stage in range(5):
        yield f"sprout/{stage}", placed(SPROUT_AT), 0, lambda p, s=stage: sprout(character(p), s), False, False


def rig():
    """拼装用的位置，成图像素。身上的相对脚底；道具尖端相对握点、朝上（0°）时。"""
    feet = Vector((0, 0, 0))
    canvas = on_screen(feet - TARGET)
    shoulder_l = (-SHOULDER_R[0], SHOULDER_R[1], SHOULDER_R[2])
    eye_l = (-EYE[0], EYE[1], EYE[2])
    out = {
        "canvas": 72,
        "feet": [round(36 + canvas[0], 2), round(36 + canvas[1], 2)],
        "shoulder_l": on_screen(placed(shoulder_l)),
        "shoulder_r": on_screen(placed(SHOULDER_R)),
        "arm_len": round(ARM_LEN * PPU, 2),
        "eye_l": on_screen(placed(eye_l)),
        "eye_r": on_screen(placed(EYE)),
        "mouth": on_screen(placed(MOUTH)),
        "hat": on_screen(placed((0, 0, TOP))),
        "laptop_at": on_screen(placed(LAPTOP_AT)),
        "notepad_at": on_screen(placed(NOTEPAD_FROM_HAND)),
        "sprout_at": on_screen(placed(SPROUT_AT)),
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
    scene.view_settings.view_transform = "Standard"

    world = bpy.data.worlds.new("world")
    world.use_nodes = True
    background = world.node_tree.nodes["Background"]
    background.inputs[0].default_value = (0.9, 0.92, 1.0, 1)
    background.inputs[1].default_value = 0.55
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
        ("key", (-1.5, -2.2, 5), 480, 2.5, (1.0, 0.96, 0.9)),
        ("fill", (3, -2.5, 1.5), 120, 3, (0.85, 0.9, 1.0)),
        ("rim", (1.5, 3, 3), 260, 2, (1, 1, 1)),
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
    for name, pivot, angle, build, square, colored in jobs():
        clear()
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
        for color, rgb in (COLORS if colored else [(None, None)]):
            if rgb:
                paint(rgb)
            full = name.replace("/", f"/{color}/", 1) if color and "/" in name else (
                f"{name}/{color}" if color else name)
            index = len(sprites)
            scene.render.filepath = os.path.join(out, "sprites", f"{index:04}.png")
            bpy.ops.render.render(write_still=True)
            sprites.append({"name": full, "width": width, "height": height,
                            "anchor": [round(-left * PPU, 2), round(top * PPU, 2)]})
        print(f"buddies: {name}", flush=True)
    with open(os.path.join(out, "sprites.json"), "w") as f:
        json.dump({"colors": [color for color, _ in COLORS], "angles": ANGLES, "sprites": sprites}, f)
    with open(os.path.join(out, "rig.json"), "w") as f:
        json.dump(rig(), f, indent=1)


main()
