#!/usr/bin/env bash
# 用钉死的 Bun 在干净目录做生产安装，打成随包归档。
# 产物：pi-runtime/dist/pi-runtime-<os>-<arch>.tar.gz 与 archive.sha256。
# release 构建把 SHA 编进二进制；这里不把归档本身编进去。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=runtime-target.sh
source "$root/scripts/runtime-target.sh"
runtime_target

cd "$root"
package="$root/pi-runtime"
dist="$package/dist"
mkdir -p "$dist"

archive="$dist/pi-runtime-${RUNTIME_OS}-${RUNTIME_ARCH}.tar.gz"
sha_file="$dist/archive.sha256"
stamp_file="$dist/pack.stamp"

sha256_file() {
  perl -MDigest::SHA=sha256_hex -e 'local $/; print sha256_hex(<>)' "$1"
}

input_stamp() {
  perl -MDigest::SHA -e '
    use File::Find;
    my @files = ("pi-runtime/package.json", "pi-runtime/bun.lock");
    find(sub { push @files, $File::Find::name if -f $_ }, "pi-runtime/src");
    @files = sort @files;
    my $sha = Digest::SHA->new(256);
    for my $file (@files) {
      open my $fh, "<:raw", $file or die "无法读取 $file: $!";
      local $/;
      $sha->add($file);
      $sha->add("\0");
      $sha->add(scalar <$fh>);
      $sha->add("\0");
    }
    print $sha->hexdigest;
  '
}

stamp="$(input_stamp)"
if [[ -f "$archive" && -f "$sha_file" && -f "$stamp_file" ]] \
  && [[ "$(tr -d '[:space:]' <"$stamp_file")" == "$stamp" ]] \
  && [[ "$(tr -d '[:space:]' <"$sha_file")" == "$(sha256_file "$archive")" ]]; then
  echo "Pi 运行时归档已是最新：$archive" >&2
  exit 0
fi

bun="$("$root/scripts/bun.sh")"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp "$package/package.json" "$package/bun.lock" "$stage/"
cp -R "$package/src" "$stage/src"
echo "正在安装 Pi 生产依赖…" >&2
(
  cd "$stage"
  "$bun" install --frozen-lockfile --production --ignore-scripts
)
# 归档只留运行需要的树。lockfile 只参与安装，不进产物。
tmp_archive="$archive.partial"
rm -f "$tmp_archive"
tar -czf "$tmp_archive" -C "$stage" package.json src node_modules
actual="$(sha256_file "$tmp_archive")"
mv "$tmp_archive" "$archive"
printf '%s\n' "$actual" >"$sha_file"
printf '%s\n' "$stamp" >"$stamp_file"
echo "已打包 $archive" >&2
echo "$actual" >&2
