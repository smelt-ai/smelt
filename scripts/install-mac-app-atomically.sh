#!/usr/bin/env bash
# 把完整 .app 复制到 /Applications 同卷暂存目录，再用 renamex_np(RENAME_SWAP)
# 原子交换。运行中的旧 GUI/daemon 继续引用旧 vnode，不会被逐文件覆盖破坏签名。
#
# 交换后的旧包留在 /Applications/.<名字>.smelt-local.XXXXXX/。本次安装持有排他锁，
# 然后只删除 lsof 能证明没人打开的那些事务目录。证明不了就留下，下次安装再试。
# 清理失败不改变本次安装的成功结果。不碰在线更新的 .smelt-update-* 路径。
set -euo pipefail

# 与 crates/smelt-core/src/updater.rs 的 bundle_use_state 同一条约定：
# 退出码 0 = 至少有一个打开项；退出码 1 且两端都无输出 = 没人用；其余都不能证明安全。
classify_lsof_outputs() {
  local code="$1"
  local stdout="$2"
  local stderr="$3"
  if [[ "$code" -eq 0 ]]; then
    printf 'in-use\n'
    return 0
  fi
  if [[ "$code" -eq 1 && ! -s "$stdout" && ! -s "$stderr" ]]; then
    printf 'unused\n'
    return 0
  fi
  printf 'unknown\n'
}

probe_dir() {
  local dir="$1"
  local stdout stderr code
  stdout="$(mktemp)"
  stderr="$(mktemp)"
  set +e
  "${SMELT_LSOF:-/usr/sbin/lsof}" -Fn +D "$dir" >"$stdout" 2>"$stderr"
  code=$?
  set -e
  classify_lsof_outputs "$code" "$stdout" "$stderr"
  rm -f "$stdout" "$stderr"
}

is_local_transaction_dir() {
  local parent="$1"
  local target_name="$2"
  local dir="$3"
  local base
  base="$(basename "$dir")"
  # 只认 mktemp 的 6 位后缀。安装锁是 .smelt-local.lock，在线更新是 .smelt-update-*。
  case "$base" in
    ."${target_name}".smelt-local.[A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9]) ;;
    *) return 1 ;;
  esac
  [[ "$(dirname "$dir")" == "$parent" ]] || return 1
  [[ -L "$dir" ]] && return 1
  [[ -d "$dir" ]] || return 1
  return 0
}

gc_retired_local_apps() {
  local parent="$1"
  local target_name="$2"
  local dir state nullglob_was=0
  shopt -q nullglob && nullglob_was=1
  shopt -s nullglob
  for dir in "$parent"/."${target_name}".smelt-local.*; do
    if ! is_local_transaction_dir "$parent" "$target_name" "$dir"; then
      continue
    fi
    state="$(probe_dir "$dir")" || state=unknown
    case "$state" in
      unused)
        if rm -rf -- "$dir"; then
          echo "· 已回收无人使用的旧 App：$dir"
        else
          echo "⚠ 无法删除旧 App：$dir" >&2
        fi
        ;;
      in-use)
        echo "· 旧 App 仍被使用，保留 $dir"
        ;;
      *)
        echo "· 无法证明旧 App 已空闲，保留 $dir"
        ;;
    esac
  done
  if [[ "$nullglob_was" -eq 0 ]]; then
    shopt -u nullglob
  fi
  return 0
}

# 锁必须由本进程持有到退出。python 抢到锁后 exec 回本脚本，fd 9 保持打开。
hold_install_lock_or_reexec() {
  local lock_file="$1"
  shift
  if [[ -n "${SMELT_LOCAL_INSTALL_LOCK_HELD:-}" ]]; then
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "✗ 需要 python3 来持有安装锁" >&2
    exit 1
  fi
  export SMELT_LOCAL_INSTALL_LOCK_HELD=1
  exec python3 - "$lock_file" "$0" "$@" <<'PY'
import fcntl
import os
import sys

lock_path, script, *rest = sys.argv[1:]
try:
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
except OSError as error:
    sys.stderr.write(f"✗ 无法创建安装锁 {lock_path}：{error}\n")
    sys.exit(1)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    sys.stderr.write(f"✗ 已有本地安装在进行：{lock_path}\n")
    sys.exit(75)
os.set_inheritable(fd, True)
if fd != 9:
    os.dup2(fd, 9)
    os.close(fd)
os.set_inheritable(9, True)
os.execv("/bin/bash", ["bash", script, *rest])
PY
}

main() {
  local SOURCE="${1:-}"
  local TARGET="${2:-/Applications/Smelt.app}"

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

  local target_name parent
  target_name="$(basename "$TARGET" .app)"
  parent="$(dirname "$TARGET")"
  # fd 9 持有安装锁。不要关闭或重定向它，进程退出时锁才释放。
  hold_install_lock_or_reexec "$parent/.${target_name}.smelt-local.lock" "$SOURCE" "$TARGET"

  # 这两个变量给 EXIT trap 用，必须是全局的。macOS 的 bash 3.2 在 shell 退出时才跑 EXIT trap，函数局部变量那时已经没了。
  transaction_dir="$(mktemp -d "$parent/.${target_name}.smelt-local.XXXXXX")"
  stage="$transaction_dir/${target_name}.app"
  swapped=false
  cleanup_uninstalled_stage() {
    status=$?
    trap - EXIT INT TERM
    if [[ "$swapped" == false && -d "$transaction_dir" ]]; then
      rm -rf -- "$transaction_dir"
    fi
    exit "$status"
  }
  trap cleanup_uninstalled_stage EXIT INT TERM

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
  else
    mv "$stage" "$TARGET"
    swapped=true
    # App 已提交后，空事务目录清理失败不能把成功安装重新报告成失败。
    rmdir "$transaction_dir" || echo "⚠ 无法清理空事务目录：$transaction_dir" >&2
  fi

  # 安装已经提交。回收失败只留下旧包，不把本次安装报成失败。
  gc_retired_local_apps "$parent" "$target_name" \
    || echo "⚠ 回收旧 App 时出错，安装结果保留" >&2
  echo "✅ 已原子安装 $TARGET"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
