#!/usr/bin/env bash
# 把 managed_runtime.rs 钉死的 Bun 准备到仓库缓存，并打印可执行文件路径。
# 开发和打包用。产品首次启动不走这里，也不回退到系统 Bun。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=runtime-target.sh
source "$root/scripts/runtime-target.sh"
runtime_target

source_file="$root/crates/smelt-core/src/managed_runtime.rs"
cache="$root/.cache/bun"
mkdir -p "$cache"

eval "$(
  perl - "$RUNTIME_OS" "$RUNTIME_ARCH" "$source_file" <<'PERL'
use strict;
use warnings;
my ($os, $arch, $path) = @ARGV;
open my $fh, "<:utf8", $path or die "无法读取 $path: $!";
local $/;
my $text = <$fh>;
my ($version) = $text =~ /const BUN_VERSION: &str = "([^"]+)"/;
die "读不到 BUN_VERSION\n" unless $version;
my ($url, $archive_sha, $zip_dir, $exe_sha);
while ($text =~ /#\[cfg\(all\((.*?)\)\)\]\s*(?:pub\(crate\)\s*)?const\s+(BUN_DOWNLOAD|BUN_ZIP_DIR|BUN_EXECUTABLE_SHA256)\b(.*?);/sg) {
  my ($cfg, $kind, $body) = ($1, $2, $3);
  next unless $cfg =~ /target_os = "\Q$os\E"/ && $cfg =~ /target_arch = "\Q$arch\E"/;
  if ($kind eq "BUN_DOWNLOAD") {
    my @strings = $body =~ /"([^"]+)"/g;
    die "BUN_DOWNLOAD 格式不对\n" unless @strings >= 2;
    ($url, $archive_sha) = @strings;
  } elsif ($kind eq "BUN_ZIP_DIR") {
    ($zip_dir) = $body =~ /"([^"]+)"/;
  } elsif ($kind eq "BUN_EXECUTABLE_SHA256") {
    ($exe_sha) = $body =~ /"([^"]+)"/;
  }
}
die "找不到 $os/$arch 的受管 Bun 锁定信息\n"
  unless $url && $archive_sha && $zip_dir && $exe_sha;
print "version=", sh_quote($version), "\n";
print "url=", sh_quote($url), "\n";
print "archive_sha=", sh_quote($archive_sha), "\n";
print "zip_dir=", sh_quote($zip_dir), "\n";
print "exe_sha=", sh_quote($exe_sha), "\n";
sub sh_quote {
  my ($value) = @_;
  $value =~ s/'/'\\''/g;
  return "'$value'";
}
PERL
)"

sha256_file() {
  perl -MDigest::SHA=sha256_hex -e 'local $/; print sha256_hex(<>)' "$1"
}

zip_path="$cache/bun-${RUNTIME_OS}-${RUNTIME_ARCH}.zip"
zip_sha_path="$cache/bun-${RUNTIME_OS}-${RUNTIME_ARCH}.sha256"
exe_path="$cache/bun"

if [[ ! -f "$zip_path" ]] || [[ "$(sha256_file "$zip_path")" != "$archive_sha" ]]; then
  echo "正在下载 Bun ${version}（${RUNTIME_OS}/${RUNTIME_ARCH}）…" >&2
  tmp_zip="$zip_path.partial"
  curl -fsSL --retry 2 -o "$tmp_zip" "$url"
  actual_archive="$(sha256_file "$tmp_zip")"
  if [[ "$actual_archive" != "$archive_sha" ]]; then
    rm -f "$tmp_zip"
    echo "Bun 压缩包校验失败（期望 ${archive_sha}，实际 ${actual_archive}）" >&2
    exit 1
  fi
  mv "$tmp_zip" "$zip_path"
fi
printf '%s\n' "$archive_sha" >"$zip_sha_path"

ready=false
if [[ -f "$exe_path" && -x "$exe_path" ]]; then
  actual_sha="$(sha256_file "$exe_path")"
  actual_version="$("$exe_path" --version 2>/dev/null || true)"
  if [[ "$actual_sha" == "$exe_sha" && "$actual_version" == "$version" ]]; then
    ready=true
  fi
fi
if $ready; then
  echo "$exe_path"
  exit 0
fi

unpack="$(mktemp -d)"
trap 'rm -rf "$unpack"' EXIT
unzip -q "$zip_path" -d "$unpack"
install -m 0755 "$unpack/$zip_dir/bun" "$exe_path"
actual_sha="$(sha256_file "$exe_path")"
actual_version="$("$exe_path" --version 2>/dev/null || true)"
if [[ "$actual_sha" != "$exe_sha" || "$actual_version" != "$version" ]]; then
  rm -f "$exe_path"
  echo "Bun 可执行文件校验失败" >&2
  exit 1
fi
echo "$exe_path"
