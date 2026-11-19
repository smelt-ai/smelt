#!/usr/bin/env bash
# JS 性能探针（crates/smelt-plugin-api/src/js/ui/bench.rs）每项单独起进程，量峰值内存，
# JS 的项减去只挂一个空 JS 视图的基线 p00_baseline，Rust 的项（名字带 rust）减去空 Rust 视图的
# p00_rust_baseline。只在 macOS 上用（读 /usr/bin/time -l 的输出）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -j 2 -p smelt-plugin-api --features js --release --lib js::ui::bench --no-run 2>&1 | tail -1
bin=$(ls -t target/release/deps/smelt_plugin_api-* | grep -v '\.d$' | head -1)
peak() {
  /usr/bin/time -l "$bin" "js::ui::bench::$1" --exact --ignored --test-threads 1 2>&1 >/dev/null |
    awk '/peak memory footprint/ { printf "%.1f", $1 / 1048576 }'
}
js_base=$(peak p00_baseline)
rust_base=$(peak p00_rust_baseline)
echo "基线：空 JS 视图 ${js_base} MB，空 Rust 视图 ${rust_base} MB（差值是 JS 运行时本身）"
for name in $("$bin" --list --ignored 2>/dev/null | sed -n 's/^js::ui::bench::\(p[0-9]*_[a-z0-9_]*\): test$/\1/p' | grep -v '^p00_'); do
  mb=$(peak "$name")
  case "$name" in *rust*) base=$rust_base ;; *) base=$js_base ;; esac
  printf '%-40s 峰值 %7.1f MB  比基线 +%7.1f MB\n' "$name" "$mb" "$(echo "$mb - $base" | bc)"
done
