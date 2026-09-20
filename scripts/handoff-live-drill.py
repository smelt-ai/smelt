#!/usr/bin/env python3
"""smeltd v2 handoff 真机演练（终端会话 + 真 fork-exec）。

覆盖单测明确排除的部分：真 spawn successor、真 SCM_RIGHTS 跨进程、
真重定父、COMMIT/回滚、指纹上报。ACP 恢复语义由回环单测
（handoff_v2_tests::import_loopback_restores_hosted_acp_mid_turn）覆盖；
带真实 provider 的 ACP 活体需 API 凭证，留手动 QA。

全程 SMELT_HOME 隔离（mkdtemp），不碰用户真守护。退出码 0=全过。

用法：
    cargo build -p smeltd
    ./scripts/handoff-live-drill.py [path/to/smeltd]
"""
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

WORK = tempfile.mkdtemp(prefix="smeltd-handoff-drill-")
HOME = os.path.join(WORK, "smelt")
SOCK = os.path.join(HOME, "smeltd.sock")
ORIG = os.path.join(WORK, "smeltd-orig")
STAGED = os.path.join(WORK, "smeltd.next")
PROMOTED = os.path.join(WORK, "smeltd")
MARKER = "MARKER-HANDOFF-V2-DRILL"


def find_binary():
    if len(sys.argv) > 1:
        return sys.argv[1]
    candidates = []
    target_dir = os.environ.get("CARGO_TARGET_DIR")
    if target_dir:
        candidates.append(os.path.join(target_dir, "debug", "smeltd"))
    here = os.path.dirname(os.path.abspath(__file__))
    candidates.append(os.path.join(here, "..", "target", "debug", "smeltd"))
    for path in candidates:
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    raise RuntimeError("找不到 smeltd 二进制：先 cargo build -p smeltd，或传路径参数")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def rpc(payload, timeout=30):
    """短 op：一行 JSON 请求 + 一行 JSON 回包。"""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCK)
    s.sendall((json.dumps(payload) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf.decode().split("\n")[0])


def watch_snapshot(session_id, timeout=10):
    """watch 流：读快照（含回放字节），不断言流格式细节，只收字节验 marker。"""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCK)
    s.sendall((json.dumps({"op": "watch", "id": session_id}) + "\n").encode())
    data = b""
    deadline = time.time() + timeout
    try:
        while time.time() < deadline:
            try:
                chunk = s.recv(65536)
            except socket.timeout:
                break
            if not chunk:
                break
            data += chunk
            if MARKER.encode() in data:
                break
    finally:
        s.close()
    return data


def wait_sock(timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(SOCK):
            try:
                rpc({"op": "version"}, timeout=5)
                return
            except OSError:
                pass
        time.sleep(0.2)
    raise RuntimeError("daemon 未就绪")


def main():
    os.makedirs(HOME, exist_ok=True)
    shutil.copy2(find_binary(), ORIG)

    env = dict(os.environ, SMELT_HOME=HOME)
    # 1. 起前任
    pred = subprocess.Popen(
        [ORIG], env=env,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        wait_sock()
        v1 = rpc({"op": "version"})
        pid1, fp1 = v1["pid"], v1.get("daemon_fingerprint")
        assert pid1 == pred.pid, f"version pid {pid1} != spawn {pred.pid}"
        assert fp1 == sha256_file(ORIG), "启动指纹必须等于二进制哈希"
        print(f"1. 前任就绪 pid={pid1} fp={fp1[:12]}...")

        # 2. 开终端 + 喂 marker
        rpc({"op": "open", "id": "drill-t1", "cwd": "/tmp", "cols": 120, "rows": 30},
            timeout=10)
        time.sleep(1.0)  # 等 shell 起
        r = rpc({"op": "input", "id": "drill-t1", "data": f"echo {MARKER}\n"})
        assert r.get("ok"), f"input 失败: {r}"
        deadline = time.time() + 10
        while time.time() < deadline:
            if MARKER.encode() in watch_snapshot("drill-t1", timeout=5):
                break
        else:
            raise RuntimeError("升级前 marker 未出现在快照")
        print("2. 终端会话就绪，marker 已落格")

        # 3. 回滚演练：坏 exe 不得伤守护
        r = rpc({"op": "upgrade", "exe": "/nonexistent/smeltd"})
        assert r.get("ok") is False and "err" in r, f"坏 exe 应干净失败: {r}"
        assert rpc({"op": "version"})["pid"] == pid1, "回滚后前任必须还在"
        print("3. 回滚干净（坏 exe 不伤守护）")

        # 4. 真升级：staged 二进制 handoff
        shutil.copy2(ORIG, STAGED)
        # 让内容不同以验证指纹切换：补一个字节再截掉？不——改内容会坏签名；
        # dev 二进制无签名，直接 append 再验证哈希变化即可。
        with open(STAGED, "ab") as f:
            f.write(b"\0")
        fp_staged = sha256_file(STAGED)
        assert fp_staged != fp1
        r = rpc({"op": "upgrade", "exe": STAGED}, timeout=30)
        assert r.get("ok") is True, f"upgrade 失败: {r}"
        print("4. upgrade 回 ok，待 successor 接管...")
        time.sleep(2.0)
        v2 = rpc({"op": "version"})
        pid2, fp2 = v2["pid"], v2.get("daemon_fingerprint")
        assert pid2 != pid1, "pid 必须变化（fork-exec 新进程）"
        assert fp2 == fp_staged, f"新指纹 {fp2} != staged {fp_staged}"
        assert os.path.exists(PROMOTED), ".next 应已扶正"
        # 前任应已退出：用 poll() 判（会 reap 僵尸；kill(pid,0) 对僵尸也成功）。
        deadline = time.time() + 10
        while pred.poll() is None and time.time() < deadline:
            time.sleep(0.1)
        assert pred.poll() is not None, "前任 COMMIT 后必须退出"
        assert pred.returncode == 0, f"前任应 exit(0)，实际 {pred.returncode}"
        print(f"5. 接管成功 pid={pid2} fp={fp2[:12]}...（前任已退）")

        # 6. 会话存活：grid 里还有 marker
        data = watch_snapshot("drill-t1", timeout=10)
        assert MARKER.encode() in data, "交接后 grid 必须保留 marker"
        print("6. 终端会话存活，grid 内容完整")

        # 7. 升级后守护仍可服务：再开一个会话
        rpc({"op": "open", "id": "drill-t2", "cwd": "/tmp", "cols": 80, "rows": 24},
            timeout=10)
        print("7. 新守护可正常开会话")
        print("ALL GREEN")
    finally:
        try:
            rpc({"op": "shutdown"}, timeout=10)
        except OSError:
            pass
        time.sleep(0.5)
        # 兜底：杀残留（pattern 限定本轮 WORK 目录，不伤其它进程）
        subprocess.run(["pkill", "-f", os.path.join(WORK, "smeltd-orig")],
                       capture_output=True)
        subprocess.run(["pkill", "-f", PROMOTED + "$"], capture_output=True)
        shutil.rmtree(WORK, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
