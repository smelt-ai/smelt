#![cfg(all(feature = "compiler", feature = "test-support"))]

use rquickjs_jit::bytecode::VerifyLimits;
use rquickjs_jit::compiler::optimized::Tier2Compiler;
use rquickjs_jit::runtime::{FeedbackTable, FunctionKey, ObservedType};
use rquickjs_jit::test_support::SnapshotFixture;

#[test]
fn mixed_bool_branch_leaf_has_a_helper_free_direct_entry() {
    let fixture = SnapshotFixture::compile(
        "(function incrementIf(value,enabled){if(enabled)return value+1;return value;})",
    );
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(32, 2);
    feedback.observe_call(key, &[ObservedType::Int32, ObservedType::Bool]);
    for instruction in verified.instructions() {
        match instruction.opcode().name() {
            "add" => {
                feedback.observe_binary(
                    key,
                    instruction.pc(),
                    ObservedType::Int32,
                    ObservedType::Int32,
                    ObservedType::Int32,
                    Default::default(),
                );
            }
            "return" => {
                feedback.observe_return(key, instruction.pc(), ObservedType::Int32);
            }
            _ => {}
        }
    }
    let clif = Tier2Compiler::host(77)
        .lower_direct_call_with_feedback_for_test(&verified, key, &feedback.snapshot(77))
        .expect("mixed Bool branch leaf must publish a direct entry");
    assert!(
        !clif.contains("call"),
        "direct leaf called a helper: {clif}"
    );
    assert!(
        clif.contains("brif"),
        "both Bool outcomes must remain executable: {clif}"
    );
}

#[cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn exercise_mixed_edge(
    policy: rquickjs_jit::JitTierPolicy,
    force_tier2: bool,
    miss: &str,
    expected: &str,
) {
    use rquickjs::{Context, Function, Runtime};
    use rquickjs_jit::{Jit, JitConfig};
    use std::time::{Duration, Instant};
    unsafe extern "C" {
        fn JS_JitGetHelperCount(
            rt: *mut rquickjs_core::qjs::JSRuntime,
            helper: u32,
            count: *mut u64,
        ) -> i32;
    }
    let runtime = Runtime::new().unwrap();
    let jit = Jit::attach(
        &runtime,
        JitConfig::builder()
            .tier_policy(policy)
            .call_threshold(2)
            .loop_threshold(16)
            .force_optimized_for_test(force_tier2)
            .stress_gc(true)
            .build()
            .unwrap(),
    )
    .unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| ctx.eval::<(), _>(
        "function leaf(value,enabled){if(enabled)return value+1;return value;}\n\
         function caller(target,value,enabled,n){for(let i=0;i<n;i++){value=target(value,enabled);}return value;}"
    )).unwrap();
    let count = || {
        context.with(|ctx| {
            let rt = unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
            let mut count = 0;
            assert_eq!(
                unsafe {
                    JS_JitGetHelperCount(
                        rt,
                        rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL,
                        &mut count,
                    )
                },
                0
            );
            count
        })
    };
    let invoke = |enabled| {
        context.with(|ctx| {
            let caller: Function = ctx.globals().get("caller").unwrap();
            let leaf: Function = ctx.globals().get("leaf").unwrap();
            caller.call::<_, i32>((leaf, 7, enabled, 128)).unwrap()
        })
    };
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    // The timed Tier-2 profitability trial needs eight optimized caller
    // executions and may demote the caller once it decides, which briefly
    // reroutes the edge through generic CALL until the replacement installs.
    // Require enough consecutive stable rounds (two caller entries each) for
    // that trial to have finished before asserting the steady state.
    let mut stable_rounds = 0;
    loop {
        let before = count();
        let entry_before = jit.metrics();
        assert_eq!(invoke(true), 135);
        assert_eq!(invoke(false), 7);
        jit.poll();
        let after = jit.metrics();
        if after.native_entries - entry_before.native_entries == 2
            && (!force_tier2 || after.tier2_entries - entry_before.tier2_entries == 2)
            && after.pending_worker_jobs == 0
            && after.optimized_demotions == entry_before.optimized_demotions
            && count() == before
        {
            stable_rounds += 1;
            if stable_rounds >= 5 {
                break;
            }
        } else {
            stable_rounds = 0;
        }
        assert!(
            Instant::now() < deadline,
            "mixed edge never eliminated CALL: {:?}",
            jit.metrics()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = count();
    let entry_before = jit.metrics();
    for _ in 0..8 {
        assert_eq!(invoke(true), 135);
        assert_eq!(invoke(false), 7);
    }
    assert_eq!(count(), before, "stable mixed edge reentered generic CALL");
    assert_eq!(
        jit.metrics().native_entries - entry_before.native_entries,
        16,
        "every caller must enter once; leaves must not reenter through QuickJS"
    );
    if force_tier2 {
        assert_eq!(jit.metrics().tier2_entries - entry_before.tier2_entries, 16);
    }
    // Each miss gets its own runtime and freshly proven helper-free edge.
    // Earlier deoptimization cannot accidentally remove coverage of this guard.
    context.with(|ctx| {
        assert_eq!(
            ctx.eval::<String, _>(format!("String({miss})")).unwrap(),
            expected
        );
    });
    assert_eq!(jit.metrics().native_entries, jit.metrics().native_exits);
}

fn exercise_guard_misses(policy: rquickjs_jit::JitTierPolicy, force_tier2: bool) {
    for (expression, expected) in [
        ("caller(leaf,2147483647,true,1)", "2147483648"),
        ("caller(leaf,1.5,true,1)", "2.5"),
        ("caller(leaf,'x',true,1)", "x1"),
        ("(()=>{let conversions=0;const input={valueOf(){conversions++;return 9}};return caller(leaf,input,true,1)+conversions})()", "11"),
        ("caller(leaf,7,0,1)", "7"),
        ("caller(leaf,7,{},1)", "8"),
        ("(()=>{leaf=function(value,enabled){return value+20};return caller(leaf,7,true,1)})()", "27"),
        ("(()=>{try{caller(42,7,true,1)}catch(e){return e instanceof TypeError}})()", "true"),
    ] {
        exercise_mixed_edge(policy, force_tier2, expression, expected);
    }
}

#[test]
#[cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn baseline_mixed_edge_eliminates_helper_and_preserves_guard_misses() {
    exercise_guard_misses(rquickjs_jit::JitTierPolicy::BaselineOnly, false);
}

#[test]
#[cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn automatic_mixed_edge_eliminates_helper_and_preserves_guard_misses() {
    exercise_guard_misses(rquickjs_jit::JitTierPolicy::Automatic, false);
}

#[test]
#[cfg(all(
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn tier2_mixed_edge_eliminates_helper_and_preserves_guard_misses() {
    exercise_guard_misses(rquickjs_jit::JitTierPolicy::Automatic, true);
}
