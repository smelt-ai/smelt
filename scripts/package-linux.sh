#!/usr/bin/env bash
# 打 Linux 无头运行时（smeltd）的发布包：dist/smeltd-<版本>-linux-<架构>.tar.gz。
# 由 release.yml 在两种架构的 runner 上各跑一次。
# 包里还有 smelt-pi、photon wasm，以及 pi-tui 的 native/。Bun 不打进包，第一次用到 MCP 或 bunx 时再下载。
#
# 不交叉编译：在哪种架构的 Linux 机器上跑，就出哪种架构的包。只编运行时这一侧：
# 桌面 GUI 那几个 crate 依赖 GPUI，只有 macOS 版。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

[[ "$(uname -s)" == Linux ]] || { echo "✗ 只在 Linux 上打包，当前是 $(uname -s)" >&2; exit 1; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) echo "✗ 不支持的架构：$(uname -m)" >&2; exit 1 ;;
esac

# 随运行时一起分发的程序。
PROGRAMS=(smeltd smelt-notify)

version="$(awk -F'"' '/^\[workspace.package\]/{inside=1; next} /^\[/{inside=0} inside && /^version *=/{print $2; exit}' Cargo.toml)"
[[ -n "$version" ]] || { echo "✗ 读不到工作区版本号" >&2; exit 1; }
name="smeltd-${version}-linux-$(uname -m)"
echo "▶ 打包 ${name}"
cargo build --release -p smeltd --bins --locked
make smelt-pi
bin="target/smelt-pi/smelt-pi"
wasm="target/smelt-pi/photon_rs_bg.wasm"
if [[ ! -x "$bin" || ! -f "$wasm" ]]; then
  echo "✗ 缺少 smelt-pi 或 photon_rs_bg.wasm" >&2
  exit 1
fi
newer="$(find packages/pi-agent/src packages/pi-agent/package.json packages/pi-agent/bun.lock \
  scripts/compile-smelt-pi.ts scripts/build-smelt-pi.sh scripts/fetch-bun.sh \
  -type f -newer "$bin" -print -quit || true)"
if [[ -n "$newer" ]]; then
  echo "✗ smelt-pi 比源码旧：$newer" >&2
  exit 1
fi

staging="dist/$name"
rm -rf "$staging" "dist/$name.tar.gz"
mkdir -p "$staging"
for program in "${PROGRAMS[@]}"; do
  install -m 0755 "target/release/$program" "$staging/$program"
done
install -m 0755 scripts/install-smeltd.sh "$staging/install.sh"
install -m 0755 scripts/smelt-up.sh "$staging/smelt-up"
install -m 0755 "$bin" "$staging/smelt-pi"
install -m 0644 "$wasm" "$staging/photon_rs_bg.wasm"
if [[ -d target/smelt-pi/native ]]; then
  cp -R target/smelt-pi/native "$staging/native"
fi
# 远程桌面（install.sh --with-desktop）的启动脚本、XFCE 设置、Chrome 策略。
cp -R scripts/desktop "$staging/desktop"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "$staging"
echo "✓ 产物：dist/$name.tar.gz"
