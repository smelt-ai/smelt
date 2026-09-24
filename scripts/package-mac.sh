#!/usr/bin/env bash
# 把 workspace GUI 打包成可分发的 Smelt.app（Apple Silicon / arm64，不签名）。
#
# 用法：
#   ./scripts/package-mac.sh            # 用已有 release 产物组装
#   ./scripts/package-mac.sh --build    # 先 cargo build --release 再组装
#
# 产物：
#   dist/Smelt.app     —— 可双击运行的应用
#   dist/Smelt.dmg     —— 分发件（定制拖拽安装窗口）
set -euo pipefail

APP_NAME="Smelt"
BIN_NAME="smelt"              # cargo 产物名
EXEC_NAME="smelt"             # .app 内可执行文件名
BUNDLE_ID="com.zzfn.smelt"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
UPDATE_URL="${SMELT_UPDATE_URL:-}"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"
BIN="$ROOT/target/release/$BIN_NAME"
DAEMON_BIN="$ROOT/target/release/smeltd"   # 终端持久化守护（GUI 按同目录寻址拉起）
NOTIFY_BIN="$ROOT/target/release/smelt-notify" # Agent hooks → smeltd 状态通道
AGENT_MCP_BIN="$ROOT/target/release/smelt-agent-mcp" # Agent 间消息的 STDIO MCP 适配器
INSTALLER_BIN="$ROOT/target/release/smelt-installer" # GUI 退出后提交原子 App 交换
PLUGIN_SOURCES="$ROOT/plugins"        # 每个子目录一个插件包（基础 manifest + 可选 UI sidecar/web）

# dmgbuild 1.6.7 要求 Python >= 3.10；Command Line Tools 自带的
# /usr/bin/python3 在新 macOS 上仍可能是 3.9，必须在耗时编译前报清楚。
PYTHON_COMMAND="${SMELT_PYTHON:-python3}"
if ! PYTHON="$(command -v "$PYTHON_COMMAND")"; then
  echo "✗ 找不到 Python 解释器：$PYTHON_COMMAND" >&2
  echo "  请先运行 brew install python，或设置 SMELT_PYTHON=/path/to/python3.10+" >&2
  exit 1
fi
if ! PYTHON_VERSION="$("$PYTHON" -c \
  'import sys; print(".".join(map(str, sys.version_info[:3]))); sys.exit(sys.version_info < (3, 10))')"; then
  echo "✗ 打包需要 Python >= 3.10，当前为 ${PYTHON_VERSION}（${PYTHON}）" >&2
  echo "  macOS Command Line Tools 的 /usr/bin/python3 可能仍是 3.9。" >&2
  echo "  请先运行 brew install python，或设置 SMELT_PYTHON=/path/to/python3.10+" >&2
  exit 1
fi

if [[ "${1:-}" == "--build" ]]; then
  echo "▶ 编译 release …"
  if ! "$ROOT/scripts/bundled-plugins.sh" dirs >/dev/null; then
    echo "✗ bundled plugin manifest 或 entrypoint 无效，拒绝继续打包" >&2
    exit 1
  fi
  cargo build --release --bin "$BIN_NAME" --bin smeltd --bin smelt-notify --bin smelt-agent-mcp --bin smelt-installer
fi

if [[ ! -f "$BIN" ]]; then
  echo "✗ 找不到 ${BIN}，先跑一次：cargo build --release --bin ${BIN_NAME}（或加 --build）" >&2
  exit 1
fi
if [[ ! -f "$DAEMON_BIN" ]]; then
  echo "✗ 找不到 ${DAEMON_BIN}（终端持久化守护），先：cargo build --release --bin smeltd" >&2
  exit 1
fi
if [[ ! -f "$NOTIFY_BIN" ]]; then
  echo "✗ 找不到 ${NOTIFY_BIN}（Agent hook helper），先：cargo build --release --bin smelt-notify" >&2
  exit 1
fi
if [[ ! -f "$AGENT_MCP_BIN" ]]; then
  echo "✗ 找不到 ${AGENT_MCP_BIN}（cross-agent MCP helper），先：cargo build --release --bin smelt-agent-mcp" >&2
  exit 1
fi
if [[ ! -f "$INSTALLER_BIN" ]]; then
  echo "✗ 找不到 ${INSTALLER_BIN}（App installer helper），先：cargo build --release --bin smelt-installer" >&2
  exit 1
fi
# 插件按目录扫描，不写死名单：新增插件只要在 plugins/<name>/ 放好 plugin.json
# 并准备好 manifest 声明的入口，就会被打进 .app。缺 package 内脚本
# 都直接报错——发版产物少一个插件必须当场知道，不能悄悄发出去。
plugin_dirs=()
if ! bundled_plugin_dirs="$("$ROOT/scripts/bundled-plugins.sh" dirs)"; then
  echo "✗ bundled plugin manifest 或 entrypoint 无效，拒绝继续打包" >&2
  exit 1
fi
while IFS= read -r candidate; do
  [[ -n "$candidate" ]] || continue
  plugin_dirs+=("$candidate/")
done <<<"$bundled_plugin_dirs"
if [[ ${#plugin_dirs[@]} -eq 0 ]]; then
  echo "✗ ${PLUGIN_SOURCES} 下没有发现任何插件包（缺 plugin.json）" >&2
  exit 1
fi
for dir in "${plugin_dirs[@]}"; do
  if ! manifest_info="$("$PYTHON" "$ROOT/scripts/plugin-manifest-info.py" "${dir}plugin.json")"; then
    echo "✗ bundled plugin manifest 无效，拒绝继续打包" >&2
    exit 1
  fi
  read -r _plugin_id entrypoint _bundled \
    <<<"$manifest_info"
  if [[ ! -f "${dir}${entrypoint}" ]]; then
    # 脚本插件的 entrypoint 直接来自源目录，缺了同样要当场报错。
    echo "✗ 找不到 ${dir}${entrypoint}（${dir} 的脚本入口）" >&2
    exit 1
  fi
done

# 校验是 arm64，避免误把 Intel 产物发给 Apple Silicon 同事
if ! file "$BIN" | grep -q "arm64"; then
  echo "✗ $BIN 不是 arm64，同事的 Apple Silicon Mac 会闪退。请在 Apple Silicon 上编译。" >&2
  exit 1
fi

echo "▶ 组装 $APP_NAME.app (v$VERSION) …"
rm -rf "$APP"
mkdir -p "$MACOS" "$RES"
cp "$BIN" "$MACOS/$EXEC_NAME"
chmod +x "$MACOS/$EXEC_NAME"
# 守护与 GUI 同目录（GUI 用 current_exe().with_file_name("smeltd") 寻址拉起）。
cp "$DAEMON_BIN" "$MACOS/smeltd"
chmod +x "$MACOS/smeltd"
# hooks 在 GUI 关闭时也要工作，因此启动后会把这份分发物原子同步到
# ~/.smelt/bin/smelt-notify；不能让 hook 直接引用可能被 DMG 覆盖的 App 内路径。
cp "$NOTIFY_BIN" "$MACOS/smelt-notify"
chmod +x "$MACOS/smelt-notify"
# ACP 与终端 agent 都从会话级配置启动该 STDIO MCP helper；GUI 启动后还会把它
# 原子同步到 ~/.smelt/bin，供 managed smeltd 从同目录定位。
cp "$AGENT_MCP_BIN" "$MACOS/smelt-agent-mcp"
chmod +x "$MACOS/smelt-agent-mcp"
# installer 运行前会复制到 ~/.smelt/installer/<attempt-id>/，随后等 GUI 完全退出；
# 绝不能从即将被交换的 Bundle 内直接运行。
cp "$INSTALLER_BIN" "$MACOS/smelt-installer"
chmod +x "$MACOS/smelt-installer"
# first-party 插件与 GUI 一起分发；GUI 启动时把 bundled 插件包同步到
# ~/.smelt/runtime/plugins，供同构建的 smeltd 拉起。
#
# 不能放 Contents/PlugIns：那是 macOS 嵌套 bundle（.appex / .bundle）的位置，
# codesign 会把子目录当成 bundle 校验 Info.plist，没有就会报
# "bundle format unrecognized, invalid, or unsuitable"。
PLUGIN_PACKAGES="$APP/Contents/Resources/plugin-packages"
# 每个包一份 plugin.json + bin/ + 可选 sidecar / web/ / assets/。
# sidecar 必须一起进 .app，否则面板、输入路由和智能体声明无法随插件升级。
for dir in "${plugin_dirs[@]}"; do
  # Validate before deriving any bundle destination from manifest-controlled strings.
  if ! manifest_info="$("$PYTHON" "$ROOT/scripts/plugin-manifest-info.py" "${dir}plugin.json")"; then
    echo "✗ bundled plugin manifest 无效，拒绝继续打包" >&2
    exit 1
  fi
  read -r plugin_id entrypoint _bundled \
    <<<"$manifest_info"
  target="$PLUGIN_PACKAGES/$plugin_id"
  target_entrypoint="$target/$entrypoint"
  mkdir -p "$(dirname "$target_entrypoint")"
  cp "${dir}plugin.json" "$target/plugin.json"
  if [[ -f "${dir}plugin-ui.json" ]]; then
    cp "${dir}plugin-ui.json" "$target/plugin-ui.json"
  fi
  if [[ -f "${dir}plugin-input.json" ]]; then
    cp "${dir}plugin-input.json" "$target/plugin-input.json"
  fi
  if [[ -f "${dir}plugin-agent.json" ]]; then
    cp "${dir}plugin-agent.json" "$target/plugin-agent.json"
  fi
  cp "${dir}${entrypoint}" "$target_entrypoint"
  entrypoint_src_dir="$(dirname "${dir}${entrypoint}")"
  entrypoint_dest_dir="$(dirname "$target_entrypoint")"
  if [[ -d "$entrypoint_src_dir" ]]; then
    for module in "$entrypoint_src_dir"/*; do
      [[ -f "$module" ]] || continue
      [[ "$module" == "${dir}${entrypoint}" ]] && continue
      base="$(basename "$module")"
      case "$base" in
        .*|*.test.ts|*.test.js|*.spec.ts|*.spec.js) continue ;;
      esac
      cp "$module" "$entrypoint_dest_dir/$base"
      chmod 644 "$entrypoint_dest_dir/$base"
    done
  fi
  # 入口权限必须在拷同目录模块之后设。模块一律 644；若入口也在循环里被
  # 再拷一次，755 会被盖掉。0.8.0 装更新会 load 候选 .app 里的包，入口必须 +x。
  # bun 运行时仍是 import 源文件，不 exec。
  chmod 755 "$target_entrypoint"
  if [[ -d "${dir}web" ]]; then
    cp -R "${dir}web" "$target/web"
  fi
  if [[ -d "${dir}assets" ]]; then
    cp -R "${dir}assets" "$target/assets"
  fi
  echo "  … 打包插件：${plugin_id}"
done

# 可选：把当前包对应的内部发布地址写进 App。若发布系统是在打包后才生成
# release URL，也可以不设这个变量；updater 会在成功自更新后把 URL 写入用户状态。
if [[ -n "$UPDATE_URL" ]]; then
  printf '%s\n' "$UPDATE_URL" >"$RES/SmeltUpdateURL"
fi

# 图标（可选）：存在 assets/AppIcon.icns 就带上
ICON_LINE=""
if [[ -f "$ROOT/assets/AppIcon.icns" ]]; then
  cp "$ROOT/assets/AppIcon.icns" "$RES/AppIcon.icns"
  ICON_LINE=$'\t<key>CFBundleIconFile</key>\n\t<string>AppIcon</string>'
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleExecutable</key>
	<string>${EXEC_NAME}</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<!-- DTSDKName / DTPlatformName：Xcode 打包会自动注入这两个键，我们手写 .app 没有。
	     部分分发平台用 app_info 解析 mac 包，靠 DTSDKName/DTManufacturerName 判定
	     「这是 macOS 应用」——两者都缺会抛 NotImplementedError: Unkonwn device。
	     补上即可正常识别（值本身只需非空）。 -->
	<key>DTSDKName</key>
	<string>macosx</string>
	<key>DTPlatformName</key>
	<string>macosx</string>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>${BUNDLE_ID}.file</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>smelt-file</string>
			</array>
		</dict>
	</array>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSLocalNetworkUsageDescription</key>
	<string>Smelt uses the local network to connect to development devices and remote sessions.</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<!-- 声明可打开文件夹：Dock 图标才接受拖入目录（触发 application:openURLs:）。
	     LSHandlerRank=Alternate 避免抢占系统默认的文件夹打开方式。 -->
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Folder</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.folder</string>
			</array>
		</dict>
	</array>
${ICON_LINE}
</dict>
</plist>
PLIST

# 去掉本机 quarantine，方便自测双击打开
xattr -cr "$APP" || true

# 签名。两条路：
#
# 1) Developer ID Application 身份（名字以 "Developer ID Application" 开头）：走可对外
#    分发的完整签名——Hardened Runtime（--options runtime）+ secure timestamp + 授权
#    清单（ci/mac/entitlements.plist）。这是公证（notarize）的前置：Apple 要求包里每个
#    Mach-O 都带 Hardened Runtime。公证与 staple 在 ci/mac/ci.sh 的 notarize 阶段做。
#
# 2) 自签名（scripts/setup-codesign-identity.sh 生成的 "Smelt Local Signing"）：
#    只解决本机身份稳定，过不了公证，自用/局域网分发够用。事件总线会校验正在运行
#    进程的共同签名证书，因此不能回退到各二进制身份互不相同的 ad-hoc 签名。
#
# CI 通过 SMELT_CODESIGN_IDENTITY 指定身份（发布时给 "Developer ID Application: … (TEAMID)"）。
IDENTITY="${SMELT_CODESIGN_IDENTITY:-Smelt Local Signing}"
IDENTITY_FOUND=false
if [[ "$IDENTITY" != "-" ]]; then
  if security find-identity -v -p codesigning | grep -qF "$IDENTITY"; then
    IDENTITY_FOUND=true
  elif [[ "$IDENTITY" == "Smelt Local Signing" ]] \
    && security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
    # Self-signed local certificates have no trusted chain, so find-identity may omit them even
    # though codesign can use the matching private key imported by setup-codesign-identity.sh.
    IDENTITY_FOUND=true
  fi
fi
if [[ "$IDENTITY" != "-" && "$IDENTITY_FOUND" != true ]]; then
  # Developer ID 是发布/公证场景，签不上就该立刻失败——静默回退 ad-hoc 会签出一个过不了
  # 公证的包，要拖到后面 notarize 阶段才炸，反馈太晚，还可能被误当成功件分发。
  # preflight 逐项自查，把常见根因（keychain 没解锁/没导入证书）直接摊在日志里。
  if [[ "$IDENTITY" == "Developer ID Application"* ]]; then
    echo "✗ 未找到 Developer ID 签名身份「${IDENTITY}」，发布构建拒绝回退 ad-hoc。"
    echo "  当前 codesigning 身份清单："
    security find-identity -v -p codesigning || true
    echo "  自查：1) keychain 已解锁？(security unlock-keychain)"
    echo "        2) Developer ID Application 证书+私钥已导入该 keychain？"
    echo "        3) SMELT_CODESIGN_IDENTITY 与 find-identity 里的 common-name 完全一致？"
    exit 1
  fi
  echo "✗ 未找到签名身份「${IDENTITY}」，事件总线安全认证不支持回退 ad-hoc。"
  echo "  一次性修复：./scripts/setup-codesign-identity.sh"
  exit 1
fi
if [[ "$IDENTITY" == "-" ]]; then
  echo "✗ 事件总线安全认证要求所有 Smelt 进程共享证书签名，不支持 ad-hoc 签名。"
  echo "  一次性修复：./scripts/setup-codesign-identity.sh"
  exit 1
fi

ENTITLEMENTS="$ROOT/ci/mac/entitlements.plist"
# 普通 entitlements 只放 library validation 例外。GPUI 的图形/字体栈里带着 wgpu、
# libloading、dlopen2 这类会在运行时 dlopen 系统/第三方动态库的依赖；Hardened Runtime
# 默认只允许加载「与主程序同 Team 签名」的库，一旦 dlopen 到别人签名（或系统）的库就会崩。
# 放开库验证是同类 Rust GUI 应用（如 Zed）的通行做法。不需要
# allow-unsigned-executable-memory 或 allow-jit。
# 这个文件必须是「无注释」的最精简 plist——codesign 用的是 AMFIUnserializeXML，
# 它比 plutil 严格得多，plist 里带 XML 注释会直接 "syntax error" 签名失败。
if [[ "$IDENTITY" == "Developer ID Application"* ]]; then
  echo "▶ Developer ID 签名（Hardened Runtime + 时间戳）：${IDENTITY} …"
  sign_opts=( --force --options runtime --timestamp --sign "$IDENTITY" )
  standard_sign_opts=( "${sign_opts[@]}" )
  [[ -f "$ENTITLEMENTS" ]] && standard_sign_opts+=( --entitlements "$ENTITLEMENTS" )
  # 先签内层松散 Mach-O，再封 .app。不走 --deep：Apple 已不建议，且它对 MacOS/ 下
  # 的辅助二进制处理顺序不稳；先内后外能保证每个可执行文件都带上 Hardened Runtime，
  # 否则公证会因某个内层文件缺 runtime 而整体被拒。主程序 smelt 由对 .app 的签名覆盖。
  echo "  … 签内层：smeltd"
  codesign "${standard_sign_opts[@]}" "$MACOS/smeltd"
  for inner in smelt-notify smelt-agent-mcp smelt-installer; do
    echo "  … 签内层：$inner"
    codesign "${standard_sign_opts[@]}" "$MACOS/$inner"
  done
  echo "  … 封 .app（含主程序 ${EXEC_NAME}）"
  codesign "${standard_sign_opts[@]}" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
  echo "  ✓ Developer ID 签名完成（下一步公证：./ci/mac/ci.sh notarize）"
else
  echo "▶ 自签名（身份：${IDENTITY}，仅稳定权限，过不了公证）…"
  codesign --force --deep --sign "$IDENTITY" "$APP"
  codesign --verify --deep --strict "$APP" && echo "  ✓ 签名校验通过"
fi

echo "▶ 打 dmg（定制安装窗口）…"
# 挂载后是一个固定尺寸、带背景箭头的窗口，把 app 拖到「应用程序」即完成安装。
#
# 这些定制（窗口尺寸 / 背景 / 图标坐标）最终只落在卷根目录的 .DS_Store 一个文件里。
# dmgbuild 靠 ds_store + mac_alias 直接把它写出来，全程不碰 Finder，因此本地与 CI
# （headless、没有 Finder 自动化授权）走的是同一条路径——本地打出来什么样，Release
# 就是什么样。旧版靠 AppleScript 指挥 Finder 定制，CI 上做不了只能整段跳过，发出去的
# 一直是没有背景和图标摆位的朴素 dmg。
#
# 换掉 AppleScript 顺带消掉两个旧坑：
#   - 旧脚本认死卷名做定制，撞上同名残留卷（上次没 detach 干净）会把新盘挤成
#     "Smelt 1"、定制悄悄写到旧盘上；dmgbuild 读 hdiutil 返回的真实挂载点，天然没
#     这个问题，那段「清理残留卷」也就不必要了（它还会误弹同名外置盘）。
#   - .DS_Store 由 Finder 异步写盘，旧脚本得轮询等它大小稳定才敢 detach；现在是同步
#     写文件，等待逻辑一并删掉。

# 打包工具链装进独立 venv：不污染系统 python，也绕开 PEP 668 externally-managed
# 限制（CI runner 的 python 多半是 Homebrew 装的，直接 pip install 会被拒）。
#
# venv 位置默认在工作区 dist/ 下（本地开发够用）；CI 上 env-setup.sh 会把
# SMELT_DMG_VENV 指到 $TOOLCHAIN_ROOT 里，跨次构建复用——否则每次 fresh checkout
# 都要从 PyPI 重下 dmgbuild+Pillow，和 Rust/nextest 的缓存策略就不一致了。
#
# PyPI 在国内/部分网络会 ReadTimeout（卡在 mac-alias 等依赖）。支持：
#   PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple ./scripts/package-mac.sh --build
# 未设置时：先官方，失败再自动换清华镜像。
VENV="${SMELT_DMG_VENV:-$DIST/.dmgvenv}"
# 复用判据不只看文件在不在，还要能真的 import：缓存的 venv 在底层 Python 升级/搬走
# 后，它的 python 符号链接会悬空，dmgbuild 一跑就炸。import 通过才算可用。
venv_ok() { [[ -x "$VENV/bin/dmgbuild" ]] && "$VENV/bin/python" -c 'import dmgbuild, PIL' >/dev/null 2>&1; }
if ! venv_ok; then
  echo "  … 准备打包工具链（dmgbuild + Pillow）"
  rm -rf "$VENV"
  mkdir -p "$(dirname "$VENV")"
  "$PYTHON" -m venv "$VENV"
  # 拉长超时，避免默认 15s 被掐断
  export PIP_DEFAULT_TIMEOUT="${PIP_DEFAULT_TIMEOUT:-120}"
  pip_base=( "$VENV/bin/pip" install --upgrade )
  # 用户指定镜像则只走一条；否则官方 → 清华
  if [[ -n "${PIP_INDEX_URL:-}" ]]; then
    echo "  … pip 使用 PIP_INDEX_URL=$PIP_INDEX_URL"
    "${pip_base[@]}" pip
    "${pip_base[@]}" "dmgbuild==1.6.7" "Pillow>=10"
  else
    if ! "${pip_base[@]}" --quiet pip \
      || ! "${pip_base[@]}" "dmgbuild==1.6.7" "Pillow>=10"; then
      echo "  ⚠ 官方 PyPI 失败，改用清华镜像重试 …"
      MIRROR="https://pypi.tuna.tsinghua.edu.cn/simple"
      "${pip_base[@]}" -i "$MIRROR" --trusted-host pypi.tuna.tsinghua.edu.cn pip
      "${pip_base[@]}" -i "$MIRROR" --trusted-host pypi.tuna.tsinghua.edu.cn \
        "dmgbuild==1.6.7" "Pillow>=10"
    fi
  fi
  [[ -x "$VENV/bin/dmgbuild" ]] || {
    echo "✗ 安装 dmgbuild 失败。可手动：" >&2
    echo "  PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple ./scripts/package-mac.sh --build" >&2
    exit 1
  }
  echo "  ✓ dmgbuild 已就绪"
fi

# 背景图：@1x + @2x 合成 retina 多分辨率 tiff，retina 屏上才不糊。
# Pillow 就在上面那个 venv 里，所以这里不再容错——过去是「缺 Pillow 就静默退化成
# 无背景」，而 CI runner 恰恰没有 Pillow，等于永远没背景还不报错。现在直接报错。
BG1="$DIST/.dmgbg.png"; BG2="$DIST/.dmgbg@2x.png"; BG_TIFF="$DIST/.dmgbg.tiff"
rm -f "$BG_TIFF"
"$VENV/bin/python" "$ROOT/scripts/make-dmg-bg.py" "$BG1" "$BG2" >/dev/null
tiffutil -cathidpicheck "$BG1" "$BG2" -out "$BG_TIFF" >/dev/null
rm -f "$BG1" "$BG2"
[[ -f "$BG_TIFF" ]] || { echo "✗ 背景图生成失败，没产出 $BG_TIFF" >&2; exit 1; }

VOL="$APP_NAME"
rm -f "$DIST/$APP_NAME.dmg"

# dmgbuild 内部仍要 attach 一个可写映像来放文件，hdiutil 的挂载/卸载在 runner 上会
# 偶发撞车——v0.4.5 两次发布分别挂在 `create failed - Resource busy` 和 `convert
# failed - Resource temporarily unavailable`，失败在不同步骤，是典型的资源竞争而非
# 固定 bug。竞争面消不掉，用重试兜住。
export SMELT_APP="$APP"
export SMELT_DMG_BG="$BG_TIFF"
export SMELT_VOL_ICON="$ROOT/assets/AppIcon.icns"
built=0
for attempt in 1 2 3; do
  if "$VENV/bin/dmgbuild" -s "$ROOT/scripts/dmg-settings.py" "$VOL" "$DIST/$APP_NAME.dmg"; then
    built=1
    break
  fi
  echo "  ⚠ 第 ${attempt}/3 次打 dmg 失败（多半是 hdiutil 挂载竞争），3s 后重试 …"
  hdiutil detach "/Volumes/$VOL" -force >/dev/null 2>&1 || true
  sleep 3
done
rm -f "$BG_TIFF"
[[ "$built" == 1 ]] || { echo "✗ dmg 打包连续 3 次失败" >&2; exit 1; }

echo ""
echo "✅ 完成"
echo "   应用：   $APP"
echo "   分发件： $DIST/$APP_NAME.dmg"
