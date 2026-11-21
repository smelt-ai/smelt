//! Scalar-loop code quality: per-loop poll amortization, exit
//! rematerialization, constant-branch folding and constant unsigned shifts.
//! Generated-code assertions establish the shape; runtime tests establish
//! that the cheaper code keeps exact interpreter semantics, deopts and
//! interrupt service.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::{
    bytecode::VerifyLimits, compiler::optimized::Tier2Compiler, test_support::SnapshotFixture, Jit,
    JitConfig,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

/// Calls before and after the loops rule out whole-function poll
/// amortization; the loop nest itself is pure Int32 arithmetic.
const INT_ARITH: &str = "function arith(iterations){\
     let total=0;\
     const outer=Math.max(1,iterations>>7);\
     for(let j=0;j<outer;j++){let sum=0;for(let i=0;i<1000;i++)sum+=i*i;total+=sum;}\
     return String(total)}";

fn expected_arith(iterations: i64) -> String {
    let outer = (iterations >> 7).max(1);
    let sum: i64 = (0..1000).map(|i| i * i).sum();
    (outer * sum).to_string()
}

fn tier2_clif(source: &str) -> String {
    let fixture = SnapshotFixture::compile(source);
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    Tier2Compiler::host(91)
        .lower_for_test(&verified, 91)
        .unwrap_or_else(|error| panic!("{source} did not lower: {error:?}"))
}

/// Counts poll countdowns: a `-1` decrement whose result is compared with
/// zero. Inline refcount releases also decrement by one, but store the
/// result back to the object header instead of testing it, so they are not
/// counted.
fn countdowns(clif: &str) -> usize {
    let lines = clif
        .lines()
        .map(|line| line.split("  ;").next().unwrap_or(line).trim())
        .collect::<Vec<_>>();
    lines
        .iter()
        .filter_map(|line| {
            if !(line.contains("iadd_imm") && line.ends_with(", -1")) {
                return None;
            }
            line.split(" = ").next()
        })
        .filter(|decremented| {
            let tested = format!("icmp_imm eq {decremented}, 0");
            lines.iter().any(|line| line.ends_with(&tested))
        })
        .count()
}

#[test]
fn a_pure_loop_counts_down_its_poll_even_when_the_function_calls() {
    let clif = tier2_clif(&format!(
        "({})",
        INT_ARITH.replace("function arith", "function")
    ));
    assert!(
        clif.contains("call_indirect"),
        "fixture must keep generic calls outside the loops: {clif}"
    );
    // Both the outer and the inner loop are call-free, so each header polls
    // through its own countdown instead of calling the runtime every trip.
    assert!(countdowns(&clif) >= 2, "{clif}");
}

#[test]
fn a_loop_containing_a_call_keeps_its_per_iteration_poll() {
    let clif = tier2_clif("(function(n){let s=0;for(let i=0;i<n;i++)s=s+Math.abs(i);return s})");
    assert_eq!(countdowns(&clif), 0, "{clif}");
}

struct Warm {
    runtime: Runtime,
    jit: Jit,
    context: Context,
}

fn warm_tier2(source: &str, name: &str, argument: i32, expected: &str) -> Warm {
    warm_tier2_with(source, name, argument, expected, true)
}

fn warm_tier2_with(
    source: &str,
    name: &str,
    argument: i32,
    expected: &str,
    stress_gc: bool,
) -> Warm {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(16)
            .force_optimized_for_test(true)
            .stress_gc(stress_gc)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 100 } else { 20 });
    loop {
        let before = jit.metrics();
        let result = context.with(|ctx| {
            let function: Function = ctx.globals().get(name).unwrap();
            function
                .call::<_, rquickjs::Value>((argument,))
                .map(|value| {
                    value
                        .as_string()
                        .map(|string| string.to_string().unwrap())
                        .or_else(|| value.as_number().map(|number| number.to_string()))
                        .unwrap()
                })
        });
        assert_eq!(result.unwrap(), expected);
        jit.poll();
        if jit.metrics().tier2_entries > before.tier2_entries {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{name} never entered Tier 2: {:?}",
            jit.metrics()
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    Warm {
        runtime,
        jit,
        context,
    }
}

#[test]
fn per_loop_amortized_poll_keeps_results_and_interrupt_service() {
    let warm = warm_tier2(INT_ARITH, "arith", 1 << 12, &expected_arith(1 << 12));
    // Steady state: exact results across trip counts, all in Tier 2.
    for iterations in [0, 127, 128, 5000, 1 << 14] {
        let before = warm.jit.metrics();
        let actual = warm.context.with(|ctx| {
            let function: Function = ctx.globals().get("arith").unwrap();
            function.call::<_, String>((iterations,)).unwrap()
        });
        assert_eq!(actual, expected_arith(i64::from(iterations)));
        assert!(warm.jit.metrics().tier2_entries > before.tier2_entries);
    }
    // A (practically) unbounded loop nest must still reach the runtime poll
    // and unwind through the uncatchable interrupt.
    let interrupts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&interrupts);
    warm.runtime.set_interrupt_handler(Some(Box::new(move || {
        seen.fetch_add(1, Ordering::SeqCst) >= 2
    })));
    let before = warm.jit.metrics();
    warm.context.with(|ctx| {
        let function: Function = ctx.globals().get("arith").unwrap();
        assert!(function.call::<_, String>((i32::MAX,)).is_err());
        drop(ctx.catch());
    });
    warm.runtime.set_interrupt_handler(None);
    let after = warm.jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(interrupts.load(Ordering::SeqCst) >= 3);
    assert_eq!(after.native_entries, after.native_exits);
    let again = warm.context.with(|ctx| {
        let function: Function = ctx.globals().get("arith").unwrap();
        function.call::<_, String>((300,)).unwrap()
    });
    assert_eq!(again, expected_arith(300));
}

#[test]
fn scalar_loop_overflow_deopt_restores_the_exact_frame() {
    // The overflow exit reloads the frame buffers and exit record instead of
    // keeping the prologue values live; the interpreter must resume with the
    // exact pre-overflow sum and induction variable.
    let warm = warm_tier2(
        "function sum(n){let s=0;for(let i=0;i<n;i++)s=s+i;return s}",
        "sum",
        1000,
        "499500",
    );
    let before = warm.jit.metrics();
    let overflowing = warm.context.with(|ctx| {
        let function: Function = ctx.globals().get("sum").unwrap();
        function.call::<_, f64>((100_000,)).unwrap()
    });
    assert_eq!(overflowing, 4_999_950_000.0);
    let after = warm.jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(after.deopts > before.deopts, "{after:?}");
    assert_eq!(after.native_entries, after.native_exits);
}

#[test]
fn constant_unsigned_shift_stays_int32_in_a_mixed_loop() {
    // `v >>> 3` of a negative Int32 is a large positive Int32; the xor must
    // consume it exactly without a Float64 detour.
    let source = "function bits(n){let v=-123456789;\
                  for(let i=0;i<n;i++)v=((v<<5)^(v>>>3)^i)|0;\
                  return v}";
    let expected = |n: i32| {
        let mut v: i32 = -123_456_789;
        for i in 0..n {
            v = v.wrapping_shl(5) ^ ((v as u32 >> 3) as i32) ^ i;
        }
        v
    };
    let warm = warm_tier2(source, "bits", 300, &expected(300).to_string());
    for n in [0, 1, 7, 1000, 4097] {
        let before = warm.jit.metrics();
        let actual = warm.context.with(|ctx| {
            let function: Function = ctx.globals().get("bits").unwrap();
            function.call::<_, i32>((n,)).unwrap()
        });
        assert_eq!(actual, expected(n));
        assert!(warm.jit.metrics().tier2_entries > before.tier2_entries);
    }
    assert_eq!(
        warm.jit.metrics().native_entries,
        warm.jit.metrics().native_exits
    );
}

const PROPS: &str = "var receiver={x:3,y:4};\
                     function props(n){const o=receiver;let t=0;\
                     const k=Math.max(0,n);\
                     for(let i=0;i<k;i++)t=t+o.x*i;\
                     return String(t)}";

fn expected_props(x: f64, n: i64) -> String {
    (x * (n * (n - 1) / 2) as f64).to_string()
}

fn call_props(warm: &Warm, n: i32) -> String {
    warm.context.with(|ctx| {
        let function: Function = ctx.globals().get("props").unwrap();
        function.call::<_, String>((n,)).unwrap()
    })
}

#[test]
fn per_loop_poll_with_guarded_property_reads_on_a_refcounted_local() {
    // Integration of per-loop poll amortization with inline DUP/FREE and
    // shape-guarded property reads: the loop reads a property of an object
    // held in a local (each read duplicates and releases the receiver), and
    // calls outside the loop rule out whole-function amortization. Stress GC
    // forces the out-of-line refcount paths (a collection per DUP). A shape
    // change must deopt with the exact frame and the recompiled code must
    // keep exact results.
    let warm = warm_tier2_with(PROPS, "props", 300, &expected_props(3.0, 300), true);
    for n in [0, 1, 63, 64, 65, 5000] {
        let before = warm.jit.metrics();
        assert_eq!(call_props(&warm, n), expected_props(3.0, i64::from(n)));
        assert!(warm.jit.metrics().tier2_entries > before.tier2_entries);
    }
    // A different shape (and a Float64 field) reaches the guarded read.
    warm.context
        .with(|ctx| ctx.eval::<(), _>("receiver={y:1,x:2.5}"))
        .unwrap();
    for n in [0, 5, 1000] {
        assert_eq!(call_props(&warm, n), expected_props(2.5, i64::from(n)));
    }
    warm.context
        .with(|ctx| ctx.eval::<(), _>("receiver={x:3,y:4}"))
        .unwrap();
    for n in [0, 64, 1000] {
        assert_eq!(call_props(&warm, n), expected_props(3.0, i64::from(n)));
    }
    let metrics = warm.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn per_loop_poll_with_guarded_property_reads_services_interrupts() {
    // Without GC stress (a collection per iteration would make the unbounded
    // loop below take minutes before the poll countdown reaches the
    // interrupt handler), an unbounded trip count must still be interrupted
    // through the counted-down poll, including after a shape-change deopt
    // and recompilation.
    let warm = warm_tier2_with(PROPS, "props", 300, &expected_props(3.0, 300), false);
    let interrupted = |warm: &Warm| {
        let interrupts = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&interrupts);
        warm.runtime.set_interrupt_handler(Some(Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst) >= 2
        })));
        warm.context.with(|ctx| {
            let function: Function = ctx.globals().get("props").unwrap();
            assert!(function.call::<_, String>((i32::MAX,)).is_err());
            drop(ctx.catch());
        });
        warm.runtime.set_interrupt_handler(None);
        assert!(interrupts.load(Ordering::SeqCst) >= 3);
    };
    interrupted(&warm);
    assert_eq!(call_props(&warm, 300), expected_props(3.0, 300));
    warm.context
        .with(|ctx| ctx.eval::<(), _>("receiver={y:1,x:2.5}"))
        .unwrap();
    for n in [0, 5, 1000] {
        assert_eq!(call_props(&warm, n), expected_props(2.5, i64::from(n)));
    }
    warm.context
        .with(|ctx| ctx.eval::<(), _>("receiver={x:3,y:4}"))
        .unwrap();
    for n in [0, 64, 1000] {
        assert_eq!(call_props(&warm, n), expected_props(3.0, i64::from(n)));
    }
    interrupted(&warm);
    let metrics = warm.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
    assert_eq!(call_props(&warm, 300), expected_props(3.0, 300));
}
