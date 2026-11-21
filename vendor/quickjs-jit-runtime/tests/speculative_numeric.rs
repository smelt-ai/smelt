#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_os = "linux",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Function, Runtime};
use rquickjs_jit::{Jit, JitConfig};

#[test]
fn real_quickjs_numeric_snapshots_lower_with_matching_feedback() {
    use rquickjs_jit::{
        bytecode::VerifyLimits,
        compiler::optimized::Tier2Compiler,
        runtime::{BinaryFeedbackFlags, FeedbackTable, FunctionKey, ObservedType},
        test_support::SnapshotFixture,
    };

    for operator in ["-", "*", "/"] {
        let fixture = SnapshotFixture::compile(&format!("(function(a,b){{return a{operator}b}})"));
        let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
        let arithmetic_pc = verified
            .instructions()
            .iter()
            .find(|instruction| matches!(instruction.opcode().name(), "sub" | "mul" | "div"))
            .unwrap()
            .pc();
        let key = FunctionKey::new(1, 1);
        let mut feedback = FeedbackTable::new(4, 4);
        for _ in 0..8 {
            feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Int32]);
            feedback.observe_binary(
                key,
                arithmetic_pc,
                ObservedType::Int32,
                ObservedType::Int32,
                ObservedType::Int32,
                BinaryFeedbackFlags::NONE,
            );
            feedback.observe_return(key, arithmetic_pc + 1, ObservedType::Int32);
        }
        let snapshot = feedback.snapshot(1);
        Tier2Compiler::host(snapshot.epoch())
            .lower_with_feedback_for_test(&verified, key, &snapshot)
            .unwrap_or_else(|error| panic!("real {operator} snapshot did not lower: {error:?}"));
    }
}

fn with_optimized_binary<T>(
    body: &str,
    warm_lhs: T,
    warm_rhs: T,
    check: impl FnOnce(&Context, &Jit),
) where
    T: Copy + for<'js> rquickjs::IntoJs<'js>,
{
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| ctx.eval::<(), _>(format!("function f(a,b){{return {body}}}")))
        .unwrap();

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while std::time::Instant::now() < deadline {
        context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            let _: f64 = function.call((warm_lhs, warm_rhs)).unwrap();
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    check(&context, &jit);
}

#[test]
fn checked_int32_subtraction_deopts_to_the_exact_overflow_result() {
    with_optimized_binary("a-b", 20_i32, 3_i32, |context, jit| {
        let before = jit.metrics();
        let result = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((i32::MIN, 1_i32)).unwrap()
        });
        assert_eq!(result, -2_147_483_649.0);
        assert!(jit.metrics().deopts > before.deopts);
    });
}

#[test]
fn checked_int32_multiply_deopts_for_javascript_negative_zero() {
    with_optimized_binary("a*b", 6_i32, 7_i32, |context, jit| {
        let before = jit.metrics();
        let result = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((0_i32, -1_i32)).unwrap()
        });
        assert_eq!(result.to_bits(), (-0.0_f64).to_bits());
        assert!(jit.metrics().deopts > before.deopts);
    });
}

#[test]
fn checked_int32_division_deopts_for_fraction_and_zero_divisor() {
    with_optimized_binary("a/b", 84_i32, 2_i32, |context, jit| {
        let before = jit.metrics();
        let fraction = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((7_i32, 2_i32)).unwrap()
        });
        let infinity = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((1_i32, 0_i32)).unwrap()
        });
        assert_eq!(fraction, 3.5);
        assert_eq!(infinity, f64::INFINITY);
        assert!(jit.metrics().deopts >= before.deopts + 2);
    });
}

#[test]
fn stable_float64_add_executes_natively_and_preserves_nan_and_infinity() {
    with_optimized_binary("a+b", 1.25_f64, 2.5_f64, |context, jit| {
        let before = jit.metrics();
        let infinity = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((f64::INFINITY, 1.5_f64)).unwrap()
        });
        let nan = context.with(|ctx| {
            let function: Function<'_> = ctx.globals().get("f").unwrap();
            function.call::<_, f64>((f64::NAN, 1.5_f64)).unwrap()
        });
        assert_eq!(infinity, f64::INFINITY);
        assert!(nan.is_nan());
        let after = jit.metrics();
        assert!(after.tier2_entries >= before.tier2_entries + 2, "{after:?}");
        assert_eq!(after.deopts, before.deopts, "{after:?}");
    });
}

// ---------------------------------------------------------------------------
// M2 core opcodes: exact runtime fallback through the production pipeline.
// Tier 2 is only reached after Tier 1 accepts the function, so these run
// once the M2 Tier 1 policy flip admits the opcodes below.
// ---------------------------------------------------------------------------

/// Installs `source` (which must define `f`) and evaluates `warm` until the
/// optimized entry has run at least once.
fn with_optimized_source(source: &str, warm: &str, check: impl FnOnce(&Context, &Jit)) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(2)
            .loop_threshold(64)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while std::time::Instant::now() < deadline {
        context.with(|ctx| {
            let _: rquickjs::Value<'_> = ctx.eval(warm).unwrap();
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    check(&context, &jit);
}

fn eval<T: for<'js> rquickjs::FromJs<'js>>(context: &Context, source: &str) -> T {
    context.with(|ctx| {
        ctx.eval(source)
            .unwrap_or_else(|error| panic!("{source}: {error} {:?}", ctx.catch()))
    })
}

#[test]
fn checked_int32_modulo_deopts_to_exact_negative_zero_nan_and_min_remainder() {
    with_optimized_source("function f(a,b){return a%b}", "f(7,3)", |context, jit| {
        let before = jit.metrics();
        assert!(eval::<bool>(context, "Object.is(f(-4,2), -0)"));
        assert!(eval::<bool>(context, "Number.isNaN(f(5,0))"));
        assert!(eval::<bool>(context, "Object.is(f(-2147483648,-1), -0)"));
        assert!(
            jit.metrics().deopts >= before.deopts + 3,
            "{:?}",
            jit.metrics()
        );
        assert_eq!(eval::<i32>(context, "f(7,-3)"), 1);
    });
}

#[test]
fn shift_counts_are_masked_like_quickjs() {
    with_optimized_source("function f(a,b){return a<<b}", "f(1,2)", |context, jit| {
        let before = jit.metrics();
        assert!(eval::<bool>(context, "f(1,33) === 2"));
        assert!(eval::<bool>(context, "f(1,32) === 1"));
        assert_eq!(jit.metrics().deopts, before.deopts, "{:?}", jit.metrics());
    });
}

#[test]
fn unsigned_shift_renormalizes_large_results_to_float64() {
    with_optimized_source("function f(a,b){return a>>>b}", "f(8,1)", |context, jit| {
        let before = jit.metrics();
        assert!(eval::<bool>(context, "f(-1,0) === 4294967295"));
        assert!(eval::<bool>(context, "f(8,33) === 4"));
        assert_eq!(jit.metrics().deopts, before.deopts, "{:?}", jit.metrics());
    });
}

#[test]
fn bitwise_not_uses_to_int32_for_float64_operands() {
    with_optimized_source("function f(a){return ~a}", "f(1)", |context, _jit| {
        assert!(eval::<bool>(context, "f(1.5) === -2"));
        assert!(eval::<bool>(context, "f(4294967294) === 1"));
        assert!(eval::<bool>(context, "f(1) === -2"));
    });
}

#[test]
fn strict_equality_of_strings_deopts_after_numeric_feedback() {
    with_optimized_source("function f(a,b){return a===b}", "f(1,1)", |context, jit| {
        let before = jit.metrics();
        assert!(eval::<bool>(context, "f('a','a') === true"));
        assert!(eval::<bool>(context, "f('a','b') === false"));
        assert!(jit.metrics().deopts > before.deopts, "{:?}", jit.metrics());
        assert!(eval::<bool>(context, "f(2,2) === true"));
    });
}

#[test]
fn loose_equality_of_null_and_undefined_is_exact() {
    with_optimized_source("function f(a,b){return a==b}", "f(1,1)", |context, _jit| {
        assert!(eval::<bool>(context, "f(null,undefined) === true"));
        assert!(eval::<bool>(context, "f(undefined,null) === true"));
        assert!(eval::<bool>(context, "f(null,0) === false"));
        assert!(eval::<bool>(context, "f(true,1) === true"));
    });
}

#[test]
fn nan_is_strictly_unequal_to_itself_without_deopt() {
    with_optimized_source(
        "function f(a,b){return a!==b}",
        "f(1.5,2.5)",
        |context, jit| {
            let before = jit.metrics();
            assert!(eval::<bool>(context, "f(NaN,NaN) === true"));
            assert!(eval::<bool>(context, "f(-0,0) === false"));
            assert_eq!(jit.metrics().deopts, before.deopts, "{:?}", jit.metrics());
        },
    );
}

#[test]
fn negating_zero_and_int32_min_produces_exact_float64_results() {
    with_optimized_source("function f(a){return -a}", "f(5)", |context, jit| {
        let before = jit.metrics();
        assert!(eval::<bool>(context, "Object.is(f(0), -0)"));
        assert!(eval::<bool>(context, "f(-2147483648) === 2147483648"));
        assert!(eval::<bool>(context, "f(7) === -7"));
        assert_eq!(jit.metrics().deopts, before.deopts, "{:?}", jit.metrics());
    });
}

/// Like `with_optimized_source`, but warms through one predefined caller so
/// a loop-shaped `f` is not starved by per-iteration script compilations.
fn with_optimized_loop(source: &str, warm: &str, check: impl FnOnce(&Context, &Jit)) {
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .force_optimized_for_test(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context
        .with(|ctx| {
            ctx.eval::<(), _>(format!(
                "{source}; globalThis.__warm=function(){{return {warm}}}"
            ))
        })
        .unwrap();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(if cfg!(rquickjs_sanitizer) { 25 } else { 5 });
    while std::time::Instant::now() < deadline {
        context.with(|ctx| {
            let warm: Function<'_> = ctx.globals().get("__warm").unwrap();
            let _: rquickjs::Value<'_> = warm.call(()).unwrap();
        });
        jit.poll();
        if jit.metrics().tier2_entries > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    assert!(jit.metrics().tier2_entries > 0, "{:?}", jit.metrics());
    check(&context, &jit);
}

#[test]
fn counted_loop_increments_stay_exact_at_the_int32_boundary() {
    // The bounded increment proof applies to `<`; the `<=` loop reaches
    // INT32_MAX and its increment must produce the exact Float64 result via
    // the retained overflow exit.
    with_optimized_loop(
        "function f(a,b){let c=0;for(let i=a;i<b;i++){c=c+1}return c}",
        "f(0,64)",
        |context, jit| {
            let before = jit.metrics();
            assert_eq!(
                eval::<i32>(context, "f(2147483640, 2147483647)"),
                7,
                "{:?}",
                jit.metrics()
            );
            assert_eq!(jit.metrics().deopts, before.deopts, "{:?}", jit.metrics());
        },
    );
    with_optimized_loop(
        "function f(a,b){let c=0;let last=0;for(let i=a;i<=b;i++){c=c+1;last=i}return c*1e10+last}",
        "f(0,64)",
        |context, jit| {
            let before = jit.metrics();
            let result = eval::<f64>(context, "f(2147483640, 2147483647)");
            assert_eq!(result, 8.0 * 1e10 + 2147483647.0, "{:?}", jit.metrics());
            let after = jit.metrics();
            assert_eq!(after.native_fallbacks, before.native_fallbacks, "{after:?}");
            assert_eq!(after.native_retries, before.native_retries, "{after:?}");
        },
    );
}
