#!/usr/bin/env bash
# 用**发布产物**验证第三方插件作者的零配置起步体验。
#
# 不能只检查 SDK 自身：仓内包能看见自己的 devDependencies，而真实第三方拿到的是
# npm tarball（只含 files 白名单、不含 devDeps），tsconfig 多半也是 `bun init` 的
# 默认值。这两条通路的差异曾产生 15 个类型错误，全部来自 SDK 自己的源码。
#
# 所以这里刻意：打真 tarball、装进空项目、用不写 types 的 tsconfig 编译。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK="$ROOT/packages/plugin-sdk"

BUN="$("$ROOT/scripts/bun.sh")" || {
  echo "✗ 找不到 bun。跑一次 GUI 会自动装受管 bun，或自行安装到 PATH" >&2
  exit 1
}

WORK="$ROOT/target/sdk-consumer-check-$$"
rm -rf "$WORK"
trap 'rm -rf "$WORK"' EXIT

# pack 必须在包目录里跑；产物名由 name+version 决定，别写死。
(cd "$SDK" && "$BUN" pm pack --destination "$WORK" >/dev/null)
TARBALL="$(find "$WORK" -maxdepth 1 -name '*.tgz' | head -1)"
[[ -n "$TARBALL" ]] || { echo "✗ 打包 SDK 失败" >&2; exit 1; }
cd "$WORK"

cat >package.json <<'JSON'
{ "name": "smelt-sdk-consumer-check", "type": "module", "private": true }
JSON

# 关键：不写 "types"。这正是第三方最可能的配置。
cat >tsconfig.json <<'JSON'
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "Preserve",
    "moduleResolution": "bundler",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true
  }
}
JSON

# 覆盖 README 首屏那段代码：共享 Bun 模块的默认导出契约。
cat >main.ts <<'TS'
import {
  InvocationFailure,
  type InvocationRequest,
  type SharedPlugin,
} from "@smelt-ai/plugin-sdk";

const plugin: SharedPlugin = {
  async invoke(request: InvocationRequest, context) {
  if (request.operation !== "greet") {
    throw new InvocationFailure("invalid_request", `unknown ${request.operation}`);
  }
  return { pluginId: context.pluginId, dataDir: context.dataDir };
  },
};

export default plugin;
TS

"$BUN" add "$TARBALL" >/dev/null 2>&1

if ! output="$("$BUN" x tsc -p tsconfig.json --noEmit 2>&1)"; then
  echo "✗ 第三方零配置消费 SDK 失败——发布出去第一批插件作者就会撞上：" >&2
  echo "$output" >&2
  exit 1
fi

echo "✓ SDK 发布产物可被零配置消费"
