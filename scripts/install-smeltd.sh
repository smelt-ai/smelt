#!/usr/bin/env bash
# 在一台 Linux 机器上装 Smelt 运行时（smeltd），可选装远程桌面和常驻浏览器（设计文档 D7、
# D9）。随 tar.gz 一起分发，解包后在包目录里执行；一键安装（scripts/get-smeltd.sh，发布成
# Release 里的 install.sh）下载、校验这个包之后调的也是它。重复执行就是升级。
#
#   ./install.sh                  装好、启动，打印配对串
#   ./install.sh --with-desktop   另装远程桌面（Xvnc + XFCE）和常驻 Chrome，Pi 接上这个 Chrome
#   ./install.sh --no-pair        不打印配对串
#
# 装在谁名下：运行时不用 root 跑。以 root 执行时（容器里常见），装完系统包后建一个普通用户
# （默认 smelt，SMELT_USER 可改），运行时和桌面都装在它名下；以普通用户执行时装在自己名下，
# 装系统包、开 linger 时用 sudo。
#
# 怎么常驻：有 systemd 的机器装成 systemd 用户服务（smeltd、smelt-display、smelt-desktop、
# smelt-chrome），开 linger 让它们开机就起、退出登录也不停。没有 systemd 的（多半是 Docker
# 容器）由 ~/.smelt/bin/smelt-up 拉起并看着，装完在后台起一份。
#
# 系统包只支持 apt（Debian 12+、Ubuntu 22.04+）。模型和密钥不在这里配：装完在桌面的能力库里
# 配（D7）。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROGRAMS=(smeltd smelt-notify)
# curl、unzip：按需下载到 ~/.smelt/runtime 的 Bun 要用（MCP 预设的 `bun x`、Claude/Codex 的 `bunx`）。
BASE_PACKAGES=(ca-certificates curl unzip)
# D9 实测过的一套；不装中文字体，中文网页全是方块。装时不带推荐包（容器里能省几百 MB），
# 所以 Xvnc 要的 X 核心字体（fixed）和任务栏图标的主题要自己列上。
DESKTOP_PACKAGES=(
  tigervnc-standalone-server xauth xfonts-base xfce4 xfce4-terminal adwaita-icon-theme
  dbus-x11 xdotool fonts-noto-cjk
)
DESKTOP_UNITS=(smelt-display.service smelt-desktop.service smelt-chrome.service)

WITH_DESKTOP=0
PAIR=1
# 内部用：root 那部分经 sudo 单独跑（system），装到用户名下那部分经 runuser 单独跑（user）。
PHASE=all

usage() {
  echo "用法：$0 [--with-desktop] [--no-pair]" >&2
}

for arg in "$@"; do
  case "$arg" in
    --with-desktop) WITH_DESKTOP=1 ;;
    --no-pair) PAIR=0 ;;
    --phase=system | --phase=user) PHASE="${arg#--phase=}" ;;
    *) usage; exit 2 ;;
  esac
done

FLAGS=()
((WITH_DESKTOP)) && FLAGS+=(--with-desktop)
((PAIR)) || FLAGS+=(--no-pair)

say() { echo "==> $*"; }

# 新开的机器上常有自动更新或初始化脚本正在跑 apt，占着 dpkg 的锁。等它放锁（最多 10 分钟），
# 不要一见锁就失败。
apt_get() { apt-get -o DPkg::Lock::Timeout=600 "$@"; }
warn() { echo "install.sh：$*" >&2; }
die() { warn "$*"; exit 1; }

# 同 sd_booted(3)。容器里一般不是：装了 systemctl 也连不上 systemd。
has_systemd() { [[ -d /run/systemd/system ]]; }

# 列出还没装的系统包；没有 dpkg 的发行版什么都不列。
missing_packages() {
  command -v dpkg-query >/dev/null 2>&1 || return 0
  local package
  for package in "$@"; do
    dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed' \
      || echo "$package"
  done
}

wanted_packages() {
  echo "${BASE_PACKAGES[@]}"
  ((WITH_DESKTOP)) && echo "${DESKTOP_PACKAGES[@]}"
  return 0
}

# ── 系统部分（root）──────────────────────────────────────────────────────────

# Google 不出 Linux arm64 的 Chrome，arm64 用发行版的 Chromium。Ubuntu 的 chromium 只是个装
# snap 的空壳，容器里用不了，所以 arm64 只支持 Debian。
install_browser() {
  local policy_dir
  case "$(uname -m)" in
    x86_64)
      if [[ -n "$(missing_packages google-chrome-stable)" ]]; then
        say "安装 Google Chrome"
        local download
        download="$(mktemp -d)"
        curl -fsSL --retry 3 -o "$download/chrome.deb" \
          https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb
        # 包里自带 Google 的 apt 源，以后跟着 apt upgrade 升级。
        apt_get install -y --no-install-recommends "$download/chrome.deb"
        rm -rf "$download"
      fi
      policy_dir=/etc/opt/chrome/policies/managed
      ;;
    aarch64)
      if [[ "$(. /etc/os-release && echo "${ID:-}")" != debian ]]; then
        die "arm64 上只支持 Debian：Google 不出 arm64 的 Chrome，Ubuntu 的 Chromium 只有 snap 版"
      fi
      if [[ -n "$(missing_packages chromium)" ]]; then
        say "安装 Chromium"
        apt_get install -y --no-install-recommends chromium
      fi
      policy_dir=/etc/chromium/policies/managed
      ;;
    *) die "不支持的架构：$(uname -m)" ;;
  esac
  # 省资源（D9 实测）：禁有声自动播放、省内存模式、关后台运行、拒通知。
  install -d "$policy_dir"
  install -m 0644 "$HERE/desktop/chrome-policy.json" "$policy_dir/smelt.json"
}

system_phase() {
  if ! command -v apt-get >/dev/null 2>&1; then
    ((WITH_DESKTOP)) && die "远程桌面只支持用 apt 的发行版（Debian、Ubuntu）"
    warn "不是 apt 系的发行版，系统包请自己装：${BASE_PACKAGES[*]}"
    return 0
  fi
  local missing
  read -ra missing <<<"$(missing_packages $(wanted_packages) | tr '\n' ' ')"
  if ((${#missing[@]})); then
    say "安装系统包：${missing[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt_get update -qq
    apt_get install -y --no-install-recommends "${missing[@]}"
  fi
  ((WITH_DESKTOP)) && install_browser
  return 0
}

# root 执行：系统部分做完，建普通用户，把用户那部分交给它跑。
install_as_root() {
  system_phase

  local user="${SMELT_USER:-smelt}"
  if ! id -u "$user" >/dev/null 2>&1; then
    say "建用户 $user：运行时和桌面都在它名下运行"
    useradd --create-home --shell /bin/bash "$user"
  fi
  local uid home
  uid="$(id -u "$user")"
  home="$(getent passwd "$user" | cut -d: -f6)"
  [[ "$uid" != 0 ]] || die "SMELT_USER 不能是 root：运行时不用 root 跑"

  local env=(HOME="$home" USER="$user" LOGNAME="$user" SHELL=/bin/bash
    PATH=/usr/local/bin:/usr/bin:/bin)
  if has_systemd; then
    loginctl enable-linger "$user"
    # linger 会拉起这个用户的 systemd，等它的总线出来才能 systemctl --user。
    local i
    for ((i = 0; i < 60; i++)); do
      [[ -S "/run/user/$uid/bus" ]] && break
      sleep 0.5
    done
    [[ -S "/run/user/$uid/bus" ]] || die "等不到用户 $user 的 systemd（/run/user/$uid/bus）"
    env+=(XDG_RUNTIME_DIR="/run/user/$uid"
      DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus")
  fi

  # 包目录可能是 root 的临时目录，那个用户进不去，复制一份给它。
  local stage rc=0
  stage="$(runuser -u "$user" -- mktemp -d)"
  cp -R "$HERE/." "$stage/"
  chown -R "$user:" "$stage"
  runuser -u "$user" -- env -i "${env[@]}" \
    bash "$stage/install.sh" --phase=user "${FLAGS[@]}" || rc=$?
  rm -rf "$stage"
  return "$rc"
}

# 普通用户执行：要 root 的那部分经 sudo 做。
install_as_user() {
  local missing
  missing="$(missing_packages $(wanted_packages))"
  if [[ -n "$missing" ]] || ((WITH_DESKTOP)); then
    if command -v sudo >/dev/null 2>&1; then
      say "装系统包要 root，用 sudo"
      sudo bash "$HERE/install.sh" --phase=system "${FLAGS[@]}"
    elif ((WITH_DESKTOP)); then
      die "装远程桌面要 root：用 root 执行，或者先装好 sudo"
    else
      warn "缺系统包 $(echo $missing)，又没有 sudo：准备 Pi 运行时会失败，请让管理员装上"
    fi
  fi
  if has_systemd && command -v sudo >/dev/null 2>&1 \
    && [[ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" != yes ]]; then
    say "开 linger：退出登录后运行时照常在线"
    sudo loginctl enable-linger "$USER" || warn "开 linger 失败，退出登录后运行时会停"
  fi
  user_phase
}

# ── 用户部分（普通用户）──────────────────────────────────────────────────────

# 先写临时文件再 rename：正在跑的程序和脚本不能原地覆盖（bash 是边读边执行的）。
put() {
  local mode=$1 from=$2 to=$3
  install -m "$mode" "$from" "$to.new"
  mv -f "$to.new" "$to"
}

write_unit() {
  local name=$1 description=$2 exec=$3 after=${4:-}
  {
    echo "[Unit]"
    echo "Description=$description"
    if [[ -n "$after" ]]; then
      echo "Wants=$after"
      echo "After=$after"
    fi
    echo
    echo "[Service]"
    echo "ExecStart=$exec"
    # 前一个服务重启时连着它的会断开退出，隔两秒再起时会等前一个就绪。
    echo "Restart=always"
    echo "RestartSec=2"
    echo
    echo "[Install]"
    echo "WantedBy=default.target"
  } >"$UNIT_DIR/$name"
}

install_desktop() {
  local dir="$SMELT_HOME/desktop" config="${XDG_CONFIG_HOME:-$HOME/.config}"
  mkdir -p "$dir/bin"
  local script
  for script in common.sh display session chrome; do
    put 0755 "$HERE/desktop/$script" "$dir/bin/$script"
  done

  # XFCE 的精简设置只在第一次装时写：之后人在远程桌面里改过的，重装不覆盖。
  local xml_dir="$config/xfce4/xfconf/xfce-perchannel-xml" xml
  mkdir -p "$xml_dir"
  for xml in "$HERE"/desktop/xfce4/*.xml; do
    [[ -e "$xml_dir/$(basename "$xml")" ]] || install -m 0644 "$xml" "$xml_dir/"
  done
  # 任务栏的两个启动图标（xfce4-panel.xml 里的 launcher-1、launcher-2）。Chrome 图标调同一个
  # 启动脚本，开的是 agent 用的那个实例，不会另起一个没有登录状态、agent 也连不上的 Chrome。
  local icon=chromium
  command -v google-chrome-stable >/dev/null 2>&1 && icon=google-chrome
  mkdir -p "$config/xfce4/panel/launcher-1" "$config/xfce4/panel/launcher-2"
  cat >"$config/xfce4/panel/launcher-1/smelt-chrome.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Chrome
Exec="$dir/bin/chrome" --new-window
Icon=$icon
EOF
  cat >"$config/xfce4/panel/launcher-2/smelt-terminal.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=终端
Exec=xfce4-terminal
Icon=utilities-terminal
EOF

  if has_systemd; then
    UNIT_DIR="$config/systemd/user"
    mkdir -p "$UNIT_DIR"
    write_unit smelt-display.service "Smelt 远程桌面：虚拟屏幕（Xvnc :99，只听本机 5999）" \
      "$dir/bin/display"
    write_unit smelt-desktop.service "Smelt 远程桌面：XFCE 会话" \
      "$dir/bin/session" smelt-display.service
    write_unit smelt-chrome.service "Smelt 远程桌面：常驻 Chrome（调试端口只绑本机 9222）" \
      "$dir/bin/chrome" smelt-desktop.service
    systemctl --user daemon-reload
    systemctl --user enable --now "${DESKTOP_UNITS[@]}"
  fi
}

smelt_up_running() {
  [[ -e "$SMELT_HOME/smelt-up.lock" ]] && ! flock -n "$SMELT_HOME/smelt-up.lock" true
}

# 没有 systemd：在后台起 smelt-up。已经在跑、但这次新装了桌面，它起的时候没看到桌面，重起
# 一份；别的情况不动它，免得打断正在用浏览器的 agent。
start_smelt_up() {
  local desktop_added=$1
  if smelt_up_running; then
    ((desktop_added)) || return 0
    say "重启 smelt-up，让它带上远程桌面"
    kill "$(cat "$SMELT_HOME/smelt-up.pid")" 2>/dev/null || true
    local i
    for ((i = 0; i < 20; i++)); do
      smelt_up_running || break
      sleep 0.5
    done
  fi
  setsid nohup "$BIN/smelt-up" >/dev/null 2>&1 </dev/null &
}

wait_for_runtime() {
  local i
  for ((i = 0; i < 60; i++)); do
    "$BIN/smeltd" status >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  die "等了 30 秒运行时还没起来，日志在 $SMELT_HOME/daemon.log"
}

user_phase() {
  [[ "$(id -u)" != 0 ]] || die "运行时不用 root 跑"
  [[ "$(uname -s)" == Linux ]] || die "这个包只能装在 Linux 上"
  local program
  for program in "${PROGRAMS[@]}"; do
    [[ -x "$HERE/$program" ]] || die "包里缺少 $program"
  done
  if has_systemd && [[ -z "${XDG_RUNTIME_DIR:-}" && -d "/run/user/$(id -u)" ]]; then
    export XDG_RUNTIME_DIR="/run/user/$(id -u)"
  fi

  SMELT_HOME="${SMELT_HOME:-$HOME/.smelt}"
  BIN="$SMELT_HOME/bin"
  mkdir -p "$BIN"
  put 0755 "$HERE/smelt-notify" "$BIN/smelt-notify"
  put 0755 "$HERE/smelt-up" "$BIN/smelt-up"
  # smelt-pi、wasm、native 留在包目录里。下面的 `smeltd install` 按它们自己的指纹
  # 发布成 ~/.smelt/bin/smelt-pi-<指纹>/，再把 ~/.smelt/bin/smelt-pi 链接拨过去。
  [[ -x "$HERE/smelt-pi" && -f "$HERE/photon_rs_bg.wasm" ]] || die "包里缺少 smelt-pi 或 photon_rs_bg.wasm"
  # 跨 Agent 消息下线后留下的旧 helper。
  rm -f "$BIN/smelt-agent-mcp"

  # 发布不可变映像、拨启动名（已经在跑就交接到新版本，会话保留）；有 systemd 时写用户单元
  # 并启动。
  "$HERE/smeltd" install
  echo "已装到 $BIN"

  local desktop_added=0
  if ((WITH_DESKTOP)); then
    [[ -x "$SMELT_HOME/desktop/bin/display" ]] || desktop_added=1
    say "配置远程桌面"
    install_desktop
  fi
  has_systemd || start_smelt_up "$desktop_added"

  wait_for_runtime
  if ((WITH_DESKTOP)); then
    "$BIN/smeltd" mcp add desktop-chrome
  fi
  if ((PAIR)); then
    "$BIN/smeltd" pair
  fi

  if ! has_systemd; then
    local watched=运行时
    ((WITH_DESKTOP)) && watched=运行时和远程桌面
    echo
    echo "这台机器没有 systemd：${watched}由 smelt-up 看着，日志在 $SMELT_HOME/logs。"
    echo "容器重启后要再拉起它，或者直接把它设成容器的启动命令："
    echo "  runuser -u $USER -- $BIN/smelt-up    （以 root）"
    echo "  $BIN/smelt-up &    （以 $USER）"
  fi
}

case "$PHASE" in
  system)
    [[ "$(id -u)" == 0 ]] || die "--phase=system 要 root"
    system_phase
    ;;
  user) user_phase ;;
  all)
    if [[ "$(id -u)" == 0 ]]; then
      install_as_root
    else
      install_as_user
    fi
    ;;
esac
