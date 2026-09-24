#!/usr/bin/env bash
# 解析出一个可用的 bun，打印其路径。
#
# 优先源码锁定版本的受管 bun：那正是插件在用户机器上被 exec 的版本。拿其它
# bun-v* 或 PATH 上任意版本验 production install，验的不是同一件事。
#
# `--managed-only` 禁止 PATH 回退，供内置 Pi 的 production 检查使用。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_file="$root/crates/smelt-core/src/acp_conn.rs"
version="$(awk -F'"' '/^const BUN_VERSION: &str = "/ { print $2; exit }' "$source_file")"
case "$(uname -m)" in
  arm64) rust_arch="aarch64" ;;
  x86_64) rust_arch="x86_64" ;;
  *) rust_arch="" ;;
esac
expected_sha="$(awk -v arch="$rust_arch" '
  /^#\[cfg\(all\(target_os = "macos", target_arch = "/ {
    matching = arch != "" && index($0, "target_arch = \"" arch "\"") > 0
    next
  }
  matching && /^const BUN_EXECUTABLE_SHA256: &str =/ {
    getline
    gsub(/[";]/, "")
    gsub(/^[[:space:]]+|[[:space:]]+$/, "")
    print
    exit
  }
' "$source_file")"
[[ -n "$version" && -n "$expected_sha" ]] || {
  echo "无法读取当前架构的受管 Bun 锁定信息" >&2
  exit 1
}

candidate="${HOME}/.smelt/runtime/bun-v${version}/bun"
managed_valid=false
if [[ -f "$candidate" && ! -L "$candidate" && -x "$candidate" ]]; then
  actual_sha="$(shasum -a 256 "$candidate" | awk '{ print $1 }')"
  actual_version="$($candidate --version 2>/dev/null || true)"
  if [[ "$actual_sha" == "$expected_sha" && "$actual_version" == "$version" ]]; then
    managed_valid=true
  fi
fi
if $managed_valid; then
  echo "$candidate"
  exit 0
fi

if [[ "${1:-}" == "--managed-only" ]]; then
  echo "锁定版本的受管 Bun 不存在或完整性校验失败：$candidate" >&2
  exit 1
fi

if command -v bun >/dev/null 2>&1; then
  command -v bun
  exit 0
fi

exit 1
