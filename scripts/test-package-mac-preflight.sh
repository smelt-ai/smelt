#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="$ROOT/target/package-mac-preflight-$$"
rm -rf "$FIXTURE"
trap 'rm -rf "$FIXTURE"' EXIT

mkdir -p "$FIXTURE/scripts"
cp "$ROOT/scripts/package-mac.sh" "$FIXTURE/scripts/package-mac.sh"
cp "$ROOT/scripts/bundled-plugins.sh" "$FIXTURE/scripts/bundled-plugins.sh"
cp "$ROOT/scripts/plugin-manifest-info.py" "$FIXTURE/scripts/plugin-manifest-info.py"
cp "$ROOT/Makefile" "$FIXTURE/Makefile"
cp "$ROOT/Cargo.toml" "$FIXTURE/Cargo.toml"
mkdir -p "$FIXTURE/target/release"
: >"$FIXTURE/target/release/smelt"
: >"$FIXTURE/target/release/smeltd"
: >"$FIXTURE/target/release/smelt-notify"
: >"$FIXTURE/target/release/smelt-agent-mcp"
mkdir -p "$FIXTURE/plugins/example/bin"
printf '%s\n' \
  '{"id":"com.example","entrypoint":"bin/example-plugin","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
: >"$FIXTURE/plugins/example/bin/example-plugin"

make_fake_python() {
  local path="$1"
  local version="$2"
  local status="$3"
  local real_python
  real_python="$(command -v python3)"
  # 这里刻意把参数表达式原样写进生成的 shim，由 shim 运行时展开。
  # shellcheck disable=SC2016
  printf '#!/usr/bin/env bash\nif [[ "${2:-}" == *"sys.version_info"* ]]; then\n  printf "%%s\\n" "%s"\n  exit %s\nfi\nexec "%s" "$@"\n' \
    "$version" "$status" "$real_python" >"$path"
  chmod +x "$path"
}

make_stopping_python() {
  local path="$1"
  local real_python
  real_python="$(command -v python3)"
  # 在签名阶段之后刻意停止，避免预检因安装 dmgbuild 而访问网络。
  # shellcheck disable=SC2016
  printf '#!/usr/bin/env bash\nif [[ "${2:-}" == *"sys.version_info"* ]]; then\n  printf "%%s\\n" "3.12.7"\n  exit 0\nfi\nif [[ "${1:-}" == "-m" && "${2:-}" == "venv" ]]; then\n  echo "test stop after signing" >&2\n  exit 1\nfi\nexec "%s" "$@"\n' \
    "$real_python" >"$path"
  chmod +x "$path"
}

make_signing_shims() {
  local shim_dir="$FIXTURE/signing-shims"
  mkdir -p "$shim_dir"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s: Mach-O 64-bit executable arm64\n" "$1"' \
    >"$shim_dir/file"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "  1) FAKEIDENTITY \"Smelt Local Signing\"\n"' \
    >"$shim_dir/security"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'exit 0' \
    >"$shim_dir/codesign"
  chmod +x "$shim_dir/file" "$shim_dir/security" "$shim_dir/codesign"
}

run_package_script() {
  local python="$1"
  set +e
  OUTPUT="$(SMELT_PYTHON="$python" "$FIXTURE/scripts/package-mac.sh" 2>&1)"
  STATUS=$?
  set -e
}

run_package_script_through_signing() {
  local python="$1"
  set +e
  OUTPUT="$(PATH="$FIXTURE/signing-shims:$PATH" SMELT_PYTHON="$python" "$FIXTURE/scripts/package-mac.sh" 2>&1)"
  STATUS=$?
  set -e
}

run_make_install() {
  set +e
  OUTPUT="$(make -C "$FIXTURE" install 2>&1)"
  STATUS=$?
  set -e
}

assert_failed_with() {
  local expected="$1"
  if [[ "$STATUS" -eq 0 || "$OUTPUT" != *"$expected"* ]]; then
    echo "✗ 预期失败并包含：$expected" >&2
    echo "$OUTPUT" >&2
    exit 1
  fi
}

run_package_script "$FIXTURE/missing-python"
assert_failed_with "找不到 Python 解释器"

make_fake_python "$FIXTURE/python3.9" "3.9.6" 1
run_package_script "$FIXTURE/python3.9"
assert_failed_with "需要 Python >= 3.10"
assert_failed_with "3.9.6"

make_fake_python "$FIXTURE/python3.12" "3.12.7" 0
run_package_script "$FIXTURE/python3.12"
assert_failed_with "不是 arm64"
if [[ "$OUTPUT" == *"需要 Python >= 3.10"* ]]; then
  echo "✗ Python 3.12 不应被版本检查拒绝" >&2
  exit 1
fi

printf '%s\n' \
  '{"id":"com.example","entrypoint":"bin/../../Contents/MacOS/smeltd","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
run_package_script "$FIXTURE/python3.12"
assert_failed_with "entrypoint is invalid"
if [[ -e "$FIXTURE/dist/Smelt.app" ]]; then
  echo "✗ 非法 entrypoint 在创建 App bundle 前必须被拒绝" >&2
  exit 1
fi

printf '%s\n' \
  '{"id":"../../outside","entrypoint":"bin/example-plugin","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
run_package_script "$FIXTURE/python3.12"
assert_failed_with "id is invalid"
if [[ -e "$FIXTURE/dist/Smelt.app" ]]; then
  echo "✗ 非法 plugin id 在创建 App bundle 前必须被拒绝" >&2
  exit 1
fi

printf '%s\n' \
  '{"id":"com.example","execution":"shared","entrypoint":"bin/example-plugin","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
run_package_script "$FIXTURE/python3.12"
assert_failed_with "execution is obsolete"

printf '%s\n' \
  '{"id":"com.example","execution":"dedicated","entrypoint":"bin/example-plugin","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
run_package_script "$FIXTURE/python3.12"
assert_failed_with "execution is obsolete"

printf '%s\n' \
  '{"id":"com.example","entrypoint":"bin/main.ts","bundled":true}' \
  >"$FIXTURE/plugins/example/plugin.json"
mkdir -p "$FIXTURE/plugins/example/bin"
: >"$FIXTURE/plugins/example/bin/main.ts"
if ! "$FIXTURE/scripts/bundled-plugins.sh" dirs | grep -Fqx "$(cd "$FIXTURE/plugins/example" && pwd)"; then
  echo "✗ 缺省 bun 的 Shared Bun package 必须出现在 bundled package 目录列表" >&2
  exit 1
fi
rm -f "$FIXTURE/plugins/example/bin/main.ts"
run_package_script "$FIXTURE/python3.12"
assert_failed_with "缺少或非法的 package entrypoint"
run_make_install
assert_failed_with "缺少或非法的 package entrypoint"

: >"$FIXTURE/plugins/example/bin/main.ts"
: >"$FIXTURE/plugins/example/bin/api.ts"
make_signing_shims
make_stopping_python "$FIXTURE/python-stop-after-signing"
run_package_script_through_signing "$FIXTURE/python-stop-after-signing"
assert_failed_with "test stop after signing"
if [[ "$OUTPUT" != *"▶ 打 dmg（定制安装窗口）…"* ]]; then
  echo "✗ 仅含共享插件的包必须通过签名阶段" >&2
  echo "$OUTPUT" >&2
  exit 1
fi

packaged_entrypoint="$FIXTURE/dist/Smelt.app/Contents/Resources/plugin-packages/com.example/bin/main.ts"
packaged_module="$FIXTURE/dist/Smelt.app/Contents/Resources/plugin-packages/com.example/bin/api.ts"
entrypoint_mode="$(stat -f '%Lp' "$packaged_entrypoint")"
module_mode="$(stat -f '%Lp' "$packaged_module")"
if [[ "$entrypoint_mode" != "755" ]]; then
  echo "✗ bun 入口必须打成 755，且不能被同目录模块拷贝盖成 644（实际 ${entrypoint_mode}）" >&2
  exit 1
fi
if [[ "$module_mode" != "644" ]]; then
  echo "✗ bun 同目录模块必须打成 644（实际 ${module_mode}）" >&2
  exit 1
fi

echo "✓ package-mac Python 和 plugin manifest 前置检查通过"
