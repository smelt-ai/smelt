# Smelt 打包与开发入口。打包重活在 scripts/package-mac.sh 里。
# GUI 会拉起同目录的 smeltd；只编 GUI 会留下过期/缺失的守护，表现为
# 「新建终端 / 打开项目没反应」。
BIN := smelt
DAEMON := smeltd

.PHONY: help build icon dist dist-build install dev clean fmt fmt-check lint lint-sh lint-paths test test-sh pi-agent-check check-all smelt-pi ios

SMELT_PI := target/smelt-pi/smelt-pi
PI_AGENT_SOURCES := $(shell find packages/pi-agent/src -type f -name '*.ts') $(shell find packages/pi-agent/skills -type f)

build install: SHELL := /bin/bash

help: ## 显示可用命令
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

fmt: ## 格式化所有 Rust 代码
	cargo fmt

fmt-check: ## 检查代码格式
	cargo fmt --check

lint: ## 运行 Clippy 静态检查（正确性 lint 由 workspace.lints deny）
	./scripts/lint-pointer.sh
	cargo clippy --workspace --all-targets

lint-sh: ## 检查 shell 脚本里 $$var 紧跟非 ASCII 的写法
	@# macOS 自带 bash 3.2 在中文 locale 下，会把紧跟在 `$arch` 后面的全角括号
	@# 字节当成变量名的一部分，于是 set -u 直接报 "unbound variable"。ASCII locale 下
	@# 完全看不出来，所以只能靠这条检查兜住——本仓 echo 里中文很多，必然复发。
	@# 修法是加花括号：$${arch}（…）。只查纳入版本管理的脚本，不碰 node_modules。
	@out=$$(git ls-files -z '*.sh' \
		| LC_ALL=C xargs -0 grep -nE '\$$[A-Za-z_][A-Za-z0-9_]*[^[:print:][:space:]]'); \
	if [ -n "$$out" ]; then \
		echo "✗ shell 变量后紧跟非 ASCII 字符，请改成 \$${var}："; \
		echo "$$out"; exit 1; \
	fi
	@echo "✓ shell 脚本变量引用检查通过"

lint-paths: ## 检查源码中硬编码的开发者本机绝对路径（/Users/<personal>）
	@# 单测与源码禁止硬编码个人用户主目录。虚拟路径请使用 /workspace/... 或 /Users/{me,test,mock,example,shared}。
	@out=$$(git grep -nE '/Users/[a-zA-Z0-9._-]+' -- \
		':(exclude)node_modules' ':(exclude)vendor' ':(exclude)dist' ':(exclude)target' \
		| grep -vE '/Users/(me|test|example|someone|shared|ci|mock|x|a|中文)(/|"|'"'"')' || true); \
	if [ -n "$$out" ]; then \
		echo "✗ 发现源码中硬编码了个人本机主目录路径，请改用通用虚拟路径（/workspace/... 或 /Users/me/...）："; \
		echo "$$out"; exit 1; \
	fi
	@echo "✓ 本地绝对路径合规检查通过"

test: ## 运行全工作区单元测试（nextest：smeltd handoff 不能同进程跑）
	@cargo nextest --version >/dev/null 2>&1 || { \
		echo "✗ 需要 cargo-nextest（与 CI 同一运行器）。安装：cargo install cargo-nextest --locked"; exit 1; }
	cargo nextest run --workspace

# package.json 的脚本按名字调 bun：钉死版本放到 PATH 最前面，子脚本才用同一个。
pi-agent-check: ## 检查内置 Pi Agent（类型 + 原生 RPC/权限单测）
	@bun="$$(./scripts/bun.sh --managed-only)" || { \
		echo "✗ 取不到钉死版本的 Bun（scripts/fetch-bun.sh）"; exit 1; }; \
	export PATH="$$(dirname "$$bun"):$$PATH"; \
	cd packages/pi-agent && "$$bun" install --frozen-lockfile --silent \
		&& "$$bun" run check \
		&& "$$bun" run production-smoke

test-sh: ## 运行打包、安装、部署脚本的 shell 测试（scripts/test-*.sh）
	@set -e; for t in scripts/test-*.sh; do echo "· $$t"; bash "$$t"; done

# 不含 fmt-check：仓库还没整体 rustfmt 干净过，CI 也不查格式（见 ci.yml）。
check-all: lint lint-sh lint-paths test test-sh pi-agent-check ## 完整检查 (Clippy + shell + 路径合规 + Rust/TS/脚本测试)

$(SMELT_PI): packages/pi-agent/package.json packages/pi-agent/bun.lock scripts/compile-smelt-pi.ts scripts/build-smelt-pi.sh scripts/fetch-bun.sh $(PI_AGENT_SOURCES)
	./scripts/build-smelt-pi.sh

ios: ## 在发版 tag 上出 iOS 发布包（版本号跟发版统一，见 docs/release.md）
	scripts/build-ios.sh

smelt-pi: $(SMELT_PI) ## 编译 smelt-pi；源码、锁和编译脚本没变就跳过

build: $(SMELT_PI) ## 编译 release 二进制（GUI + 守护 + helpers + smelt-pi）
	cargo build --release --bin $(BIN) --bin $(DAEMON) --bin smelt-notify --bin smelt-installer

icon: ## 生成 app 图标（assets/AppIcon.icns）
	./scripts/make-icon.sh

dist: ## 用已有 release 产物打包 app + dmg
	./scripts/package-mac.sh

dist-build: ## 先编 release 再打包（一步到位）
	./scripts/package-mac.sh --build

install: $(SMELT_PI) ## 原子安装到 /Applications（不打 dmg）；活跃 ACP 不阻塞，旧 runtime 首次迁移在后台完成
	@# make build && make install 必须装刚编的二进制；只检查 dist 是否存在会把
	@# 旧包再装一遍，守护永远跑不到新代码。插件脚本没有 cargo binary，因此同时
	@# 检查 plugins 的源内容（按 .gitignore 排除编辑器生成文件和本地 clone 的外部仓库）。
	@# codesign -dv 不打印证书名，Authority= 从 -dvv 才出现。少这一级时已签好的包也会被当成签名不对。
	@[ -f target/release/$(BIN) ] && [ -f target/release/$(DAEMON) ] \
		&& [ -f target/release/smelt-notify ] \
		&& [ -f target/release/smelt-installer ] || { \
		echo "✗ 缺少 release 产物，先 make build"; exit 1; }
	@if [ -d dist/Smelt.app/Contents/Resources/plugins ]; then \
		plugin_sources_changed="$$(git ls-files -co --exclude-standard -- plugins | while read -r f; do \
			[ "$$f" -nt dist/Smelt.app/Contents/Resources/plugins ] && { echo "$$f"; break; }; done)"; \
	else \
		plugin_sources_changed=missing; \
	fi; \
	sig_ok=0; \
	identity="$${SMELT_CODESIGN_IDENTITY:-Smelt Local Signing}"; \
	if [ -d dist/Smelt.app ]; then \
		sig="$$(codesign -dvv dist/Smelt.app 2>&1 || true)"; \
		case "$$sig" in \
			*Signature=adhoc*|*linker-signed*) ;; \
			*) printf '%s\n' "$$sig" | grep -F -q "Authority=$$identity" && sig_ok=1 ;; \
		esac; \
	fi; \
	if [ ! -d dist/Smelt.app ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/$(DAEMON) ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/$(BIN) ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/smelt-notify ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/smelt-installer ] \
		|| [ target/release/$(DAEMON) -nt dist/Smelt.app/Contents/MacOS/$(DAEMON) ] \
		|| [ target/release/$(BIN) -nt dist/Smelt.app/Contents/MacOS/$(BIN) ] \
		|| [ target/release/smelt-notify -nt dist/Smelt.app/Contents/MacOS/smelt-notify ] \
		|| [ target/release/smelt-installer -nt dist/Smelt.app/Contents/MacOS/smelt-installer ] \
		|| [ ! -x dist/Smelt.app/Contents/Resources/smelt-pi/smelt-pi ] \
		|| [ target/smelt-pi/smelt-pi -nt dist/Smelt.app/Contents/Resources/smelt-pi/smelt-pi ] \
		|| [ target/smelt-pi/photon_rs_bg.wasm -nt dist/Smelt.app/Contents/Resources/smelt-pi/photon_rs_bg.wasm ] \
		|| ! diff -rq target/smelt-pi/skills dist/Smelt.app/Contents/Resources/smelt-pi/skills >/dev/null 2>&1 \
		|| [ -n "$$plugin_sources_changed" ] \
		|| [ "$$sig_ok" -ne 1 ]; then \
		echo "· dist 落后于 release、签名不对，或插件脚本有改动，重新打包"; \
		./scripts/package-mac.sh --no-dmg; \
	fi
	@# 存量 make install 仍走旧的显式安装入口；在线更新已改由进程外 installer
	@# 在 GUI 完全退出后提交。这里保留 daemon handoff 语义，后续再收敛到同一 core。
	@./target/release/$(BIN) --install-app dist/Smelt.app /Applications/Smelt.app
	@echo "✅ 安装完成"

dev: ## 编译 release 并原子安装到 /Applications（= make build 后 make install）
	@# 不写成 `dev: build install` 依赖：make -j 会把两者并行，install 可能装上旧产物。
	@$(MAKE) --no-print-directory build
	@$(MAKE) --no-print-directory install

clean: ## 清理 make build / make install 的构建产物（target/ 与 dist/，不卸载已安装的 App）
	@set -eu; \
	target_dir="$${CARGO_TARGET_DIR:-target}"; \
	kb_of() { if [ -e "$$1" ]; then du -sk "$$1" | awk '{print $$1}'; else echo 0; fi; }; \
	human() { \
		kb="$$1"; \
		if [ "$$kb" -ge 1048576 ]; then awk -v k="$$kb" 'BEGIN { printf "%.1f GB", k/1048576 }'; \
		elif [ "$$kb" -ge 1024 ]; then awk -v k="$$kb" 'BEGIN { printf "%.1f MB", k/1024 }'; \
		else printf '%s KB' "$$kb"; fi; \
	}; \
	before_target=$$(kb_of "$$target_dir"); \
	before_dist=$$(kb_of dist); \
	cargo clean; \
	rm -rf dist; \
	after_target=$$(kb_of "$$target_dir"); \
	after_dist=$$(kb_of dist); \
	freed_target=$$((before_target - after_target)); \
	freed_dist=$$((before_dist - after_dist)); \
	if [ "$$freed_target" -lt 0 ]; then freed_target=0; fi; \
	if [ "$$freed_dist" -lt 0 ]; then freed_dist=0; fi; \
	freed=$$((freed_target + freed_dist)); \
	if [ "$$freed" -eq 0 ]; then \
		echo "没有可清理的构建产物"; \
	else \
		parts=""; \
		if [ "$$freed_target" -gt 0 ]; then parts="target $$(human $$freed_target)"; fi; \
		if [ "$$freed_dist" -gt 0 ]; then \
			if [ -n "$$parts" ]; then parts="$${parts}，dist $$(human $$freed_dist)"; else parts="dist $$(human $$freed_dist)"; fi; \
		fi; \
		echo "✅ 已清理 $$(human $$freed)（$${parts}）"; \
	fi
