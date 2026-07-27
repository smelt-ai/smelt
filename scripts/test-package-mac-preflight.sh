#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/smelt-package-preflight.XXXXXX")"
trap 'rm -rf "$FIXTURE"' EXIT

mkdir -p "$FIXTURE/scripts"
cp "$ROOT/scripts/package-mac.sh" "$FIXTURE/scripts/package-mac.sh"
cp "$ROOT/Cargo.toml" "$FIXTURE/Cargo.toml"
mkdir -p "$FIXTURE/target/release" "$FIXTURE/remote-web/dist"
: >"$FIXTURE/target/release/smelt"
: >"$FIXTURE/target/release/smeltd"
: >"$FIXTURE/target/release/smelt-bridge"
: >"$FIXTURE/remote-web/dist/index.html"

make_fake_python() {
  local path="$1"
  local version="$2"
  local status="$3"
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "%s"\nexit %s\n' \
    "$version" "$status" >"$path"
  chmod +x "$path"
}

run_package_script() {
  local python="$1"
  set +e
  OUTPUT="$(SMELT_PYTHON="$python" "$FIXTURE/scripts/package-mac.sh" 2>&1)"
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

echo "✓ package-mac Python 前置检查通过"
