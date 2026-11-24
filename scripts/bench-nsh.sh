#!/usr/bin/env bash
# NSH / GPUI 性能基准运行器与基线对比工具。
# 用法：
#   ./scripts/bench-nsh.sh [--suite quick|core|all] [--scenarios s1,s2,...]
#                          [--warmup 3] [--measure 6] [--save path.json]
#                          [--compare baseline.json] [--no-build]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

suite="core"
scenarios_csv=""
warmup="3"
measure="6"
save_path=""
compare_path=""
no_build="0"

usage() {
  cat <<'EOF'
用法：./scripts/bench-nsh.sh [选项]

选项：
  --suite <quick|core|all>   预设场景集（默认 core）
  --scenarios <s1,s2,...>    显式指定逗号分隔的场景列表（覆盖 --suite）
  --warmup <秒>              每个场景预热时长（默认 3）
  --measure <秒>             每个场景采样时长（默认 6）
  --save <path.json>         将结构化结果连同 Git/机器元数据保存到 JSON
  --compare <baseline.json>  与基线 JSON 对比，输出差值并标记退化项
  --no-build                 跳过 release bench 编译
  -h, --help                 显示帮助
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --suite)
      suite="${2:?--suite 需要参数}"
      shift 2
      ;;
    --scenarios)
      scenarios_csv="${2:?--scenarios 需要参数}"
      shift 2
      ;;
    --warmup)
      warmup="${2:?--warmup 需要秒数}"
      shift 2
      ;;
    --measure)
      measure="${2:?--measure 需要秒数}"
      shift 2
      ;;
    --save)
      save_path="${2:?--save 需要路径}"
      shift 2
      ;;
    --compare)
      compare_path="${2:?--compare 需要基线 JSON 路径}"
      shift 2
      ;;
    --no-build)
      no_build="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "✗ 未知参数：$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ -z "$scenarios_csv" ]; then
  case "$suite" in
    quick)
      scenarios_csv="real-boot-staged,real-conv-idle,real-conv-stream-60,real-term-flood,real-session-switch"
      ;;
    core)
      scenarios_csv="real-boot-staged,real-state-storm,real-conv-idle,real-conv-sweep,real-conv-stream-60,real-conv-grow,real-conv-scroll,real-term-idle,real-term-flood,real-term-and-conv,real-git-diff,real-files-editor,real-session-switch,real-resize"
      ;;
    all)
      scenarios_csv="idle,split,whole,wholev,gpui,chat-stream,real-boot,real-boot-staged,real-state-storm,real-conv-idle,real-conv-sweep,real-conv-stream-60,real-conv-grow,real-conv-scroll,real-term-idle,real-term-flood,real-term-4,real-term-and-conv,real-git-diff,real-files-editor,real-session-switch,real-resize"
      ;;
    *)
      echo "✗ 不认识的 suite：${suite}（可选 quick | core | all）" >&2
      exit 2
      ;;
  esac
fi

if [ -n "$compare_path" ] && [ ! -f "$compare_path" ]; then
  echo "✗ 基线文件不存在：${compare_path}" >&2
  exit 2
fi

if [ "$no_build" != "1" ]; then
  echo "· 编译 release bench（cargo build -j 2 --release -p smelt-nsh --example bench）" >&2
  cargo build -j 2 --release -p smelt-nsh --example bench
fi

target_dir="${CARGO_TARGET_DIR:-target}"
bench_bin="${target_dir}/release/examples/bench"
if [ ! -x "$bench_bin" ]; then
  echo "✗ 找不到可执行文件：${bench_bin}" >&2
  exit 1
fi

results_jsonl="$(mktemp)"
cleanup() {
  rm -f "$results_jsonl"
}
trap cleanup EXIT

IFS=',' read -r -a scenarios <<< "$scenarios_csv"
total="${#scenarios[@]}"
idx=0

for scenario in "${scenarios[@]}"; do
  idx=$((idx + 1))
  echo "· [${idx}/${total}] 跑场景：${scenario}（warmup=${warmup}s, measure=${measure}s）" >&2
  line="$("$bench_bin" "$scenario" --warmup "$warmup" --measure "$measure" --json)"
  printf '%s\n' "$line" >> "$results_jsonl"
done

git_rev="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
git_dirty="false"
if [ -n "$(git status --porcelain 2>/dev/null || true)" ]; then
  git_dirty="true"
fi

python3 - "$results_jsonl" "$warmup" "$measure" "$git_rev" "$git_dirty" "$save_path" "$compare_path" <<'PY'
import datetime
import json
import os
import platform
import sys

jsonl_path, warmup_s, measure_s, git_rev, git_dirty_str, save_path, compare_path = sys.argv[1:8]

results = []
with open(jsonl_path, "r", encoding="utf-8") as f:
    for raw in f:
        raw = raw.strip()
        if raw:
            results.append(json.loads(raw))

if not results:
    print("✗ 没有采集到任何基准结果", file=sys.stderr)
    sys.exit(1)

print()
headers = [
    "Scenario",
    "Frames/FPS",
    "Draw avg",
    "Draw p95",
    "Draw p99",
    "Draw max",
    ">16.7ms",
    "Main ms/s",
    "Proc ms/s",
    "Mem MB (dMB)",
    "Heap MB (dMB)",
    "Err",
]
rows = []
any_errors = False
for r in results:
    fr = r["frames"]
    cpu = r["cpu"]
    mem = r["memory"]
    if r["errors"] > 0:
        any_errors = True
    heap_str = "-"
    if mem.get("mem_heap_mb") is not None and mem.get("mem_heap_delta_mb") is not None:
        heap_str = f'{mem["mem_heap_mb"]:.0f} ({mem["mem_heap_delta_mb"]:+.1f})'
    rows.append([
        r["scenario"],
        f'{fr["drawn_frames"]}/{fr["fps"]:.1f}',
        f'{fr["draw_avg_ms"]:.2f}',
        f'{fr["draw_p95_ms"]:.2f}',
        f'{fr["draw_p99_ms"]:.2f}',
        f'{fr["draw_max_ms"]:.2f}',
        f'{fr["slow_16ms_pct"]:.1f}%',
        f'{cpu["main_cpu_ms_per_s"]:.1f}',
        f'{cpu["process_cpu_ms_per_s"]:.1f}',
        f'{mem["mem_total_mb"]:.0f} ({mem["mem_delta_mb"]:+.1f})',
        heap_str,
        str(r["errors"]),
    ])

widths = [len(h) for h in headers]
for row in rows:
    for i, cell in enumerate(row):
        widths[i] = max(widths[i], len(cell))

def fmt_row(row):
    parts = []
    for i, cell in enumerate(row):
        if i == 0:
            parts.append(cell.ljust(widths[i]))
        else:
            parts.append(cell.rjust(widths[i]))
    return " | ".join(parts)

print(fmt_row(headers))
print("-+-".join("-" * w for w in widths))
for row in rows:
    print(fmt_row(row))

boot_rows = [r for r in results if r.get("boot")]
if boot_rows:
    print("\n冷启动统计：")
    for r in boot_rows:
        b = r["boot"]
        parts = [
            f'ready={b["ready_ms"]:.1f}ms',
            f'plugins={b["plugins"]}',
            f'vms={b["plugin_vms"]}',
            f'errors={b["plugin_errors"]}',
        ]
        if b.get("stage_ms") is not None:
            parts.append(f'stage={b["stage_ms"]:.1f}ms')
        if b.get("compile_total_ms") is not None:
            parts.append(f'compile={b["compile_total_ms"]:.1f}ms')
        print(f'  {r["scenario"]}: ' + ", ".join(parts))

if save_path:
    parent = os.path.dirname(save_path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    payload = {
        "git_rev": git_rev,
        "git_dirty": git_dirty_str == "true",
        "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "os": platform.system(),
        "arch": platform.machine(),
        "warmup_s": float(warmup_s),
        "measure_s": float(measure_s),
        "results": results,
    }
    with open(save_path, "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=2)
    print(f"\n✓ 已保存基线：{save_path}")

failed = False
if any_errors:
    print("\n✗ 存在运行时插件错误 (errors > 0)", file=sys.stderr)
    failed = True

if compare_path:
    with open(compare_path, "r", encoding="utf-8") as f:
        baseline_doc = json.load(f)
    base_list = baseline_doc["results"] if isinstance(baseline_doc, dict) and "results" in baseline_doc else baseline_doc
    base_map = {item["scenario"]: item for item in base_list}

    print(f"\n基线对比（vs {compare_path}）：")
    cmp_headers = ["Scenario", "Draw p95 (d)", "Main ms/s (d)", "Proc ms/s (d)", "Mem MB (d)", "Heap MB (d)", "Status"]
    cmp_rows = []
    for cur in results:
        name = cur["scenario"]
        base = base_map.get(name)
        if not base:
            cmp_rows.append([name, "-", "-", "-", "-", "-", "NEW"])
            continue
        cur_p95 = cur["frames"]["draw_p95_ms"]
        base_p95 = base["frames"]["draw_p95_ms"]
        dp95 = cur_p95 - base_p95

        cur_main = cur["cpu"]["main_cpu_ms_per_s"]
        base_main = base["cpu"]["main_cpu_ms_per_s"]
        dmain = cur_main - base_main

        cur_proc = cur["cpu"]["process_cpu_ms_per_s"]
        base_proc = base["cpu"]["process_cpu_ms_per_s"]
        dproc = cur_proc - base_proc

        cur_mem = cur["memory"]["mem_total_mb"]
        base_mem = base["memory"]["mem_total_mb"]
        dmem = cur_mem - base_mem

        cur_heap = cur["memory"].get("mem_heap_mb")
        base_heap = base["memory"].get("mem_heap_mb")
        dheap = (cur_heap - base_heap) if (cur_heap is not None and base_heap is not None) else None

        reasons = []
        if cur["errors"] > 0:
            reasons.append("errors>0")
        if base_p95 > 0 and dp95 > 1.0 and (dp95 / base_p95) > 0.20:
            reasons.append("p95>20%+1ms")
        elif base_p95 == 0 and dp95 > 1.0:
            reasons.append("p95>1ms")
        if base_main > 0 and dmain > 5.0 and (dmain / base_main) > 0.25:
            reasons.append("main>25%+5ms/s")
        elif base_main == 0 and dmain > 5.0:
            reasons.append("main>5ms/s")
        if dheap is not None and dheap > 15.0:
            reasons.append("heap>+15MB")

        status = "PASS" if not reasons else "FAIL(" + ",".join(reasons) + ")"
        if reasons:
            failed = True
        cmp_rows.append([
            name,
            f"{cur_p95:.2f} ({dp95:+.2f})",
            f"{cur_main:.1f} ({dmain:+.1f})",
            f"{cur_proc:.1f} ({dproc:+.1f})",
            f"{cur_mem:.0f} ({dmem:+.1f})",
            f"{cur_heap:.0f} ({dheap:+.1f})" if dheap is not None else "-",
            status,
        ])

    cwidths = [len(h) for h in cmp_headers]
    for row in cmp_rows:
        for i, cell in enumerate(row):
            cwidths[i] = max(cwidths[i], len(cell))

    def fmt_cmp(row):
        return " | ".join(
            cell.ljust(cwidths[i]) if i in (0, len(row) - 1) else cell.rjust(cwidths[i])
            for i, cell in enumerate(row)
        )

    print(fmt_cmp(cmp_headers))
    print("-+-".join("-" * w for w in cwidths))
    for row in cmp_rows:
        print(fmt_cmp(row))

if failed:
    sys.exit(1)
PY
