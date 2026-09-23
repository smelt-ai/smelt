# Smelt 打包与开发入口。打包重活在 scripts/package-mac.sh 里。
# GUI 会拉起同目录的 smeltd；只编 GUI 会留下过期/缺失的守护，表现为
# 「新建终端 / 打开项目没反应」。
BIN := smelt
DAEMON := smeltd

.PHONY: help build run icon dist dist-build install clean fmt fmt-check lint lint-sh test sdk-check pi-agent-check check-all

build run install: SHELL := /bin/bash

help: ## 显示可用命令
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

fmt: ## 格式化所有 Rust 代码
	cargo fmt

fmt-check: ## 检查代码格式
	cargo fmt --check

lint: ## 运行 Clippy 静态检查（正确性 lint 由 workspace.lints deny）
	cargo clippy --workspace --all-targets

lint-sh: ## 检查 shell 脚本里 $$var 紧跟非 ASCII 的写法
	@# macOS 自带 bash 3.2 在中文 locale 下，会把紧跟在 `$plugin_id` 后面的全角括号
	@# 字节当成变量名的一部分，于是 set -u 直接报 "unbound variable"。ASCII locale 下
	@# 完全看不出来，所以只能靠这条检查兜住——本仓 echo 里中文很多，必然复发。
	@# 修法是加花括号：$${plugin_id}（…）。
	@out=$$(LC_ALL=C find scripts ci packages plugins -name '*.sh' -print0 2>/dev/null \
		| LC_ALL=C xargs -0 grep -nE '\$$[A-Za-z_][A-Za-z0-9_]*[^[:print:][:space:]]'); \
	if [ -n "$$out" ]; then \
		echo "✗ shell 变量后紧跟非 ASCII 字符，请改成 \$${var}："; \
		echo "$$out"; exit 1; \
	fi
	@echo "✓ shell 脚本变量引用检查通过"

test: ## 运行全工作区单元测试（nextest：smeltd handoff 不能同进程跑）
	@cargo nextest --version >/dev/null 2>&1 || { \
		echo "✗ 需要 cargo-nextest（与 CI 同一运行器）。安装：cargo install cargo-nextest --locked"; exit 1; }
	cargo nextest run --workspace

sdk-check: ## 检查 TS 插件 SDK（类型 + 单测）
	@bun="$$(./scripts/bun.sh)" || { \
		echo "✗ 找不到 bun。跑一次 GUI 会自动装受管 bun，或自行安装到 PATH"; exit 1; }; \
	cd packages/plugin-sdk && "$$bun" install --silent && "$$bun" run typecheck && "$$bun" run test
	@# 打真 tarball、装进空项目并用不写 types 的 tsconfig 编译，验证第三方通过
	@# package.json 的 exports/types 消费发布产物时不会偷看到 SDK 的 devDependencies。
	@./scripts/sdk-consumer-check.sh

pi-agent-check: ## 检查内置 Pi Agent（类型 + 原生 RPC/权限单测）
	@bun="$$(./scripts/bun.sh)" || { \
		echo "✗ 找不到 bun。跑一次 GUI 会自动装受管 bun，或自行安装到 PATH"; exit 1; }; \
	cd packages/pi-agent && "$$bun" install --frozen-lockfile --silent && "$$bun" run check

check-all: fmt-check lint lint-sh test sdk-check pi-agent-check ## 完整检查 (格式 + Clippy + shell + Rust/TS 测试)

build: ## 编译 release 二进制（GUI + 守护 + helpers）
	@./scripts/bundled-plugins.sh dirs >/dev/null || { \
		echo "✗ bundled plugin manifest 或 entrypoint 无效，拒绝构建"; exit 1; \
	}
	cargo build --release --bin $(BIN) --bin $(DAEMON) --bin smelt-notify --bin smelt-agent-mcp --bin smelt-sync-plugins --bin smelt-installer

run: ## 本地直接跑 GUI（开发用）
	@./scripts/bundled-plugins.sh dirs >/dev/null || { \
		echo "✗ bundled plugin manifest 或 entrypoint 无效，拒绝构建"; exit 1; \
	}
	cargo build --bin $(BIN) --bin $(DAEMON) --bin smelt-notify --bin smelt-agent-mcp --bin smelt-installer
	cargo run --bin $(BIN)

icon: ## 生成 app 图标（assets/AppIcon.icns）
	./scripts/make-icon.sh

dist: ## 用已有 release 产物打包 app + dmg
	./scripts/package-mac.sh

dist-build: ## 先编 release 再打包（一步到位）
	./scripts/package-mac.sh --build

install: ## 原子安装到 /Applications；活跃 ACP 不阻塞，旧 runtime 首次迁移在后台完成
	@# make build && make install 必须装刚编的二进制；只检查 dist 是否存在会把
	@# 旧包再装一遍，守护永远跑不到新代码，插件映射竞态也修不进去。Shared Bun
	@# package 没有 cargo binary，因此同时检查整个 bundled package 的源内容。
	@./scripts/bundled-plugins.sh dirs >/dev/null || { \
		echo "✗ bundled plugin manifest 或 entrypoint 无效，拒绝安装"; exit 1; \
	}
	@[ -f target/release/$(BIN) ] && [ -f target/release/$(DAEMON) ] \
		&& [ -f target/release/smelt-notify ] && [ -f target/release/smelt-agent-mcp ] \
		&& [ -f target/release/smelt-installer ] || { \
		echo "✗ 缺少 release 产物，先 make build"; exit 1; }
	if [ -d dist/Smelt.app/Contents/Resources/plugin-packages ]; then \
		if ! plugin_sources_changed="$$(./scripts/bundled-plugins.sh newer-than dist/Smelt.app/Contents/Resources/plugin-packages)"; then \
			echo "✗ bundled plugin package 状态无效，拒绝安装旧 dist"; exit 1; \
		fi; \
	else \
		plugin_sources_changed=missing; \
	fi; \
	if [ ! -d dist/Smelt.app ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/$(DAEMON) ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/$(BIN) ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/smelt-notify ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/smelt-agent-mcp ] \
		|| [ ! -x dist/Smelt.app/Contents/MacOS/smelt-installer ] \
		|| [ target/release/$(DAEMON) -nt dist/Smelt.app/Contents/MacOS/$(DAEMON) ] \
		|| [ target/release/$(BIN) -nt dist/Smelt.app/Contents/MacOS/$(BIN) ] \
		|| [ target/release/smelt-notify -nt dist/Smelt.app/Contents/MacOS/smelt-notify ] \
		|| [ target/release/smelt-agent-mcp -nt dist/Smelt.app/Contents/MacOS/smelt-agent-mcp ] \
		|| [ target/release/smelt-installer -nt dist/Smelt.app/Contents/MacOS/smelt-installer ] \
		|| [ -n "$$plugin_sources_changed" ]; then \
		echo "· dist 落后于 release 或 bundled plugin package，重新打包"; \
		./scripts/package-mac.sh; \
	fi
	@# 存量 make install 仍走旧的显式安装入口；在线更新已改由进程外 installer
	@# 在 GUI 完全退出后提交。这里保留 daemon handoff 语义，后续再收敛到同一 core。
	@./target/release/$(BIN) --install-app dist/Smelt.app /Applications/Smelt.app
	@echo "✅ 安装完成"

clean: ## 清理 dist/ 产物
	rm -rf dist
