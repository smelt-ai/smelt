"""把 buddies_render.py 渲好的零件打包编进插件（src/list/buddy/），插件在运行时按姿势拼小人。

  python3 crates/plugins/sessions/scripts/buddies_pack.py target/buddies

输出：
  assets/buddies.bin            所有零件的 WebP 首尾相接
  src/list/buddy/sprites.rs     每张在包里的位置、大小、锚点，按组的编号常量，和拼装用的位置

渲图是成图的两倍，这里缩到成图（2x 屏上 36pt 的画布 = 72px），再按 pt 写进 Rust。脚下的影子不用
Blender 渲，这里画一张软椭圆。零件按组排：同一组（名字去掉末尾编号）连着放，按颜色分的组再按颜色
排，Rust 用「组的第一张 + 编号」取。

渲染那步要 Blender，单独放一个文件（它按 GPL）；这步只要 Pillow。
"""
import io
import json
import os
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
BLOB = os.path.join(HERE, "..", "assets", "buddies.bin")
RUST = os.path.join(HERE, "..", "src", "list", "buddy", "sprites.rs")
SCALE = 2  # 成图像素 / pt


def shadow():
    """脚下的影子：一块模糊的椭圆，成图像素。"""
    w, h, blur = 44, 14, 3
    image = Image.new("RGBA", (w, h))
    ImageDraw.Draw(image).ellipse((blur, blur, w - blur, h - blur), fill=(0, 0, 0, 80))
    return image.filter(ImageFilter.GaussianBlur(blur * 0.7)), [w / 2, h / 2]


def ident(name):
    return name.upper().replace("-", "_").replace("/", "_")


def main():
    rendered = sys.argv[1]
    spec = json.load(open(os.path.join(rendered, "sprites.json")))
    rig = json.load(open(os.path.join(rendered, "rig.json")))
    colors = spec["colors"]

    # 名字拆成（组, 编号）：末段是数字的是组里第几张。组按第一次出现排，组内按编号。
    entries = []
    for index, sprite in enumerate(spec["sprites"]):
        parts = sprite["name"].split("/")
        number = int(parts[-1]) if parts[-1].isdigit() else None
        group = "/".join(parts[:-1] if number is not None else parts)
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
                    f"size: ({image.width / SCALE}, {image.height / SCALE}), "
                    f"anchor: ({ax / SCALE}, {ay / SCALE}) }},")
        blob += data
        firsts.setdefault(group, index)
        if number is not None:
            assert index - firsts[group] == number, f"{group} 缺第 {index - firsts[group]} 张"

    consts = []
    for group, first in firsts.items():
        consts.append(f"pub(super) const {ident(group)}: usize = {first};")
    # 按颜色分的组再给一张表：BODY[颜色]、ARM[颜色]。
    for stem in sorted({group.split("/")[0] for group in firsts if group.split("/")[-1] in colors}):
        items = ", ".join(ident(f"{stem}/{color}") for color in colors)
        consts.append(f"pub(super) const {ident(stem)}: [usize; {len(colors)}] = [{items}];")

    places = []
    for key, value in rig.items():
        if key == "canvas":
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
            "use super::Sprite;\n\n"
            "pub(super) const BLOB: &[u8] = include_bytes!(\"../../../assets/buddies.bin\");\n\n"
            f"/// 转着的零件每组几张（每 {360 // spec['angles']}° 一张）。\n"
            f"pub(super) const ANGLES: usize = {spec['angles']};\n"
            f"/// 身子有几种颜色（顺序：{'、'.join(colors)}）。\n"
            f"pub(super) const COLORS: usize = {len(colors)};\n\n"
            "/// 每组第一张的编号。\n"
            + "\n".join(consts) + "\n\n"
            "/// 拼装用的位置（pt）。`FEET` 是脚底在画布上的位置；身上各处相对脚底；`TIP_*` 是道具\n"
            "/// 朝上时尖端相对握点；`NOTEPAD_AT` 相对左手；`ARM_LEN` 是肩到手。\n"
            + "\n".join(places) + "\n\n"
            f"pub(super) const SPRITES: [Sprite; {len(rows)}] = [\n" + "\n".join(rows) + "\n];\n")
    # 按仓库的 rustfmt 排一遍，生成的和手排的一样（rustfmt 跟着 Rust 工具链装）。
    config = os.path.join(HERE, "..", "..", "..", "..", "rustfmt.toml")
    subprocess.run(["rustfmt", "--edition", "2024", "--config-path", config, RUST], check=True)
    print(f"wrote {len(rows)} sprites ({len(blob)} bytes) and {os.path.normpath(RUST)}")


if __name__ == "__main__":
    main()
