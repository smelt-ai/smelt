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
# 每次安装使用独立暂存名，避免和在线更新或另一条手动安装争用 smeltd.next。
NEXT="${MANAGED_DIR}/smeltd.install.$$.next"
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

mkdir -p "$MANAGED_DIR"
cp -f "$SRC" "$NEXT"
chmod 755 "$NEXT"

# exec 新守护之前必须先写好 daemon-sets 映射。make install 会先 handoff、后
# 启动 GUI；若映射还不存在，smeltd 启动时会认为没有 bundled 插件。
sync_plugin_set_for_candidate "$SRC" "$NEXT"

if [[ "$BACKGROUND_ON_BUSY" == true ]]; then
  marker_tmp="${PENDING_MARKER}.$$.tmp"
  printf '%s\n' "$NEXT" > "$marker_tmp"
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
  mv -f "$NEXT" "$MANAGED"
  if [[ "$BACKGROUND_ON_BUSY" == true ]]; then
    rm -f -- "$PENDING_MARKER"
  fi
  echo "· 守护未运行，已写入 $MANAGED"
  exit 0
fi

# 发 upgrade 到独立暂存文件
python3 - "$SOCK" "$NEXT" "$WAIT_FOR_IDLE" "$BACKGROUND_ON_BUSY" <<'PY'
import json, socket, sys, time, os

sock_path, next_path = sys.argv[1], sys.argv[2]
wait_for_idle = sys.argv[3] == "true"
background_on_busy = sys.argv[4] == "true"
managed = os.path.expanduser("~/.smelt/bin/smeltd")
pending_marker = managed + ".install-pending"
detached = False

if background_on_busy:
    marker_tmp = pending_marker + f".{os.getpid()}.tmp"
    with open(marker_tmp, "w", encoding="utf-8") as marker:
        marker.write(next_path + "\n")
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
            if open(pending_marker, encoding="utf-8").read().strip() != next_path:
                try:
                    os.unlink(next_path)
                except FileNotFoundError:
                    pass
                print("· 安装 handoff 已被更新的构建取代，停止旧重试", flush=True)
                sys.exit(0)
        except FileNotFoundError:
            sys.exit(0)
    result, blockers = upgrade(next_path)
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
            print(f"\n[{time.strftime('%Y-%m-%d %H:%M:%S')}] 后台等待 ACP 安全边界：{next_path}")
            continue
        if not wait_for_idle:
            try:
                os.unlink(next_path)
            except FileNotFoundError:
                pass
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
    print(f"✗ upgrade({os.path.basename(next_path)}) 失败", file=sys.stderr)
    if background_on_busy:
        try:
            if open(pending_marker, encoding="utf-8").read().strip() == next_path:
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

# 新版 daemon 会在初始化前自行 next → managed，并以正式路径继续同一份 handoff。
# 旧版候选没有这段启动收尾：脚本仍替它 rename，再补一次 upgrade 校正进程名。
if background_on_busy:
    try:
        if open(pending_marker, encoding="utf-8").read().strip() != next_path:
            print("· handoff 成功前任务已被新构建取代，保留当前 daemon，不再覆盖 managed")
            sys.exit(0)
    except FileNotFoundError:
        sys.exit(0)
try:
    current = version()
except Exception:
    current = {}
current_exe = current.get("exe") or ""
running_stable = os.path.abspath(current_exe) == os.path.abspath(managed)
if os.path.exists(next_path):
    os.replace(next_path, managed)
elif not (running_stable and os.path.isfile(managed)):
    print("✗ handoff 后暂存文件与正式运行映像均不存在", file=sys.stderr)
    sys.exit(1)
if background_on_busy:
    try:
        os.unlink(pending_marker)
    except FileNotFoundError:
        pass
if not running_stable:
    result, _ = upgrade(managed)
    if result != "ok":
        # 已在 managed inode 上跑，可接受
        print(f"⚠ 二次 upgrade 到 managed 未 ack，文件已在 {managed}")
    else:
        for _ in range(15):
            time.sleep(0.15)
            try:
                v = version()
                exe = v.get("exe") or ""
                if exe.endswith("/.smelt/bin/smeltd") or exe.endswith("/smelt/bin/smeltd"):
                    break
            except Exception:
                pass

print(f"✅ 守护已交接 → {managed}")
try:
    print(json.dumps(version(), ensure_ascii=False))
except Exception:
    pass
PY
