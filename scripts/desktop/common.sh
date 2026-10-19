# 远程桌面几个启动脚本共用，由它们 source，不单独执行。
#
# 显示号 :99 和 smeltd 注入文字用的默认值一致（crates/smeltd/src/desktop.rs）。

DISPLAY_NUMBER=99
export DISPLAY=":$DISPLAY_NUMBER"
X_SOCKET="/tmp/.X11-unix/X$DISPLAY_NUMBER"

# 等虚拟屏幕起来。几个服务差不多同时被拉起，X 还没开始听就去连会直接失败退出。
wait_for_display() {
  local i
  for ((i = 0; i < 60; i++)); do
    [[ -S "$X_SOCKET" ]] && return 0
    sleep 0.5
  done
  echo "等了 30 秒，虚拟屏幕 $DISPLAY 还没起来" >&2
  return 1
}
