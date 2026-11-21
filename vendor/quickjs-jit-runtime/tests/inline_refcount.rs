//! Inline DUP/FREE reference-count fast paths must keep interpreter results,
//! ownership and finalization exact in every tier and in stress-GC mode.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    any(target_os = "linux", target_os = "macos"),
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{CatchResultExt, Context, Runtime};
use rquickjs_core::qjs;
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};

unsafe extern "C" {
    fn JS_JitGetHelperCount(rt: *mut qjs::JSRuntime, helper: u32, count: *mut u64) -> i32;
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Interpreter,
    Tier1,
    Tier2,
    Automatic,
}

const DRIVER: &str = r#"
var results = [];
function batch(count) {
  results = new Array(count);
  function callWorkload() { return workload(200, 3); }
  function next(index) {
    for (let i = index; i < count; i++) {
      const result = callWorkload();
      results[i] = result;
    }
  }
  return next(0);
}
function checksum() {
  return results.map(function(value) {
    if (typeof value === 'number') return 'number:' + value;
    if (typeof value === 'string') return 'string:' + value;
    return typeof value + ':' + String(value);
  }).join('|');
}
"#;

/// Runs `batch(4)` repeatedly and checks every round against `expected`
/// (workloads keep no state across calls). JIT modes keep going until native
/// code has run for at least ten rounds, so background compilation latency on
/// a loaded host cannot turn the comparison into an interpreter-only one.
fn run(mode: Mode, stress: bool, source: &str, expected: Option<&str>) -> String {
    let runtime = Runtime::new().unwrap();
    let jit = match mode {
        Mode::Interpreter => None,
        _ => {
            let mut builder = JitConfig::builder().stress_gc(stress);
            builder = match mode {
                Mode::Tier1 => builder
                    .call_threshold(2)
                    .loop_threshold(1)
                    .tier_policy(JitTierPolicy::BaselineOnly),
                Mode::Tier2 => builder
                    .call_threshold(1)
                    .loop_threshold(1)
                    .tier_policy(JitTierPolicy::Optimize)
                    .force_optimized_for_test(true),
                _ => builder,
            };
            Some(Jit::attach(&runtime, builder.build().unwrap()).unwrap())
        }
    };
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(source).catch(&ctx).unwrap();
        ctx.eval::<(), _>(DRIVER).catch(&ctx).unwrap();
    });
    let round = || {
        context.with(|ctx| {
            ctx.eval::<(), _>("batch(4)")
                .catch(&ctx)
                .unwrap_or_else(|error| panic!("{mode:?} stress={stress}: {error}"));
            ctx.eval::<String, _>("checksum()").catch(&ctx).unwrap()
        })
    };
    let first = round();
    let Some(jit) = jit else {
        return first;
    };
    assert_eq!(Some(first.as_str()), expected, "{mode:?} stress={stress}");
    // Sanitizer builds compile and run stress-GC rounds far more slowly.
    let budget = if cfg!(rquickjs_sanitizer) { 300 } else { 60 };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget);
    let mut native_rounds = 0;
    let mut rounds = 1;
    // Production tiering may legitimately demote an unprofitable function to
    // the interpreter, so only forced tiers must keep running native code.
    let required_native_rounds = if matches!(mode, Mode::Automatic) {
        0
    } else {
        10
    };
    while rounds < 40 || native_rounds < required_native_rounds {
        let before = jit.metrics().native_entries;
        assert_eq!(Some(round().as_str()), expected, "{mode:?} stress={stress}");
        rounds += 1;
        if jit.metrics().native_entries > before {
            native_rounds += 1;
        }
        jit.poll();
        // The deadline only bounds waiting for native code. Once the native
        // requirement holds, a slow host (coverage, stress GC) may stop before
        // the minimum round count instead of failing.
        if std::time::Instant::now() >= deadline {
            assert!(
                native_rounds >= required_native_rounds,
                "{mode:?} stress={stress} never ran native code: {:?}",
                jit.metrics()
            );
            break;
        }
        if native_rounds < required_native_rounds {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    context.with(|ctx| ctx.run_gc());
    first
}

fn assert_all_tiers(source: &str) {
    let expected = run(Mode::Interpreter, false, source, None);
    for mode in [Mode::Tier1, Mode::Tier2, Mode::Automatic] {
        for stress in [false, true] {
            run(mode, stress, source, Some(&expected));
        }
    }
}

#[test]
fn scalar_loop_under_benchmark_driver() {
    assert_all_tiers(
        "function workload(n, seed) { let s = 0; for (let i = 0; i < n; i++) s += i; return s + seed; }",
    );
}

#[test]
fn shared_object_references_survive_repeated_dup_and_free() {
    assert_all_tiers(
        r#"
        const shared = { x: 1, nested: { y: 2 } };
        function workload(n, seed) {
          let sum = 0;
          for (let i = 0; i < n; i++) {
            let a = shared;
            let b = a;
            let c = b.nested;
            a = null;
            sum += b.x + c.y;
          }
          return sum + seed;
        }
        "#,
    );
}

#[test]
fn last_reference_is_finalized_exactly_once() {
    assert_all_tiers(
        r#"
        function workload(n, seed) {
          const registry = [];
          let live = 0;
          for (let i = 0; i < n; i++) {
            let object = { i, text: 'v' + i };
            let alias = object;
            object = undefined;
            live += alias.i;
            alias = undefined;
            if ((i & 31) === 0) registry.push({ i });
            if (registry.length > 8) registry.length = 0;
          }
          return live + registry.length + seed;
        }
        "#,
    );
}

#[test]
fn polymorphic_objects_and_strings() {
    assert_all_tiers(
        r#"
        function workload(iterations, seed) {
          const objects = [];
          for (let i = 0; i < iterations; i++) {
            let object;
            switch (i & 3) {
              case 0: object = { x: i, y: seed, kind: 0 }; break;
              case 1: object = { y: seed, x: i, extra: 'e' + i, kind: 1 }; break;
              case 2: object = { x: i, kind: 2 }; object.y = seed; break;
              default: object = Object.create(null); object.kind = 3; object.x = i; object.y = seed;
            }
            object.x += object.kind;
            objects.push(object);
          }
          let sum = 0;
          for (let i = 0; i < objects.length; i++) sum += objects[i].x + objects[i].y;
          return sum;
        }
        "#,
    );
}

#[test]
fn generic_calls_release_consumed_operands() {
    assert_all_tiers(
        r#"
        const box = { v: 7 };
        function id(value) { return value; }
        function pick(a, b) { return a.v > b.v ? a : b; }
        function workload(n, seed) {
          let best = box;
          let s = '';
          for (let i = 0; i < n; i++) {
            const candidate = { v: (i * 7) % 13 };
            best = pick(id(best), candidate);
            s = id(s.length > 8 ? '' : s + 'x');
          }
          return best.v + s.length + seed;
        }
        "#,
    );
}

/// Outside stress GC, a shared heap reference is duplicated and released
/// without DUP/FREE helpers, while the last reference still reaches the FREE
/// helper (the only path that may finalize). Stress GC keeps every helper.
#[test]
fn helpers_run_only_for_last_references_or_stress() {
    for stress in [false, true] {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .loop_threshold(1)
                .tier_policy(JitTierPolicy::BaselineOnly)
                .stress_gc(stress)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                const shared = { v: 1 };
                function sharedOnly(n) {
                  let sum = 0;
                  for (let i = 0; i < n; i++) { let a = shared; let b = a; sum += b.v; a = b = undefined; }
                  return sum;
                }
                function lastOnly(n) {
                  let sum = 0;
                  for (let i = 0; i < n; i++) { let a = { v: i }; sum += a.v; a = undefined; }
                  return sum;
                }
                "#,
            )
            .catch(&ctx)
            .unwrap();
        });
        let rt = context.with(|ctx| unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
        let count = |helper| {
            let mut value = 0;
            assert_eq!(unsafe { JS_JitGetHelperCount(rt, helper, &mut value) }, 0);
            value
        };
        let call =
            |source: &str| context.with(|ctx| ctx.eval::<i32, _>(source).catch(&ctx).unwrap());
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let before = jit.metrics().native_entries;
            assert_eq!(call("sharedOnly(64)"), 64);
            assert_eq!(call("lastOnly(64)"), 2016);
            jit.poll();
            let metrics = jit.metrics();
            if metrics.native_entries >= before + 2 && metrics.pending_worker_jobs == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never native: {metrics:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let dup = qjs::JSJitHelperId_JS_JIT_HELPER_DUP;
        let free = qjs::JSJitHelperId_JS_JIT_HELPER_FREE;
        let entries = jit.metrics().native_entries;
        let (dups, frees) = (count(dup), count(free));
        assert_eq!(call("sharedOnly(64)"), 64);
        assert_eq!(jit.metrics().native_entries, entries + 1);
        if stress {
            assert!(count(dup) > dups, "stress GC must keep DUP helpers");
            assert!(count(free) > frees, "stress GC must keep FREE helpers");
        } else {
            assert_eq!(count(dup), dups, "shared DUP reached the helper");
            assert_eq!(count(free), frees, "shared FREE reached the helper");
        }
        let frees = count(free);
        assert_eq!(call("lastOnly(64)"), 2016);
        assert_eq!(jit.metrics().native_entries, entries + 2);
        assert!(
            count(free) >= frees + 64,
            "every last reference must reach the FREE helper"
        );
        runtime.run_gc();
    }
}
