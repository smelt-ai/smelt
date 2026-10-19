#!/usr/bin/env bash
# 没有 systemd 的机器（多半是 Docker 容器）上拉起并看住运行时和远程桌面，顶替 systemd 那边的
# smeltd.service 和三个 smelt-*.service。随运行时的包分发，install.sh 装成
# ~/.smelt/bin/smelt-up，装完在后台起一份。
#
# 它一直在前台跑，可以直接当容器的启动命令；容器重启后不设启动命令就要手动再起一次。
# 同一个用户只跑一份，重复起会直接退出。日志在 ~/.smelt/logs。
set -uo pipefail

SMELT_HOME="${SMELT_HOME:-$HOME/.smelt}"
BIN="$SMELT_HOME/bin"
DESKTOP="$SMELT_HOME/desktop/bin"
LOGS="$SMELT_HOME/logs"
mkdir -p "$LOGS"

# 锁在 9 号描述符上。拉起的程序都要关掉它（9>&-），否则运行时继承了锁，smelt-up 自己退出后
# 下一份以为它还在。
exec 9>"$SMELT_HOME/smelt-up.lock"
flock -n 9 || { echo "smelt-up 已经在跑" >&2; exit 0; }
echo $$ >"$SMELT_HOME/smelt-up.pid"

# 退出时带走桌面那几个进程。运行时不在这个进程组里（setsid 出去的），见 watch_runtime。
trap 'trap - TERM INT EXIT; rm -f "$SMELT_HOME/smelt-up.pid"; kill 0' TERM INT EXIT

# 退出了就隔两秒重启，同 systemd 的 Restart=always。
keep() {
  local name=$1
  shift
  : >"$LOGS/$name.log"
  while true; do
    "$@" >>"$LOGS/$name.log" 2>&1 </dev/null 9>&-
    sleep 2
  done
}

# 运行时不能等它退出再重启：升级时旧进程拉起新进程（交接）后退出，新进程照常服务。所以只看
# 它在不在服务，不在才拉起。碰巧和交接撞上时，多起的那份发现已有实例会自己退出。
watch_runtime() {
  while true; do
    if ! "$BIN/smeltd" status >/dev/null 2>&1; then
      setsid "$BIN/smeltd" >>"$LOGS/smeltd.log" 2>&1 </dev/null 9>&- &
    fi
    sleep 5
  done
}

if [[ -x "$DESKTOP/display" ]]; then
  keep display "$DESKTOP/display" &
  keep session "$DESKTOP/session" &
  keep chrome "$DESKTOP/chrome" &
fi
watch_runtime &
wait
