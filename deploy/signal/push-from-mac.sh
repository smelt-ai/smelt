#!/usr/bin/env bash
# 在 Mac 上跑：下载 Linux 二进制 + 把部署文件 scp 到腾讯云，避免 VPS 拉 GitHub 卡住。
#
# 用法：
#   ./deploy/signal/push-from-mac.sh ubuntu@1.2.3.4
#   DOMAIN=signal.example.com ./deploy/signal/push-from-mac.sh ubuntu@1.2.3.4
#
# 然后 SSH 上机执行脚本打印的那一行 sudo 命令。

set -euo pipefail

HOST="${1:-}"
REPO="${REPO:-smelt-ai/smelt}"
DOMAIN="${DOMAIN:-}"
SSH_OPTS="${SSH_OPTS:-}"

if [[ -z "$HOST" ]]; then
  echo "用法: $0 user@vps-ip" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STAGE="$(mktemp -d /tmp/smelt-signal-push.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT

echo "[$(date +%H:%M:%S)] 1/3 从 GitHub 下 Linux 二进制（在 Mac 上，一般比 VPS 快）…"
curl -fL --connect-timeout 15 --max-time 180 \
  -o "$STAGE/smelt-signal" \
  "https://github.com/${REPO}/releases/download/signal-nightly/smelt-signal-x86_64-unknown-linux-gnu"
chmod +x "$STAGE/smelt-signal"
ls -lh "$STAGE/smelt-signal"

echo "[$(date +%H:%M:%S)] 2/3 打包 deploy 脚本…"
cp "$ROOT/deploy/signal/install.sh" "$STAGE/"
cp "$ROOT/deploy/signal/smelt-signal.service" "$STAGE/"
cp "$ROOT/deploy/signal/smelt-signal.env.example" "$STAGE/"
cp "$ROOT/deploy/signal/Caddyfile" "$STAGE/" 2>/dev/null || true

echo "[$(date +%H:%M:%S)] 3/3 scp → ${HOST}:/tmp/smelt-signal-deploy/ …"
ssh $SSH_OPTS "$HOST" 'mkdir -p /tmp/smelt-signal-deploy'
scp $SSH_OPTS -r "$STAGE"/* "$HOST:/tmp/smelt-signal-deploy/"

echo
echo "已上传。SSH 上机后执行（会逐步打日志）："
echo
if [[ -n "$DOMAIN" ]]; then
  echo "  ssh $HOST"
  echo "  cd /tmp/smelt-signal-deploy"
  echo "  sudo DOMAIN=${DOMAIN} BIN=/tmp/smelt-signal-deploy/smelt-signal TLS=nginx bash install.sh"
else
  echo "  # 先只装进程验证（不需要域名）："
  echo "  ssh $HOST"
  echo "  cd /tmp/smelt-signal-deploy"
  echo "  sudo SKIP_TLS=1 BIN=/tmp/smelt-signal-deploy/smelt-signal bash install.sh"
  echo
  echo "  # 有域名后再上 HTTPS："
  echo "  sudo DOMAIN=signal.你的域名.com BIN=/tmp/smelt-signal-deploy/smelt-signal TLS=nginx bash install.sh"
fi
echo
