#!/usr/bin/env bash
# 把正在跑的 smeltd 无缝交接到 ~/.smelt/bin/smeltd（会话 PTY 保留）。
# 用法：handoff-daemon-to-managed.sh [源 smeltd 路径]
# 失败退出非 0；守护没在跑时返回 0（无需 handoff）。
set -euo pipefail

SRC="${1:-}"
MANAGED_DIR="${HOME}/.smelt/bin"
MANAGED="${MANAGED_DIR}/smeltd"
NEXT="${MANAGED_DIR}/smeltd.next"
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

mkdir -p "$MANAGED_DIR"
cp -f "$SRC" "$NEXT"
chmod 755 "$NEXT"

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
  echo "· 守护未运行，已写入 $MANAGED"
  exit 0
fi

# 发 upgrade 到 next
python3 - "$SOCK" "$NEXT" <<'PY'
import json, socket, sys, time, os

sock_path, next_path = sys.argv[1], sys.argv[2]

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

def upgrade(exe: str) -> bool:
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
        return False
    try:
        return json.loads(data.decode().strip()).get("ok") is True
    except Exception:
        return False

before = None
try:
    before = version()
except Exception as e:
    print(f"✗ 读 version 失败：{e}", file=sys.stderr)
    sys.exit(1)

if not upgrade(next_path):
    print("✗ upgrade(smeltd.next) 失败", file=sys.stderr)
    sys.exit(1)

# 等新进程就绪
ok = False
for _ in range(25):
    time.sleep(0.2)
    try:
        v = version()
        # 优先看 exe 路径
        exe = v.get("exe") or ""
        if exe and (exe.endswith("smeltd.next") or exe.endswith("/smelt/bin/smeltd") or "smelt/bin" in exe):
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

# next → managed，再 upgrade 一次让 current_exe 变成正式名
managed = os.path.expanduser("~/.smelt/bin/smeltd")
os.replace(next_path, managed)
if not upgrade(managed):
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
