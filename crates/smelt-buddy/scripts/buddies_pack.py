"""把 buddies_render.py 渲好的零件打包编进插件（src/buddy/），插件在运行时按姿势拼小人。

  python3 crates/smelt-buddy/scripts/buddies_pack.py target/buddies

输出：
  assets/buddies.bin            所有零件的 WebP 首尾相接
  src/buddy/sprites.rs          每张在包里的位置、锚点（大小看解出来的图），按组的编号常量，和拼装用的位置

渲图是成图的两倍，这里缩到成图（2x 屏上 36pt 的画布 = 72px），再按 pt 写进 Rust。脚下的影子不用
Blender 渲，这里画一张软椭圆。零件按组排：同一组（名字去掉末尾编号）连着放，Rust 用「组的第一张 +
编号」取（跟着身子转身的零件按朝向编号）。身子、胳膊和戴的东西的主料是中性灰（比别的零件暗 headroom 档渲，留出高光），这里按身子色、
戴的颜色算出插件要乘的系数（线性，乘回那几档），再把戴的东西列成表（每件有没有主料、点缀两张，占
不占眼睛），列出每种身子色配哪些戴的颜色。

撞色按 OKLab 判：两个都带色的，色相差不到 CLASH_HUE 度不配（同色系深浅搭也糊）；不管带不带色，
OKLab 里的距离（ΔEOK，CSS Color 4 的感知色差）不到 CLASH_DISTANCE 不配（浅黄身子配奶白围巾）。

渲染那步要 Blender，单独放一个文件（它按 GPL）；这步只要 Pillow。
"""
import io
import json
import math
import os
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
BLOB = os.path.join(HERE, "..", "assets", "buddies.bin")
RUST = os.path.join(HERE, "..", "src", "buddy", "sprites.rs")
SCALE = 2  # 成图像素 / pt

CLASH_HUE = 40         # 都带色时色相差不到这么多度不配
CLASH_DISTANCE = 0.2   # OKLab 距离不到这么多不配
CHROMATIC = 0.05       # OKLCH 彩度到这么多算带色


def shadow():
    """脚下的影子：一块模糊的椭圆，成图像素。"""
    w, h, blur = 44, 14, 3
    image = Image.new("RGBA", (w, h))
    ImageDraw.Draw(image).ellipse((blur, blur, w - blur, h - blur), fill=(0, 0, 0, 80))
    return image.filter(ImageFilter.GaussianBlur(blur * 0.7)), [w / 2, h / 2]


def linear(v):
    return ((v + 0.055) / 1.055) ** 2.4 if v > 0.04045 else v / 12.92


def oklab(lightness, chroma, hue):
    return (lightness, chroma * math.cos(math.radians(hue)), chroma * math.sin(math.radians(hue)))


def clashes(body, wear):
    """身子色和戴的颜色（都是 OKLCH）配不配得上。"""
    hue_gap = abs((wear[2] - body[2] + 180) % 360 - 180)
    if min(body[1], wear[1]) >= CHROMATIC and hue_gap < CLASH_HUE:
        return True
    return math.dist(oklab(*body), oklab(*wear)) < CLASH_DISTANCE


def ident(name):
    return name.upper().replace("-", "_").replace("/", "_")


def main():
    rendered = sys.argv[1]
    spec = json.load(open(os.path.join(rendered, "sprites.json")))
    rig = json.load(open(os.path.join(rendered, "rig.json")))
    neutral = spec["neutral"]
    headroom = 2 ** spec["headroom"]

    # 名字拆成（组, 编号）：末段是数字的是组里第几张。组按第一次出现排，组内按编号。
    entries = []
    for sprite in spec["sprites"]:
        parts = sprite["name"].split("/")
        number = int(parts[-1]) if parts[-1].isdigit() else None
        group = "/".join(parts[:-1] if number is not None else parts)
        index = len(entries)
        image = Image.open(os.path.join(rendered, "sprites", f"{index:04}.png")).convert("RGBA")
        image = image.resize((sprite["width"], sprite["height"]), Image.LANCZOS)
        entries.append((group, number, image, sprite["anchor"]))
    image, anchor = shadow()
    entries.append(("shadow", None, image, anchor))
    order = {}
    for group, *_ in entries:
        order.setdefault(group, len(order))
    entries.sort(key=lambda entry: (order[entry[0]], entry[1] or 0))

    blob = bytearray()
    rows, firsts = [], {}
    for index, (group, number, image, (ax, ay)) in enumerate(entries):
        out = io.BytesIO()
        image.save(out, "WEBP", quality=90, method=6)
        data = out.getvalue()
        rows.append(f"    Sprite {{ start: {len(blob)}, len: {len(data)}, "
                    f"anchor: ({ax / SCALE}, {ay / SCALE}) }},")
        blob += data
        firsts.setdefault(group, index)
        if number is not None:
            assert index - firsts[group] == number, f"{group} 缺第 {index - firsts[group]} 张"

    consts = []
    for group, first in firsts.items():
        consts.append(f"pub(super) const {ident(group)}: usize = {first};")

    def tints(name, colors, what):
        rows = []
        for color in colors:
            factors = [linear(c) / linear(n) * headroom for c, n in zip(color["rgb"], neutral)]
            rows.append("[" + ", ".join(f"{f:.4f}" for f in factors) + "]")
        return (f"/// {what}（线性 RGB，乘完超过 1 的截断）。顺序："
                f"{'、'.join(color['name'] for color in colors)}。\n"
                f"pub(super) const {name}: [[f32; 3]; {len(colors)}] = [{', '.join(rows)}];\n")

    def allowed(body):
        """身子色 `body` 配得上的戴的颜色，按位（第 i 位是第 i 种）。"""
        mask = 0
        for i, wear in enumerate(spec["wear_colors"]):
            if not clashes(body["oklch"], wear["oklch"]):
                mask |= 1 << i
        return mask

    def wearing(kind, items):
        rows = []
        for item in items:
            group = f"{kind}/{item['name']}"
            base = f"Some({ident(group + '/base')})" if item["base"] else "None"
            trim = f"Some({ident(group + '/trim')})" if item["trim"] else "None"
            eyes = "true" if item["eyes"] else "false"
            dangle = "None"
            if item.get("dangle"):
                hung = group + "/dangle"
                parts = {part: f"Some({ident(hung + '/' + part)})" if item["dangle"][part] else "None"
                         for part in ("base", "trim")}
                pivots = ", ".join(f"({x / SCALE}, {y / SCALE})" for x, y in rig["dangles"][item["name"]])
                dangle = (f"Some(Dangle {{ base: {parts['base']}, trim: {parts['trim']}, "
                          f"pivots: [{pivots}] }})")
            rows.append(f"    Wearable {{ base: {base}, trim: {trim}, eyes: {eyes}, dangle: {dangle} }}, // {item['name']}")
        return f"pub(super) static {ident(kind)}S: [Wearable; {len(items)}] = [\n" + "\n".join(rows) + "\n];\n"

    places = []
    for key, value in rig.items():
        if key == "shoulders":
            pairs = ", ".join("[" + ", ".join("(" + ", ".join(f"{v / SCALE}" for v in point) + ")" for point in pair)
                              + "]" for pair in value)
            places.append(f"pub(super) const SHOULDERS: [[(f32, f32, f32); 2]; {len(value)}] = [{pairs}];")
        elif key == "dangles":
            continue  # 写进戴的那张表（`wearing`）。
        elif key == "canvas":
            places.append(f"pub(super) const CANVAS: f32 = {value / SCALE};")
        elif isinstance(value, list):
            places.append(f"pub(super) const {ident(key)}: (f32, f32) = "
                          f"({value[0] / SCALE}, {value[1] / SCALE});")
        else:
            places.append(f"pub(super) const {ident(key)}: f32 = {value / SCALE};")

    with open(BLOB, "wb") as f:
        f.write(blob)
    with open(RUST, "w") as f:
        f.write(
            "// 由 scripts/buddies_pack.py 生成，别手改：改 buddies_render.py 重渲再打包。\n\n"
            "use super::{Dangle, Sprite, Wearable};\n\n"
            "pub(super) const BLOB: &[u8] = include_bytes!(\"../../assets/buddies.bin\");\n\n"
            f"/// 会转的零件每组几个关键朝向（每 {360 // spec['keys']}° 一张，从 0° 起）。\n"
            f"pub(super) const KEYS: usize = {spec['keys']};\n"
            f"/// 跟着身子转身的零件每组几个朝向（每 {360 // spec['facings']}° 一张，0 是正面，往画面右边转为正）。\n"
            f"pub(super) const FACINGS: usize = {spec['facings']};\n"
            + tints("BODY_TINTS", spec["colors"], "身子色乘到中性灰的身子、胳膊上的系数")
            + tints("WEAR_TINTS", spec["wear_colors"], "戴的东西的颜色乘到中性灰的主料上的系数")
            + "/// 每种身子色配哪些戴的颜色（第 i 位是 `WEAR_TINTS` 第 i 种）：色相太近或色差太小的不配，\n"
            + "/// 戴上糊成一片（见 `buddies_pack.py` 的 `clashes`）。\n"
            + f"pub(super) const WEAR_FOR_BODY: [u16; {len(spec['colors'])}] = "
            + "[" + ", ".join(f"0b{allowed(body):0{len(spec['wear_colors'])}b}" for body in spec["colors"]) + "];\n"
            + "\n/// 戴的东西，头顶、脸上、脖子各一张表：可换色的主料、固定色的点缀各是哪张，头顶会晃的部分和它每个朝向的挂点。\n"
            + "".join(wearing(kind, spec["wear"][kind]) for kind in ("head", "face", "neck"))
            + "\n"
            "/// 每组第一张的编号。\n"
            + "\n".join(consts) + "\n\n"
            "/// 拼装用的位置（pt）。`FEET` 是脚底在画布上的位置；身上各处相对脚底；`TIP_*` 是道具\n"
            "/// 朝上时尖端相对握点；`NOTEPAD_AT` 相对左手；`ARM_LEN` 是肩到手。`SHOULDERS` 是每个朝向\n"
            "/// 左、右肩的位置和比身子中轴离镜头远多少（正的在身子后面）。\n"
            + "\n".join(places) + "\n\n"
            f"pub(super) static SPRITES: [Sprite; {len(rows)}] = [\n" + "\n".join(rows) + "\n];\n")
    # 按仓库的 rustfmt 排一遍，生成的和手排的一样（rustfmt 跟着 Rust 工具链装）。
    config = os.path.join(HERE, "..", "..", "..", "..", "rustfmt.toml")
    subprocess.run(["rustfmt", "--edition", "2024", "--config-path", config, RUST], check=True)
    print(f"wrote {len(rows)} sprites ({len(blob)} bytes) and {os.path.normpath(RUST)}")


if __name__ == "__main__":
    main()
