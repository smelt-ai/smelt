# 从 Phosphor Icons 的 npm 包取界面图标，产物在 crates/smelt-ui/assets/phosphor/<名字>.svg。
#
# 只收清单 crates/smelt-ui/assets/phosphor/icons.txt 里列的图标：每行「名字 来源 [粗细] [mirror]」，
# 名字是 smelt 里的叫法（资源路径 icons/<名字>.svg、IconName 的变体名都从它来），来源是 Phosphor
# 的图标名，mirror 表示左右翻转（Phosphor 只有左侧栏，右侧栏翻出来）。粗细默认 bold：regular 在
# 14px 下线宽不到 1px，灰色一压就发虚，bold 和常见的 2px 描边图标一样粗；品牌标志写 fill。
# 换图标集、换某一枚图标都只改清单，调用方按名字取，不用动。
#
# Phosphor 画走样的品牌标志从 Font Awesome Free 的 brands 取，来源写 `fa:<名字>`，不带粗细和
# mirror。目前只有 Linux：Phosphor 的企鹅简化成钟形加一张脸，小尺寸认不出来。
#
# 包版本钉死，下载后校验 npm 登记的 sha512，只从压缩包里按名字读文件，不解包到磁盘。
# 清单里没有的 svg 会被删掉，许可证随包更新。
#
# 用法：python3 scripts/vendor-icons.py
# 加图标：清单加一行，跑一遍脚本，再在 crates/smelt-ui/src/icon.rs 的 IconName 表里加一行。
import base64
import hashlib
import io
import pathlib
import sys
import tarfile
import urllib.request

VERSION = "2.1.1"
TARBALL = f"https://registry.npmjs.org/@phosphor-icons/core/-/core-{VERSION}.tgz"
INTEGRITY = "sha512-v4ARvrip4qBCImOE5rmPUylOEK4iiED9ZyKjcvzuezqMaiRASCHKcRIuvvxL/twvLpkfnEODCOJp5dM4eZilxQ=="
FA_VERSION = "6.7.2"
FA_TARBALL = f"https://registry.npmjs.org/@fortawesome/fontawesome-free/-/fontawesome-free-{FA_VERSION}.tgz"
FA_INTEGRITY = "sha512-JUOtgFW6k9u4Y+xeIaEiLr3+cjoUPiAuLXoyKOJSia6Duzb7pq+A76P9ZdPDoAoxHdHzq6gE9/jKBGXlZT8FbA=="
FA_PREFIX = "fa:"
WEIGHTS = {"thin", "light", "regular", "bold", "fill", "duotone"}

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/smelt-ui/assets/phosphor"


def manifest():
    entries = []
    for number, line in enumerate((OUT / "icons.txt").read_text().splitlines(), 1):
        line = line.split("#", 1)[0].split()
        if not line:
            continue
        name, source, *rest = line
        weight = next((word for word in rest if word in WEIGHTS), "bold")
        mirror = "mirror" in rest
        unknown = [word for word in rest if word not in WEIGHTS and word != "mirror"]
        if unknown:
            sys.exit(f"icons.txt:{number}: 不认识 {unknown}")
        if source.startswith(FA_PREFIX) and rest:
            sys.exit(f"icons.txt:{number}: Font Awesome 的图标不带粗细和 mirror")
        entries.append((name, source, weight, mirror))
    names = [name for name, *_ in entries]
    if len(names) != len(set(names)):
        sys.exit("icons.txt 里有重名")
    return entries


def fetch(tarball, integrity):
    request = urllib.request.Request(tarball, headers={"User-Agent": "smelt-vendor-icons"})
    with urllib.request.urlopen(request) as response:
        data = response.read()
    algorithm, expected = integrity.split("-", 1)
    actual = base64.b64encode(hashlib.new(algorithm, data).digest()).decode()
    if actual != expected:
        sys.exit(f"{tarball} 校验不对：{algorithm}-{actual}")
    return tarfile.open(fileobj=io.BytesIO(data), mode="r:gz")


def read(archive, path):
    member = archive.extractfile(f"package/{path}")
    if member is None:
        sys.exit(f"包里没有 {path}")
    return member.read().decode()


def main():
    entries = manifest()
    archive = fetch(TARBALL, INTEGRITY)
    fa_archive = None
    written = set()
    for name, source, weight, mirror in entries:
        if source.startswith(FA_PREFIX):
            fa_archive = fa_archive or fetch(FA_TARBALL, FA_INTEGRITY)
            svg = read(fa_archive, f"svgs/brands/{source.removeprefix(FA_PREFIX)}.svg").strip()
            (OUT / f"{name}.svg").write_text(svg + "\n")
            written.add(f"{name}.svg")
            continue
        suffix = "" if weight == "regular" else f"-{weight}"
        svg = read(archive, f"assets/{weight}/{source}{suffix}.svg").strip()
        if mirror:
            head, _, body = svg.partition(">")
            body = body.removesuffix("</svg>")
            svg = f'{head}><g transform="matrix(-1 0 0 1 256 0)">{body}</g></svg>'
        (OUT / f"{name}.svg").write_text(svg + "\n")
        written.add(f"{name}.svg")
    for stale in OUT.glob("*.svg"):
        if stale.name not in written:
            stale.unlink()
    (OUT / "LICENSE").write_text(read(archive, "LICENSE"))
    fa_license = OUT / "LICENSE-fontawesome.txt"
    if fa_archive:
        fa_license.write_text(read(fa_archive, "LICENSE.txt"))
    else:
        fa_license.unlink(missing_ok=True)
    print(f"Phosphor {VERSION}、Font Awesome {FA_VERSION}：{len(written)} 枚")


if __name__ == "__main__":
    main()
