#!/usr/bin/env bash
# 把正在跑的 smeltd 无缝交接到 ~/.smelt/bin/smeltd（会话 PTY 保留）。
# make install 已改走 `smelt --install-app`（与在线更新同一套插件映射 + 交接）。
# 本脚本仅保留手动交接。
# 用法：handoff-daemon-to-managed.sh [--wait|--background-on-busy] [源 smeltd 路径]
# 默认只尝试一次：ACP 正在运行时退出 75，让调用方保留可响应的界面；显式 --wait
# 才会在前台等待安全边界；--background-on-busy 把同一重试放到脱离终端的后台，
# 安装命令立即返回。守护没在跑时返回 0（无需 handoff）。
set -euo pipefail

WAIT_FOR_IDLE=false
BACKGROUND_ON_BUSY=false
while (($#)); do
  case "$1" in
    --wait)
      WAIT_FOR_IDLE=true
      shift
      ;;
    --background-on-busy)
      BACKGROUND_ON_BUSY=true
      shift
      ;;
    -h|--help)
      echo "用法：$(basename "$0") [--wait|--background-on-busy] [源 smeltd 路径]"
      exit 0
      ;;
    *)
      break
      ;;
  esac
done

if [[ "$WAIT_FOR_IDLE" == true && "$BACKGROUND_ON_BUSY" == true ]]; then
  echo "✗ --wait 与 --background-on-busy 不能同时使用" >&2
  exit 2
fi

if (($# > 1)); then
  echo "✗ 参数过多；用法：$(basename "$0") [--wait|--background-on-busy] [源 smeltd 路径]" >&2
  exit 2
fi

SRC="${1:-}"
MANAGED_DIR="${HOME}/.smelt/bin"
MANAGED="${MANAGED_DIR}/smeltd"
PENDING="${MANAGED_DIR}/smeltd.pending"
PENDING_MARKER="${MANAGED}.install-pending"
SOCK="${HOME}/.smelt/smeltd.sock"

if [[ -z "$SRC" ]]; then
  if [[ -x /Applications/Smelt.app/Contents/MacOS/smeltd ]]; then
    SRC=/Applications/Smelt.app/Contents/MacOS/smeltd
  elif [[ -x "$(dirname "$0")/../target/release/smeltd" ]]; then
    SRC="$(cd "$(dirname "$0")/.." && pwd)/target/release/smeltd"
  else
    echo "✗ 未指定源 smeltd 且找不到默认路径" >&2
    exit 1
  fi
fi

if [[ ! -x "$SRC" && ! -f "$SRC" ]]; then
  echo "✗ 源 smeltd 不存在：$SRC" >&2
  exit 1
fi

sync_plugin_set_for_candidate() {
  local src="$1"
  local daemon="$2"
  local macos_dir packages sync_bin smelt_root
  macos_dir="$(cd "$(dirname "$src")" && pwd)"
  packages="$macos_dir/../Resources/plugin-packages"
  sync_bin="$(cd "$(dirname "$0")/.." && pwd)/target/release/smelt-sync-plugins"
  smelt_root="${HOME}/.smelt"
  if [[ ! -x "$sync_bin" ]]; then
    echo "· 跳过插件集同步：找不到 ${sync_bin}（先 make build）" >&2
    return 0
  fi
  if [[ ! -d "$packages" ]]; then
    echo "· 跳过插件集同步：找不到 $packages" >&2
    return 0
  fi
  echo "· 同步 bundled 插件集 → $daemon"
  "$sync_bin" --packages "$packages" --smelt-root "$smelt_root" --daemon "$daemon"
}

# 映像按内容指纹写一次，之后不再改这个文件。启动名只是同目录里的符号链接。
publish_image() {
  local src="$1"
  local hash image tmp existing
  hash="$(shasum -a 256 "$src" | awk '{print $1}')"
  if [[ ! "$hash" =~ ^[0-9a-f]{64}$ ]]; then
    echo "✗ 无法计算映像指纹" >&2
    exit 1
  fi
  image="${MANAGED_DIR}/smeltd-${hash}"
  if [[ -e "$image" || -L "$image" ]]; then
    if [[ -L "$image" || ! -f "$image" ]]; then
      echo "✗ 映像路径不是普通文件：$image" >&2
      exit 1
    fi
    existing="$(shasum -a 256 "$image" | awk '{print $1}')"
    if [[ "$existing" != "$hash" ]]; then
      echo "✗ 映像名与内容不一致，拒绝覆盖：$image" >&2
      exit 1
    fi
  else
    tmp="${MANAGED_DIR}/.smeltd-${hash}.partial.$$"
    cp "$src" "$tmp"
    chmod 755 "$tmp"
    if ln "$tmp" "$image" 2>/dev/null; then
      rm -f -- "$tmp"
    else
      rm -f -- "$tmp"
      if [[ ! -f "$image" || -L "$image" ]]; then
        echo "✗ 写入映像失败：$image" >&2
        exit 1
      fi
      existing="$(shasum -a 256 "$image" | awk '{print $1}')"
      if [[ "$existing" != "$hash" ]]; then
        echo "✗ 映像名与内容不一致，拒绝覆盖：$image" >&2
        exit 1
      fi
    fi
  fi
  printf '%s\n' "$image"
}

link_relative() {
  local name="$1" target="$2"
  local tmp="${MANAGED_DIR}/.${name}.link.$$"
  ln -s "$target" "$tmp"
  mv -f "$tmp" "${MANAGED_DIR}/${name}"
}

path_is_mapped() {
  python3 - "$1" <<'PY'
import ctypes, ctypes.util, os, sys
path = sys.argv[1]
if not os.path.lexists(path):
    sys.exit(1)
lib = ctypes.CDLL(ctypes.util.find_library("c") or "/usr/lib/libSystem.B.dylib")
fn = getattr(lib, "proc_listpidspath", None)
if fn is None:
    sys.exit(1)
fn.argtypes = [
    ctypes.c_uint32, ctypes.c_uint32, ctypes.c_char_p,
    ctypes.c_uint32, ctypes.c_void_p, ctypes.c_int,
]
fn.restype = ctypes.c_int
encoded = os.fsencode(path)
hint = fn(1, 0, encoded, 0, None, 0)
if hint <= 0:
    sys.exit(1)
buf = ctypes.create_string_buffer(hint)
sys.exit(0 if fn(1, 0, encoded, 0, buf, hint) > 0 else 1)
PY
}

mkdir -p "$MANAGED_DIR"
IMAGE="$(publish_image "$SRC")"
IMAGE_NAME="$(basename "$IMAGE")"

# exec 新守护之前必须先写好 daemon-sets 映射。make install 会先 handoff、后
# 启动 GUI；若映射还不存在，smeltd 启动时会认为没有 bundled 插件。
sync_plugin_set_for_candidate "$SRC" "$IMAGE"

if [[ "$BACKGROUND_ON_BUSY" == true ]]; then
  marker_tmp="${PENDING_MARKER}.$$.tmp"
  printf '%s\n' "$IMAGE" > "$marker_tmp"
  mv -f "$marker_tmp" "$PENDING_MARKER"
fi

# 守护没在跑：只落盘 managed，退出 0
if [[ ! -S "$SOCK" ]] || ! python3 - "$SOCK" <<'PY' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(0.5)
try:
    s.connect(sys.argv[1])
except Exception:
    sys.exit(1)
sys.exit(0)
PY
then
  # 套接字连不上不等于进程已经退出。启动名仍被映射就只挂 pending，不拨链接。
  if path_is_mapped "$MANAGED"; then
    link_relative smeltd.pending "$IMAGE_NAME"
    echo "· 守护仍被映射，不改启动名，已挂上 ${PENDING}" >&2
    exit 0
  fi
  link_relative smeltd "$IMAGE_NAME"
  if [[ -L "$PENDING" ]]; then
    rm -f -- "$PENDING"
  fi
  if [[ "$BACKGROUND_ON_BUSY" == true ]]; then
    rm -f -- "$PENDING_MARKER"
  fi
  echo "· 守护未运行，启动名已指向 $IMAGE_NAME"
  exit 0
fi

# 先挂上 pending，再 exec 这份不可变映像。忙的时候留下链接，空闲升级还能找到它。
link_relative smeltd.pending "$IMAGE_NAME"

python3 - "$SOCK" "$IMAGE" "$WAIT_FOR_IDLE" "$BACKGROUND_ON_BUSY" "$IMAGE_NAME" <<'PY'
import json, socket, sys, time, os

sock_path, image_path = sys.argv[1], sys.argv[2]
wait_for_idle = sys.argv[3] == "true"
background_on_busy = sys.argv[4] == "true"
image_name = sys.argv[5]
managed = os.path.expanduser("~/.smelt/bin/smeltd")
pending_marker = managed + ".install-pending"
detached = False

if background_on_busy:
    marker_tmp = pending_marker + f".{os.getpid()}.tmp"
    with open(marker_tmp, "w", encoding="utf-8") as marker:
        marker.write(image_path + "\n")
        marker.flush()
        os.fsync(marker.fileno())
    os.replace(marker_tmp, pending_marker)

def version():
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(5)
    s.connect(sock_path)
    s.sendall(b'{"op":"version"}\n')
    data = b""
    while b"\n" not in data:
        chunk = s.recv(4096)
        if not chunk:
            break
        data += chunk
    s.close()
    return json.loads(data.decode().strip())

def upgrade(exe: str):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(30)
    s.connect(sock_path)
    s.sendall(json.dumps({"op": "upgrade", "exe": exe}).encode() + b"\n")
    data = b""
    while b"\n" not in data:
        chunk = s.recv(4096)
        if not chunk:
            break
        data += chunk
    s.close()
    if not data:
        return "failed", []
    try:
        response = json.loads(data.decode().strip())
    except Exception:
        return "failed", []
    if response.get("busy") is True:
        blockers = response.get("sessions")
        return "busy", blockers if isinstance(blockers, list) else []
    return ("ok", []) if response.get("ok") is True else ("failed", [])

before = None
try:
    before = version()
except Exception as e:
    print(f"✗ 读 version 失败：{e}", file=sys.stderr)
    sys.exit(1)

# ACP 有未完成 RPC 时守护会明确回 busy。默认立刻交回控制权，不能把安装命令卡在
# 一个可能持续数小时的 Agent 回合里；显式 --wait 才轮询。两条路径都绝不覆盖 App，
# 避免出现“安装成功、实际仍跑旧守护”的假升级。
next_wait_note_at = 0.0
while True:
    if detached:
        try:
            if open(pending_marker, encoding="utf-8").read().strip() != image_path:
                print("· 安装 handoff 已被更新的构建取代，停止旧重试", flush=True)
                sys.exit(0)
        except FileNotFoundError:
            sys.exit(0)
    result, blockers = upgrade(image_path)
    if result == "ok":
        break
    if result == "busy":
        if background_on_busy and not detached:
            child = os.fork()
            if child != 0:
                print(f"· ACP 仍在旧版运行时中；已转后台等待并热切换（pid={child}），安装继续。")
                print(f"  日志：{os.path.expanduser('~/.smelt/install-handoff.log')}")
                sys.exit(0)
            os.setsid()
            log_path = os.path.expanduser("~/.smelt/install-handoff.log")
            log = open(log_path, "a", encoding="utf-8", buffering=1)
            os.dup2(log.fileno(), 1)
            os.dup2(log.fileno(), 2)
            detached = True
            wait_for_idle = True
            print(f"\n[{time.strftime('%Y-%m-%d %H:%M:%S')}] 后台等待 ACP 安全边界：{image_path}")
            continue
        if not wait_for_idle:
            detail = f"（{len(blockers)} 个会话）" if blockers else ""
            print(f"⏸ ACP 会话仍在运行{detail}，未替换 App，以保护当前会话。", file=sys.stderr)
            print("  请在当前回合结束后重新执行 make install；需要前台等待时加 --wait。", file=sys.stderr)
            sys.exit(75)
        now = time.monotonic()
        if now >= next_wait_note_at:
            detail = f"（{len(blockers)} 个会话）" if blockers else ""
            print(
                f"· ACP 会话仍在运行{detail}，继续等待安全交接（可按 Ctrl-C 取消安装）…",
                file=sys.stderr,
                flush=True,
            )
            next_wait_note_at = now + 20
        time.sleep(2)
        continue
    print(f"✗ upgrade({os.path.basename(image_path)}) 失败", file=sys.stderr)
    if background_on_busy:
        try:
            if open(pending_marker, encoding="utf-8").read().strip() == image_path:
                os.unlink(pending_marker)
        except FileNotFoundError:
            pass
    sys.exit(1)

# 等新进程就绪
ok = False
for _ in range(25):
    time.sleep(0.2)
    try:
        v = version()
        # 优先看 exe 路径
        exe = v.get("exe") or ""
        if exe and "smelt/bin" in exe:
            ok = True
            break
        # 无 exe 字段：mtime 推进即可
        if v.get("exe_mtime", 0) >= before.get("exe_mtime", 0):
            # 至少 pid 变了或 started_at 变了
            if v.get("pid") != before.get("pid") or v.get("started_at") != before.get("started_at"):
                ok = True
                break
            ok = True
            break
    except Exception:
        continue

if not ok:
    print("✗ handoff 后守护未就绪", file=sys.stderr)
    sys.exit(1)

# 提交已经完成。只把启动名拨到这份映像，不挪动映像文件本身。
if background_on_busy:
    try:
        if open(pending_marker, encoding="utf-8").read().strip() != image_path:
            print("· handoff 成功前任务已被新构建取代，保留当前 daemon，不再改启动名")
            sys.exit(0)
    except FileNotFoundError:
        sys.exit(0)
managed_dir = os.path.dirname(managed)
link_tmp = os.path.join(managed_dir, f".smeltd.link.{os.getpid()}")
os.symlink(image_name, link_tmp)
os.replace(link_tmp, managed)
pending = os.path.join(managed_dir, "smeltd.pending")
if os.path.islink(pending):
    os.unlink(pending)
if background_on_busy:
    try:
        os.unlink(pending_marker)
    except FileNotFoundError:
        pass

print(f"✅ 守护已交接 → {managed}（{image_name}）")
try:
    print(json.dumps(version(), ensure_ascii=False))
except Exception:
    pass
PY
