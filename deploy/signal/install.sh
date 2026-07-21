#!/usr/bin/env bash
# 腾讯云 / 国内 VPS 部署 smelt-signal（每步打时间戳，curl 带超时，卡在哪一目了然）
#
# 用法：
#   # 国内 VPS 默认走 GitHub 镜像拉二进制（可直接网页终端跑）
#   sudo SKIP_TLS=1 bash install.sh
#   sudo DOMAIN=signal.example.com bash install.sh
#
#   # 已有二进制
#   sudo DOMAIN=signal.example.com BIN=/tmp/smelt-signal bash install.sh
#
# 环境变量：
#   DOMAIN     上 HTTPS 时必填，例如 signal.example.com
#   BIN        可选，已有二进制路径（设置后不下载）
#   REPO       默认 smelt-ai/smelt
#   BRANCH     默认 feat/webrtc-edge
#   TLS        nginx|caddy|none  默认 nginx
#   SKIP_TLS   1 = 只装进程
#   GH_MIRROR  镜像前缀，默认 https://ghfast.top/
#              空字符串 "" = 直连 GitHub（海外机）
#              也可换：https://ghproxy.net/  https://mirror.ghproxy.com/

set -euo pipefail

DOMAIN="${DOMAIN:-}"
BIN="${BIN:-}"
REPO="${REPO:-smelt-ai/smelt}"
BRANCH="${BRANCH:-feat/webrtc-edge}"
TLS="${TLS:-nginx}"
SKIP_TLS="${SKIP_TLS:-0}"
# 国内默认镜像；海外可 GH_MIRROR= sudo ...
GH_MIRROR="${GH_MIRROR-https://ghfast.top/}"

ORIGIN_RELEASE="https://github.com/${REPO}/releases/download/signal-nightly"
ORIGIN_RAW="https://raw.githubusercontent.com/${REPO}/${BRANCH}/deploy/signal"

log()  { printf '\n[%s] >>> %s\n' "$(date '+%H:%M:%S')" "$*"; }
ok()   { printf '[%s]     OK %s\n' "$(date '+%H:%M:%S')" "$*"; }
die()  { printf '[%s] !!! %s\n' "$(date '+%H:%M:%S')" "$*" >&2; exit 1; }

# 给 GitHub/raw URL 套镜像；已是镜像或非 github 则原样
mirror_url() {
  local url="$1"
  if [[ -z "${GH_MIRROR}" ]]; then
    printf '%s' "$url"
    return
  fi
  case "$url" in
    https://github.com/*|https://raw.githubusercontent.com/*|http://github.com/*)
      # 避免重复套
      if [[ "$url" == "${GH_MIRROR}"* ]]; then
        printf '%s' "$url"
      else
        printf '%s%s' "${GH_MIRROR}" "$url"
      fi
      ;;
    *)
      printf '%s' "$url"
      ;;
  esac
}

# 多地址尝试：镜像 → 备用镜像 → 直连
fetch() {
  local path_or_url="$1" out="$2" step="$3"
  local candidates=()
  local u

  if [[ "$path_or_url" == https://* ]] || [[ "$path_or_url" == http://* ]]; then
    candidates+=("$(mirror_url "$path_or_url")")
    # 备用镜像（主镜像挂了时）
    if [[ -n "${GH_MIRROR}" ]]; then
      candidates+=("https://ghproxy.net/${path_or_url}")
      candidates+=("https://mirror.ghproxy.com/${path_or_url}")
      candidates+=("$path_or_url") # 最后直连碰运气
    fi
  else
    candidates+=("$path_or_url")
  fi

  # 去重
  local tried=()
  for u in "${candidates[@]}"; do
    local seen=0 t
    for t in "${tried[@]+"${tried[@]}"}"; do
      [[ "$t" == "$u" ]] && seen=1 && break
    done
    [[ "$seen" -eq 1 ]] && continue
    tried+=("$u")

    log "下载 ($step): $u"
    if curl -fL --connect-timeout 15 --max-time 180 --retry 1 \
        -o "$out" "$u"; then
      ok "已保存 $out ($(wc -c <"$out") bytes)"
      return 0
    fi
    log "该源失败，试下一个…"
  done
  die "下载失败 ($step)。可换: GH_MIRROR=https://ghproxy.net/ 或在 Mac 下好后 BIN=/path"
}

need_root() {
  [[ "$(id -u)" -eq 0 ]] || die "请用 sudo 运行"
}

step_bin() {
  log "步骤 1/6：安装 smelt-signal 二进制"
  local dest=/usr/local/bin/smelt-signal
  if [[ -n "$BIN" ]]; then
    [[ -f "$BIN" ]] || die "BIN 文件不存在: $BIN"
    install -m 755 "$BIN" "$dest"
    ok "从 BIN=$BIN 安装"
  else
    local tmp
    tmp="$(mktemp)"
    fetch "${ORIGIN_RELEASE}/smelt-signal-x86_64-unknown-linux-gnu" "$tmp" "signal binary"
    install -m 755 "$tmp" "$dest"
    rm -f "$tmp"
  fi
  "$dest" --version 2>/dev/null || true
  # 无 --version 时至少 file 一下
  file "$dest" || true
  ok "$dest"
}

step_config() {
  log "步骤 2/6：写入 /etc/smelt + systemd unit"
  mkdir -p /etc/smelt
  if [[ -f "$(dirname "$0")/smelt-signal.env.example" ]]; then
    cp "$(dirname "$0")/smelt-signal.env.example" /etc/smelt/smelt-signal.env
    cp "$(dirname "$0")/smelt-signal.service" /etc/systemd/system/smelt-signal.service
    ok "从本地 deploy/signal 复制配置"
  else
    fetch "${ORIGIN_RAW}/smelt-signal.env.example" /etc/smelt/smelt-signal.env "env"
    fetch "${ORIGIN_RAW}/smelt-signal.service" /etc/systemd/system/smelt-signal.service "unit"
  fi
  systemctl daemon-reload
  systemctl enable smelt-signal
  systemctl restart smelt-signal
  sleep 0.5
  if ! systemctl is-active --quiet smelt-signal; then
    journalctl -u smelt-signal -n 30 --no-pager || true
    die "smelt-signal 未能启动，见上面 journal"
  fi
  ok "systemd active"
}

step_health_local() {
  log "步骤 3/6：本机 health"
  local body
  body="$(curl -fsS --connect-timeout 3 --max-time 5 http://127.0.0.1:7878/health)" || {
    journalctl -u smelt-signal -n 30 --no-pager || true
    die "本机 7878 无响应"
  }
  ok "health = $body"
}

step_tls_nginx() {
  log "步骤 4/6：安装 nginx + certbot（apt，走系统源，通常比 Caddy 快）"
  export DEBIAN_FRONTEND=noninteractive
  log "apt-get update …"
  apt-get update -y
  ok "apt update 完成"
  log "apt-get install nginx certbot python3-certbot-nginx …"
  apt-get install -y nginx certbot python3-certbot-nginx
  ok "包安装完成"

  log "写 nginx site: $DOMAIN"
  cat >/etc/nginx/sites-available/smelt-signal <<EOF
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};

    location / {
        proxy_pass http://127.0.0.1:7878;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        # WebSocket
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }
}
EOF
  ln -sfn /etc/nginx/sites-available/smelt-signal /etc/nginx/sites-enabled/smelt-signal
  rm -f /etc/nginx/sites-enabled/default
  nginx -t
  systemctl enable --now nginx
  systemctl reload nginx
  ok "nginx 已 reload（HTTP）"

  log "步骤 5/6：certbot 申请证书（需 80 对公网开放 + 域名已解析）"
  if ! certbot --nginx -d "$DOMAIN" --non-interactive --agree-tos \
      --register-unsafely-without-email --redirect; then
    die "certbot 失败。检查：1) 域名 A 记录 2) 安全组 80/443 3) journalctl -u nginx"
  fi
  ok "HTTPS 就绪"
}

step_tls_caddy() {
  log "步骤 4/6：安装 Caddy（国外源，国内可能很慢；更建议 TLS=nginx）"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -y
  apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl
  log "拉 Caddy apt 源 key（可能卡住）…"
  curl -1sLf --connect-timeout 15 --max-time 60 \
    'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
    | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf --connect-timeout 15 --max-time 60 \
    'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
    | tee /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -y
  apt-get install -y caddy
  cat >/etc/caddy/Caddyfile <<EOF
${DOMAIN} {
	encode gzip
	reverse_proxy 127.0.0.1:7878
}
EOF
  systemctl enable --now caddy
  systemctl reload caddy
  ok "Caddy 已配置"
}

step_public() {
  log "步骤 6/6：公网探活 https://${DOMAIN}/health"
  local body
  body="$(curl -fsS --connect-timeout 10 --max-time 20 "https://${DOMAIN}/health")" || {
    die "公网 HTTPS 失败。本机已 OK 的话查安全组 443 / DNS"
  }
  ok "公网 health = $body"
  log "建房试一下"
  curl -fsS --connect-timeout 10 --max-time 20 \
    -X POST "https://${DOMAIN}/v1/rooms" \
    -H 'content-type: application/json' -d '{}' || true
  echo
  ok "完成。信令 WSS：wss://${DOMAIN}/ws"
}

main() {
  need_root
  log "smelt-signal 安装开始 DOMAIN=${DOMAIN:-'(空)'} TLS=$TLS BIN=${BIN:-'(将下载)'}"

  step_bin
  step_config
  step_health_local

  if [[ "$SKIP_TLS" == "1" ]]; then
    log "SKIP_TLS=1，跳过反代。本机已可用：http://127.0.0.1:7878/health"
    exit 0
  fi

  [[ -n "$DOMAIN" ]] || die "请设置 DOMAIN=你的域名（或 SKIP_TLS=1 只装进程）"

  case "$TLS" in
    nginx) step_tls_nginx; step_public ;;
    caddy) step_tls_caddy; step_public ;;
    none)
      log "TLS=none，只装进程"
      ;;
    *) die "未知 TLS=$TLS（nginx|caddy|none）" ;;
  esac
}

main "$@"
