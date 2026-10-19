#!/usr/bin/env bash
# Smelt 运行时的一键安装（设计文档 D7）。发布成 GitHub Release 里的 install.sh：
#
#   curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash
#   curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop
#
# 按 uname -m 下载对应架构的包，对着同一个 Release 的 SHA256SUMS 核对，解包后把参数原样交给
# 包里的 install.sh（参数见那个脚本）。SMELT_VERSION=0.7.0 装指定版本，不设就是最新版。
#
# 整个脚本包在 main 里：经管道执行时 bash 是边下边读的，下载中断不能只执行半截。
set -euo pipefail

main() {
  local repo=smelt-ai/smelt
  [[ "$(uname -s)" == Linux ]] || { echo "Smelt 运行时只能装在 Linux 上" >&2; exit 1; }
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | aarch64) ;;
    *) echo "不支持的架构：$arch（只有 x86_64 和 aarch64）" >&2; exit 1 ;;
  esac
  local tool
  for tool in curl tar sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || { echo "缺少 $tool" >&2; exit 1; }
  done

  local base="https://github.com/$repo/releases/latest/download"
  [[ -n "${SMELT_VERSION:-}" ]] && base="https://github.com/$repo/releases/download/v$SMELT_VERSION"

  # 不能是 local：EXIT 时 main 已经返回，trap 里还要用它。
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/SHA256SUMS"
  # 文件名带版本号，从校验和清单里按架构认出来，这样 latest 不用另外查版本。
  local name
  name="$(awk -v suffix="-linux-$arch.tar.gz" \
    'substr($2, length($2) - length(suffix) + 1) == suffix { print $2; exit }' "$work/SHA256SUMS")"
  [[ -n "$name" ]] || { echo "这个版本没有 linux-$arch 的包" >&2; exit 1; }

  echo "==> 下载 $name"
  curl -fSL --retry 3 --progress-bar -o "$work/$name" "$base/$name"
  (cd "$work" && grep -F "  $name" SHA256SUMS | sha256sum -c --quiet -) \
    || { echo "$name 校验失败" >&2; exit 1; }
  tar -xzf "$work/$name" -C "$work"
  bash "$work/${name%.tar.gz}/install.sh" "$@"
}

main "$@"
