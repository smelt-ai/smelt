#!/usr/bin/env bash
# 把完整 .app 复制到 /Applications 同卷暂存目录，再用 renamex_np(RENAME_SWAP)
# 原子交换。运行中的旧 GUI/daemon 继续引用旧 vnode，不会被逐文件覆盖破坏签名。
set -euo pipefail

SOURCE="${1:-}"
TARGET="${2:-/Applications/Smelt.app}"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "✗ 原子 App 安装仅支持 macOS" >&2
  exit 1
fi
if [[ -z "$SOURCE" || ! -d "$SOURCE" ]]; then
  echo "✗ App 源目录不存在：${SOURCE:-<空>}" >&2
  exit 1
fi
if [[ "${SOURCE##*.}" != "app" || "$TARGET" != /Applications/*.app ]]; then
  echo "✗ 拒绝非 .app 或 /Applications 之外的安装目标：$TARGET" >&2
  exit 2
fi
for executable in smelt smeltd smelt-notify smelt-agent-mcp; do
  if [[ ! -x "$SOURCE/Contents/MacOS/$executable" ]]; then
    echo "✗ App 缺少可执行文件：Contents/MacOS/$executable" >&2
    exit 1
  fi
done
if [[ ! -f "$SOURCE/Contents/Info.plist" ]]; then
  echo "✗ App 缺少 Contents/Info.plist" >&2
  exit 1
fi

target_name="$(basename "$TARGET" .app)"
stage="/Applications/.${target_name}.smelt-local-$(date +%s)-$$.app"
swapped=false
cleanup_uninstalled_stage() {
  status=$?
  trap - EXIT INT TERM
  if [[ "$swapped" == false && -d "$stage" ]]; then
    rm -rf -- "$stage"
  fi
  exit "$status"
}
trap cleanup_uninstalled_stage EXIT INT TERM

# 清理之前原子交换留下、且已没有任何进程引用的旧 App。lsof 不可用或探测
# 出错时宁可保留；路径还要再次匹配固定前缀，绝不扩大删除范围。
if command -v lsof >/dev/null 2>&1; then
  for old in "/Applications/.${target_name}.smelt-local-"*.app; do
    [[ -d "$old" ]] || continue
    case "$old" in
      "/Applications/.${target_name}.smelt-local-"*.app) ;;
      *) continue ;;
    esac
    if ! lsof +D "$old" >/dev/null 2>&1; then
      rm -rf -- "$old"
    fi
  done
fi

/usr/bin/ditto "$SOURCE" "$stage"

if [[ -e "$TARGET" ]]; then
  python3 - "$stage" "$TARGET" <<'PY'
import ctypes
import os
import sys

left, right = (os.fsencode(path) for path in sys.argv[1:3])
libc = ctypes.CDLL(None, use_errno=True)
renamex_np = libc.renamex_np
renamex_np.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
renamex_np.restype = ctypes.c_int
RENAME_SWAP = 0x00000002
if renamex_np(left, right, RENAME_SWAP) != 0:
    error = ctypes.get_errno()
    raise OSError(error, os.strerror(error), sys.argv[2])
PY
  swapped=true
  # 交换后 stage 是完整旧包。运行中的旧 App 可能仍映射它，保留到下一次安装
  # 确认无进程引用后再清理。
  echo "· 旧 App 暂存于 ${stage}（退出旧进程后自动在下次安装清理）"
else
  mv "$stage" "$TARGET"
  swapped=true
fi
echo "✅ 已原子安装 $TARGET"
