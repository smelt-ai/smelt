#!/usr/bin/env bash
# 在一台 Linux 机器上装 Smelt 运行时（smeltd），可选装远程桌面和常驻浏览器（设计文档 D7、
# D9）。随 tar.gz 一起分发，解包后在包目录里执行；一键安装（scripts/get-smeltd.sh，发布成
# Release 里的 install.sh）下载、校验这个包之后调的也是它。重复执行就是升级。
#
#   ./install.sh                  装好、启动，打印配对串
#   ./install.sh --with-desktop   另装远程桌面（Xvnc + XFCE）和常驻 Chrome，Pi 接上这个 Chrome
#   ./install.sh --no-pair        不打印配对串
#   ./install.sh --relay=<地址>   远程访问只用这个自建 iroh relay（域名、IP 或 URL），配对串会
#                                 带上它；不给就用 n0 的公共 relay（见 smeltd relay）
#   ./install.sh --apt-mirror=<镜像>  装系统包时把 Debian / Ubuntu 官方源换成这个镜像（主机名
#                                 或 URL）。不给时先试官方源，连不上（中国大陆常见）就自动换
#                                 APT_MIRRORS 里头一个连得上的。只用于这次安装，不改 /etc/apt
#
# 装在谁名下：谁执行就装在谁名下。以普通用户执行时装系统包、开 linger 用 sudo；以 root 执行时
# 运行时就以 root 跑（见 runtime_user），SMELT_USER 可以改成装在一个普通用户名下。以 root 运行
# 时告诉 Claude Code 这是沙箱（IS_SANDBOX=1），否则它拒绝跳过权限确认；Chrome 也得关沙箱才肯
# 以 root 起。
#
# 怎么敲到命令（smeltd 和命令行 smelt）：运行用户是 root 时在 /usr/local/bin 放软链接；是另一个
# 用户时在那里生成同名脚本，切到那个用户再跑（见 root_command_script）；以普通用户执行时在
# ~/.local/bin 放软链接，不改 shell 配置。运行时起的智能体不靠这些：它们的 PATH 里本来就有
# ~/.smelt/bin。
#
# 怎么常驻：有 systemd 的机器装成 systemd 用户服务（smeltd、smelt-display、smelt-desktop、
# smelt-chrome），开 linger 让它们开机就起、退出登录也不停。没有 systemd 的（多半是 Docker
# 容器）由 ~/.smelt/bin/smelt-up 拉起并看着，装完在后台起一份。
#
# 系统包只支持 apt（Debian 12+、Ubuntu 22.04+）。模型和密钥不在这里配：装完在桌面的能力库里
# 配（D7）。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROGRAMS=(smeltd smelt-notify smelt)
# 用户在终端里直接敲的。
COMMANDS=(smeltd smelt)
# curl：运行时升级下载新版本要用。
BASE_PACKAGES=(ca-certificates curl)
# D9 实测过的一套；不装中文字体，中文网页全是方块。装时不带推荐包（容器里能省几百 MB），
# 所以 Xvnc 要的 X 核心字体（fixed）、图标主题和终端的等宽字体要自己列上。图标用 Papirus
# （只占磁盘，不耗 CPU）；Adwaita 留着给鼠标指针，也是 Papirus 缺图标时的后备。GTK3 只内置
# 浅色的 Adwaita，xsettings.xml 里的 Adwaita-dark 在 gnome-themes-extra-data 里，不装就退回
# 浅色。开始菜单用 Whisker（带搜索和收藏夹），文件管理器用 Thunar，底部的停靠栏用 Plank，
# 界面字体用 Inter（接近 macOS 的系统字体）。
DESKTOP_PACKAGES=(
  tigervnc-standalone-server xauth xfonts-base xfce4 xfce4-terminal adwaita-icon-theme
  papirus-icon-theme gnome-themes-extra-data xfce4-whiskermenu-plugin thunar plank dbus xdotool
  fonts-noto-cjk fonts-dejavu-core fonts-inter
)
# 运行时装成 systemd 用户服务：linger 拉起用户的 systemd 要 pam_systemd 给它建
# XDG_RUNTIME_DIR，systemctl --user 要用户总线（/run/user/<uid>/bus）。minbase 一类的精简
# 镜像（如 Debian Cloud Micro）两样都没有，用户的 systemd 起不来。
SYSTEMD_PACKAGES=(libpam-systemd dbus-user-session)
DESKTOP_UNITS=(smelt-display.service smelt-desktop.service smelt-chrome.service)

WITH_DESKTOP=0
PAIR=1
RELAY=""
APT_MIRROR=""
# 内部用：root 那部分经 sudo 单独跑（system），装到用户名下那部分经 runuser 单独跑（user）。
PHASE=all

usage() {
  echo "用法：$0 [--with-desktop] [--no-pair] [--relay=<地址>] [--apt-mirror=<镜像>]" >&2
}

for arg in "$@"; do
  case "$arg" in
    --with-desktop) WITH_DESKTOP=1 ;;
    --no-pair) PAIR=0 ;;
    --relay=?*) RELAY="${arg#--relay=}" ;;
    --apt-mirror=?*) APT_MIRROR="${arg#--apt-mirror=}" ;;
    --phase=system | --phase=user) PHASE="${arg#--phase=}" ;;
    *) usage; exit 2 ;;
  esac
done

FLAGS=()
((WITH_DESKTOP)) && FLAGS+=(--with-desktop)
((PAIR)) || FLAGS+=(--no-pair)
[[ -z "$RELAY" ]] || FLAGS+=("--relay=$RELAY")
[[ -z "$APT_MIRROR" ]] || FLAGS+=("--apt-mirror=$APT_MIRROR")

say() { echo "==> $*"; }

warn() { echo "install.sh：$*" >&2; }
die() { warn "$*"; exit 1; }

# 用了 --apt-mirror 时，每次 apt-get 都读换过地址的那份源配置（见 use_apt_mirror）。
APT_SOURCES=()
# 新开的机器上常有自动更新或初始化脚本正在跑 apt，占着 dpkg 的锁。等它放锁（最多 10 分钟），
# 不要一见锁就失败。
apt_get() {
  local rc=0
  apt-get -o DPkg::Lock::Timeout=600 "${APT_SOURCES[@]}" "$@" || rc=$?
  ((rc)) && warn "apt-get 失败。是源连不上或太慢的话，用 --apt-mirror=<镜像> 指定一个重新执行"
  return "$rc"
}

# 软件包索引每次安装刷新一次：机器上留着的索引可能是旧的，按它去取包会 404；换了镜像时
# 更得按镜像重新取。用哪套源也在这时定：不装包就不用探测。
APT_UPDATED=0
apt_update() {
  ((APT_UPDATED)) && return 0
  choose_apt_sources
  apt_get update -qq
  APT_UPDATED=1
}

# 中国大陆的机器常连不上 Debian / Ubuntu 官方源。没给 --apt-mirror 又连不上时，按顺序换头一个
# 连得上的镜像。
APT_MIRRORS=(mirrors.aliyun.com mirrors.tuna.tsinghua.edu.cn mirrors.ustc.edu.cn)
OFFICIAL_APT='https?://(deb\.debian\.org|security\.debian\.org|([a-z]+\.)?archive\.ubuntu\.com|security\.ubuntu\.com|ports\.ubuntu\.com)/'
APT_SOURCE_FILES=(/etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources)

# 只试 TCP 建连：连不上的机器在这一步就失败（不通或超时），用不着装了 curl 才能探测。
reachable() { timeout 5 bash -c "exec 3<>/dev/tcp/$1/$2" 2>/dev/null; }

official_sources_reachable() {
  local url host
  while read -r url; do
    host="${url#*://}"
    host="${host%/}"
    if [[ "$url" == https:* ]]; then reachable "$host" 443; else reachable "$host" 80; fi \
      || return 1
  done < <(grep -Eoh "$OFFICIAL_APT" "${APT_SOURCE_FILES[@]}" 2>/dev/null | sort -u)
}

choose_apt_sources() {
  if [[ -n "$APT_MIRROR" ]]; then
    use_apt_mirror "$APT_MIRROR"
    return 0
  fi
  official_sources_reachable && return 0
  local mirror
  for mirror in "${APT_MIRRORS[@]}"; do
    if reachable "$mirror" 80; then
      say "连不上 Debian / Ubuntu 官方源"
      use_apt_mirror "$mirror"
      return 0
    fi
  done
  warn "官方源和镜像（${APT_MIRRORS[*]}）都连不上，照原来的源试"
}

# 只在这次安装里把发行版官方源换成镜像，不改 /etc/apt：源配置复制一份、改掉官方地址，
# apt-get 经 -o 读这份。路径原样保留，镜像站都按官方源的目录摆（/debian、/debian-security、
# /ubuntu、/ubuntu-ports）。只给主机名时用 http：新机器上可能还没装 ca-certificates。
use_apt_mirror() {
  local mirror=$1
  [[ "$mirror" == *://* ]] || mirror="http://$mirror"
  mirror="${mirror%/}"
  # 不能是 local：EXIT 时 trap 里还要用它。
  apt_dir="$(mktemp -d)"
  trap 'rm -rf "$apt_dir"' EXIT
  mkdir "$apt_dir/sources.list.d"
  local file to replaced=0
  for file in "${APT_SOURCE_FILES[@]}"; do
    [[ -f "$file" ]] || continue
    to="$apt_dir/sources.list.d/$(basename "$file")"
    [[ "$file" == /etc/apt/sources.list ]] && to="$apt_dir/sources.list"
    grep -Eq "$OFFICIAL_APT" "$file" && replaced=1
    sed -E "s#$OFFICIAL_APT#$mirror/#g" "$file" >"$to"
  done
  ((replaced)) || die "apt 源里没有 Debian / Ubuntu 的官方地址，--apt-mirror 无从替换"
  say "这次安装用镜像 $mirror 装系统包"
  APT_SOURCES=(-o "Dir::Etc::SourceList=$apt_dir/sources.list"
    -o "Dir::Etc::SourceParts=$apt_dir/sources.list.d")
}

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
  has_systemd && echo "${SYSTEMD_PACKAGES[@]}"
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
        # mktemp -d 是 0700，apt 的下载用户 _apt 读不到，会退回 root 取包并报一句提示。
        chmod -R a+rX "$download"
        apt_update
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
        apt_update
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
  export DEBIAN_FRONTEND=noninteractive
  local missing
  read -ra missing <<<"$(missing_packages $(wanted_packages) | tr '\n' ' ')"
  if ((${#missing[@]})); then
    say "安装系统包：${missing[*]}"
    apt_update
    apt_get install -y --no-install-recommends "${missing[@]}"
  fi
  ((WITH_DESKTOP)) && install_browser
  return 0
}

# root 执行时运行时装在谁名下：SMELT_USER 优先；没给时以前装在 smelt 名下的接着用它（重新执行
# 就是升级，换个用户等于另装一份、丢掉配对和会话），否则就是 root。
runtime_user() {
  if [[ -n "${SMELT_USER:-}" ]]; then
    echo "$SMELT_USER"
  elif id -u smelt >/dev/null 2>&1 \
    && [[ -e "$(getent passwd smelt | cut -d: -f6)/.smelt/bin/smeltd" ]]; then
    echo smelt
  else
    echo root
  fi
}

# 开 linger 拉起这个用户的 systemd，等它的总线出来才能 systemctl --user。
start_user_manager() {
  local user=$1 uid=$2
  loginctl enable-linger "$user"
  local i
  for ((i = 0; i < 60; i++)); do
    [[ -S "/run/user/$uid/bus" ]] && break
    sleep 0.5
  done
  [[ -S "/run/user/$uid/bus" ]] || die "等不到用户 $user 的 systemd（/run/user/$uid/bus）"
}

# root 执行：系统部分做完，用户那部分交给运行用户跑。
install_as_root() {
  system_phase

  local user
  user="$(runtime_user)"
  if [[ "$user" == root ]]; then
    install_for_root
    return
  fi
  if ! id -u "$user" >/dev/null 2>&1; then
    say "建用户 ${user}：运行时和桌面都在它名下运行"
    useradd --create-home --shell /bin/bash "$user"
  fi
  local uid home
  uid="$(id -u "$user")"
  home="$(getent passwd "$user" | cut -d: -f6)"
  [[ "$uid" != 0 ]] || die "SMELT_USER=$user 是 uid 0 的用户：要以 root 运行就写 SMELT_USER=root"

  local env=(HOME="$home" USER="$user" LOGNAME="$user" SHELL=/bin/bash
    PATH=/usr/local/bin:/usr/bin:/bin)
  if has_systemd; then
    start_user_manager "$user" "$uid"
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
  ((rc == 0)) || return "$rc"

  local runtime_dir=""
  has_systemd && runtime_dir="/run/user/$uid"
  local command
  for command in "${COMMANDS[@]}"; do
    install_root_command "$command" "$user" "$uid" "$home" "$runtime_dir"
  done
  echo
  say "运行时在用户 $user 名下。常用命令（root 或 sudo 都行）：smeltd status、smeltd pair、smelt agent list"
}

# 运行用户就是 root：用户那部分直接在这个进程里跑。HOME 等按 root 的账户重设，sudo 执行时
# 可能还是调用者的。
install_for_root() {
  export HOME USER=root LOGNAME=root
  HOME="$(getent passwd root | cut -d: -f6)"
  if has_systemd; then
    start_user_manager root 0
    export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
  fi
  user_phase
  link_commands "$ROOT_COMMAND_DIR"
  say "运行时以 root 运行（要装在普通用户名下，用 SMELT_USER=<用户名> 重新执行）"
}

ROOT_COMMAND_DIR=/usr/local/bin
ROOT_COMMAND_MARKER="由 Smelt 的 install.sh 生成"

# /usr/local/bin/<命令> 的内容。不能用软链接：root 的 ~ 和权限都不对，运行时连不上。root
# 调用时一定先切用户再执行：那份程序在普通用户目录里，它能改，以 root 执行就等于提权。
root_command_script() {
  local command="$1" user="$2" uid="$3" home="$4" runtime_dir="$5"
  local env_args="HOME=$(printf %q "$home") USER=$(printf %q "$user") LOGNAME=$(printf %q "$user")"
  if [[ -n "$runtime_dir" ]]; then
    env_args+=" XDG_RUNTIME_DIR=$(printf %q "$runtime_dir")"
    env_args+=" DBUS_SESSION_BUS_ADDRESS=$(printf %q "unix:path=$runtime_dir/bus")"
  fi
  cat <<WRAPPER
#!/bin/bash
# ${ROOT_COMMAND_MARKER}，重新安装时会覆盖。
bin=$(printf %q "$home/.smelt/bin/$command")
user=$(printf %q "$user")
case "\$(id -u)" in
  0 | $uid) ;;
  *)
    echo "$command 在用户 \$user 名下运行，请用 sudo $command \$*" >&2
    exit 1
    ;;
esac
if [[ ! -x "\$bin" ]]; then
  echo "找不到 \${bin}，运行时可能已经卸载；不用了可以删掉 \$0" >&2
  exit 1
fi
if [[ "\$(id -u)" == $uid ]]; then
  exec "\$bin" "\$@"
fi
# 调用者的目录那个用户多半进不去。
cd $(printf %q "$home")
exec runuser -u "\$user" -- env -i $env_args SHELL=/bin/bash \\
  PATH=/usr/local/bin:/usr/bin:/bin TERM="\${TERM:-dumb}" "\$bin" "\$@"
WRAPPER
}

# 已经有一个不是我们生成的同名文件时不覆盖。
install_root_command() {
  local target="$ROOT_COMMAND_DIR/$1"
  if [[ -e "$target" ]] && ! grep -qF "$ROOT_COMMAND_MARKER" "$target" 2>/dev/null; then
    warn "$target 已存在且不是安装脚本生成的，没有覆盖；用 runuser -u $2 -- $4/.smelt/bin/$1 调用"
    return 0
  fi
  local tmp
  tmp="$(mktemp "$target.XXXXXX")"
  root_command_script "$@" >"$tmp"
  chmod 0755 "$tmp"
  mv -f "$tmp" "$target"
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
  link_commands "$HOME/.local/bin"
}

# 在 dir 下放指向 ~/.smelt/bin 的软链接，不改 shell 配置：普通用户放 ~/.local/bin，root 放
# /usr/local/bin。覆盖自己放的软链接和以前生成的切用户脚本，别人的文件不动。自定义了
# SMELT_HOME 时不放：软链接带不上这个变量，会连到另一份运行时。
link_commands() {
  local dir=$1
  echo
  if [[ "$SMELT_HOME" != "$HOME/.smelt" ]]; then
    say "SMELT_HOME 是自定义的，调用时带上它：SMELT_HOME=$SMELT_HOME $BIN/smeltd status"
    return 0
  fi
  mkdir -p "$dir"
  local command link
  for command in "${COMMANDS[@]}"; do
    link="$dir/$command"
    if [[ -e "$link" && ! -L "$link" ]] && ! grep -qF "$ROOT_COMMAND_MARKER" "$link" 2>/dev/null; then
      warn "$link 已存在且不是安装脚本放的，没有覆盖；用 $BIN/$command 调用"
      continue
    fi
    ln -sfn "$BIN/$command" "$link"
  done
  say "常用命令：smeltd status、smeltd pair、smelt agent list"
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "$dir 不在 PATH 里，加上后才能直接敲：export PATH=\"$dir:\$PATH\"" ;;
  esac
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

# XFCE 和停靠栏的设置随包走，按内容记一份指纹：包里的设置没变就不动，人在远程桌面里改过
# 的保留；变了（包括有指纹之前装的机器）就整份换掉，再重启会话让它读新的。
apply_desktop_config() {
  local dir=$1 config=$2 browser=$3
  local stamp="$dir/config.sha256" xml_dir="$config/xfce4/xfconf/xfce-perchannel-xml" want
  # 图片和主题也算进去：窗口管理器、xfdesktop、面板和停靠栏都只在起来时读一次，换了要重启
  # 会话才看得到。浏览器也算：停靠栏上的图标指向它。
  want="$({
    cat "$HERE"/desktop/xfce4/*.xml "$HERE/desktop/terminalrc" "$HERE"/desktop/*.{png,jpg} \
      "$HERE"/desktop/xfwm4/* "$HERE"/desktop/plank/*
    echo "$browser"
  } | sha256sum | cut -d' ' -f1)"
  [[ "$(cat "$stamp" 2>/dev/null)" == "$want" ]] && return 0
  mkdir -p "$xml_dir" "$config/xfce4/terminal"
  install -m 0644 "$HERE"/desktop/xfce4/*.xml "$xml_dir/"
  # xfconf 不展开 ~ 和变量，壁纸要写绝对路径。
  sed -i "s|@DESKTOP_DIR@|$dir|g" "$xml_dir/xfce4-desktop.xml"
  # xfce4-terminal 1.0 以前的版本不读 xfconf，读这份；和 xfce4-terminal.xml 内容相同。
  install -m 0644 "$HERE/desktop/terminalrc" "$config/xfce4/terminal/terminalrc"

  # 停靠栏：设置（见 desktop/plank/settings）和三个启动图标。图标按文件名排在设置的
  # dock-items 里；人在停靠栏上拖走、加上的，Plank 自己增删这里的文件。
  mkdir -p "$config/glib-2.0/settings"
  install -m 0644 "$HERE/desktop/plank/settings" "$config/glib-2.0/settings/keyfile"
  local launchers="$config/plank/dock1/launchers" apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  rm -rf "$launchers"
  mkdir -p "$launchers"
  local item desktop
  for item in "browser:$apps/$browser.desktop" terminal:/usr/share/applications/xfce4-terminal.desktop \
    files:/usr/share/applications/thunar.desktop; do
    desktop=${item#*:}
    printf '[PlankDockItemPreferences]\nLauncher=file://%s\n' "$desktop" >"$launchers/${item%%:*}.dockitem"
  done

  restart_desktop_session
  echo "$want" >"$stamp"
}

# 换了设置的会话要整个重来。xfconfd 把设置缓存在内存里，正常退出会把旧值写回文件，所以
# 会话里的进程一律 SIGKILL，不给写回的机会；看护（systemd 或 smelt-up）随即按新设置拉起。
# 没在跑就什么都不做。
restart_desktop_session() {
  if has_systemd; then
    systemctl --user is-active --quiet smelt-desktop.service || return 0
    systemctl --user kill -s KILL smelt-desktop.service
    systemctl --user restart smelt-desktop.service
    return 0
  fi
  pkill -KILL -u "$(id -u)" -x '(xfce4-session|xfwm4|xfce4-panel|xfdesktop|xfsettingsd|xfconfd|plank)' || true
  # 换成 dbus-run-session 之前，会话的总线由 dbus-launch 起，会话死了它还留着。
  pkill -KILL -u "$(id -u)" -x dbus-launch || true
  pkill -KILL -u "$(id -u)" -f '^dbus-daemon .*--session' || true
}

install_desktop() {
  local dir="$SMELT_HOME/desktop" config="${XDG_CONFIG_HOME:-$HOME/.config}"
  mkdir -p "$dir/bin"
  local script
  for script in common.sh display session chrome; do
    put 0755 "$HERE/desktop/$script" "$dir/bin/$script"
  done

  # 壁纸（xfce4-desktop.xml）和开始菜单按钮的图标；以前的 png 壁纸和彩色图标不用了。
  put 0644 "$HERE/desktop/wallpaper.jpg" "$dir/wallpaper.jpg"
  put 0644 "$HERE/desktop/menu-icon.png" "$dir/menu-icon.png"
  rm -f "$dir/wallpaper.png" "$dir/smelt.png"

  # 窗口主题（xfwm4.xml 选的 Smelt）和停靠栏主题（desktop/plank/settings 选的 Smelt），装到
  # 用户自己的主题目录，两个程序都会去那里找。
  local data="${XDG_DATA_HOME:-$HOME/.local/share}"
  mkdir -p "$data/themes/Smelt/xfwm4" "$data/plank/themes/Smelt"
  install -m 0644 "$HERE"/desktop/xfwm4/* "$data/themes/Smelt/xfwm4/"
  install -m 0644 "$HERE/desktop/plank/dock.theme" "$data/plank/themes/Smelt/dock.theme"
  # 会话起来时拉起停靠栏。Plank 只认 GSettings，这里多半没有 dconf，让它用 GLib 自带的
  # keyfile 后端，设置写在 ~/.config/glib-2.0/settings/keyfile（见 apply_desktop_config）。
  mkdir -p "$config/autostart"
  cat >"$config/autostart/smelt-dock.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Dock
Exec=env GSETTINGS_BACKEND=keyfile plank
NoDisplay=true
EOF

  # 浏览器的菜单项用同名文件盖掉系统那份（用户目录里同名的优先），改成调同一个启动脚本：
  # 从菜单、文件管理器或别的程序打开网页，开的都是 agent 用的那个实例，不会另起一个没有
  # 登录状态、agent 也连不上的 Chrome。带 --new-window：不带参数是服务在拉起（见 chrome）。
  # MimeType 照抄系统那份，网页和链接仍然归它打开。
  local browser=chromium name=Chromium
  if command -v google-chrome-stable >/dev/null 2>&1; then
    browser=google-chrome name=Chrome
  fi
  local apps="$data/applications"
  mkdir -p "$apps"
  cat >"$apps/$browser.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$name
Exec="$dir/bin/chrome" --new-window %U
Icon=$browser
Categories=Network;WebBrowser;
MimeType=text/html;application/xhtml+xml;x-scheme-handler/http;x-scheme-handler/https;
EOF
  # 开始菜单（Whisker）的默认设置：按钮只有 Smelt 的白色剪影、不带文字，同 macOS 菜单栏
  # 左上角的苹果；菜单里不放用户头像（profile-shape=2）；收藏夹放浏览器、终端、文件管理器。
  # 2.7（Debian 12、Ubuntu 22.04）和 2.8 起都先读这份，人在菜单里改过的另存，不受它影响。
  mkdir -p "$config/xfce4/whiskermenu"
  cat >"$config/xfce4/whiskermenu/defaults.rc" <<EOF
favorites=$browser.desktop,xfce4-terminal.desktop,thunar.desktop
button-icon=$dir/menu-icon.png
show-button-icon=true
show-button-title=false
profile-shape=2
EOF
  # 更早的版本在面板上放过两个启动图标，现在的面板没有了。
  rm -f "$config/xfce4/panel/launcher-1/smelt-chrome.desktop" \
    "$config/xfce4/panel/launcher-2/smelt-terminal.desktop"
  rmdir "$config/xfce4/panel/launcher-1" "$config/xfce4/panel/launcher-2" 2>/dev/null || true

  # 菜单项写好之后再换设置：要重启会话的话，新会话一起来就看得到它们。
  apply_desktop_config "$dir" "$config" "$browser"

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
  put 0755 "$HERE/smelt" "$BIN/smelt"
  put 0755 "$HERE/smelt-up" "$BIN/smelt-up"
  # smelt-agent、wasm、native 留在包目录里。下面的 `smeltd install` 把它们装进
  # ~/.smelt/bin/agent/，~/.smelt/bin/smelt-agent 链接指向那里。
  [[ -x "$HERE/smelt-agent" && -f "$HERE/photon_rs_bg.wasm" ]] || die "包里缺少 smelt-agent 或 photon_rs_bg.wasm"
  # 跨 Agent 消息下线后留下的旧 helper。
  rm -f "$BIN/smelt-agent-mcp"

  # 以 root 运行：Claude Code 见到 root 就拒绝跳过权限确认，除非声明这是沙箱。要在
  # `smeltd install` 启动服务之前写好。没有 systemd 时由 smelt-up 设。
  if [[ "$(id -u)" == 0 ]] && has_systemd; then
    local dropin="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/smeltd.service.d"
    mkdir -p "$dropin"
    printf '[Service]\nEnvironment=IS_SANDBOX=1\n' >"$dropin/sandbox.conf"
  fi

  # 装成 ~/.smelt/bin/smeltd（已经在跑就重启到新版本，进行中的会话会中断）；有 systemd 时
  # 写用户单元并启动。
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
    "$BIN/smeltd" mcp add playwright
  fi
  # 配对串要带上中继，所以先设中继再配对。
  if [[ -n "$RELAY" ]]; then
    "$BIN/smeltd" relay "$RELAY"
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
    if [[ "$USER" == root ]]; then
      echo "  $BIN/smelt-up"
    else
      echo "  runuser -u $USER -- $BIN/smelt-up    （以 root）"
      echo "  $BIN/smelt-up &    （以 ${USER}）"
    fi
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
