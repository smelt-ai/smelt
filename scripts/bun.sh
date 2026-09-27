#!/usr/bin/env bash
# Resolve the source-locked managed Bun generation, printing its executable path.
#
# `--managed-only` forbids PATH fallback and is used by the embedded Pi production smoke.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_file="$root/crates/smelt-core/src/managed_runtime.rs"
version="$(awk -F'"' '/^pub\(crate\) const BUN_VERSION: &str = "/ { print $2; exit }' "$source_file")"
case "$(uname -m)" in
  arm64) rust_arch="aarch64" ;;
  x86_64) rust_arch="x86_64" ;;
  *) rust_arch="" ;;
esac
expected_sha="$(awk -v arch="$rust_arch" '
  /^#\[cfg\(all\(target_os = "macos", target_arch = "/ {
    matching = arch != "" && index($0, "target_arch = \"" arch "\"") > 0
    next
  }
  matching && /^pub\(crate\) const BUN_EXECUTABLE_SHA256: &str =/ {
    getline
    gsub(/[";]/, "")
    gsub(/^[[:space:]]+|[[:space:]]+$/, "")
    print
    exit
  }
' "$source_file")"
release="$(awk -v arch="$rust_arch" '
  /^#\[cfg\(all\(target_os = "macos", target_arch = "/ {
    matching = arch != "" && index($0, "target_arch = \"" arch "\"") > 0
    next
  }
  matching && /^const BUN_DOWNLOAD:/ {
    getline
    url = $0
    sub(/^[[:space:]]*"/, "", url)
    sub(/".*$/, "", url)
    getline
    sha = $0
    sub(/^[[:space:]]*"/, "", sha)
    sub(/".*$/, "", sha)
    print url "\t" sha
    exit
  }
' "$source_file")"
IFS=$'\t' read -r download_url archive_sha <<< "$release"
platform="macos"
[[ -n "$version" && -n "$expected_sha" && -n "$download_url" && -n "$archive_sha" ]] || {
  echo "无法读取当前架构的受管 Bun 锁定信息" >&2
  exit 1
}

# Match managed_runtime.rs::sha256_parts: each UTF-8 field is prefixed by a u64 LE length.
identity_sha="$(perl -MDigest::SHA=sha256_hex -e '
  my $bytes = "";
  for my $part (@ARGV) { $bytes .= pack("Q<", length($part)) . $part; }
  print sha256_hex($bytes);
' "$version" "$platform" "$rust_arch" "$expected_sha" "$download_url" "$archive_sha")"
expected_id="bun-$identity_sha"
runtime_root="${HOME}/.smelt/runtime"
current_file="$runtime_root/current/bun"
current_id="$(cat "$current_file" 2>/dev/null || true)"
candidate="$runtime_root/store/bun/$current_id/bun"
manifest="$runtime_root/store/bun/$current_id/manifest.json"
ready="$runtime_root/store/bun/$current_id/READY"
managed_valid=false
if [[ "$current_id" == "$expected_id" \
  && -f "$candidate" && ! -L "$candidate" && -x "$candidate" \
  && -f "$manifest" && ! -L "$manifest" \
  && -f "$ready" && ! -L "$ready" ]]; then
  actual_sha="$(shasum -a 256 "$candidate" | awk '{ print $1 }')"
  actual_version="$("$candidate" --version 2>/dev/null || true)"
  if [[ "$actual_sha" == "$expected_sha" && "$actual_version" == "$version" ]] \
    && grep -Fq "\"generation_id\": \"$expected_id\"" "$manifest" \
    && grep -Fq "\"version\": \"$version\"" "$manifest" \
    && grep -Fq "\"platform\": \"$platform\"" "$manifest" \
    && grep -Fq "\"arch\": \"$rust_arch\"" "$manifest" \
    && grep -Fq "\"executable_sha256\": \"$expected_sha\"" "$manifest" \
    && grep -Fq "\"download_url\": \"$download_url\"" "$manifest" \
    && grep -Fq "\"archive_sha256\": \"$archive_sha\"" "$manifest" \
    && grep -Fq 'ready' "$ready"; then
    managed_valid=true
  fi
fi
if $managed_valid; then
  echo "$candidate"
  exit 0
fi

if [[ "${1:-}" == "--managed-only" ]]; then
  echo "锁定版本的受管 Bun generation 不存在或完整性校验失败：$candidate" >&2
  exit 1
fi

if command -v bun >/dev/null 2>&1; then
  command -v bun
  exit 0
fi

exit 1
