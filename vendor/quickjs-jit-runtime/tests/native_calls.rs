//! P3a native-call convention: pure self-recursive Int32 functions run their
//! recursion as direct native calls, and every failure re-executes the CALL
//! in the interpreter with identical results, exceptions and effects.
// Sanitizer builds are excluded: at -O0 with instrumentation one interpreter
// level takes tens of KiB of C stack, so the default 1 MiB JS stack overflows
// within a few dozen levels and the recursion-depth semantics these tests
// compare cannot be exercised.
#![cfg(all(
    not(rquickjs_sanitizer),
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{Context, Runtime};
use rquickjs_core::qjs;
use rquickjs_jit::bytecode::VerifyLimits;
use rquickjs_jit::runtime::{FeedbackTable, FunctionKey, ObservedType};
use rquickjs_jit::test_support::SnapshotFixture;
use rquickjs_jit::{Jit, JitConfig, JitTierPolicy};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(rt: *mut qjs::JSRuntime, helper: u32, count: *mut u64) -> i32;
}

/// Serializes this file's tests. Native recursion is admitted only after
/// automatic tiering has profiled the installed baseline, so tests running
/// concurrently on a slow (coverage-instrumented) build can starve that
/// profile until the deadline.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const FIBONACCI: &str = "function fib(n){if(n<2)return n;return fib(n-1)+fib(n-2);}";

fn generic_calls(context: &Context) -> u64 {
    context.with(|ctx| {
        let rt = unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
        let mut count = 0;
        assert_eq!(
            unsafe { JS_JitGetHelperCount(rt, qjs::JSJitHelperId_JS_JIT_HELPER_CALL, &mut count) },
            0
        );
        count
    })
}

struct Harness {
    _runtime: Runtime,
    jit: Jit,
    context: Context,
}

impl Harness {
    fn new(source: &str, force_tier2: bool) -> Self {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .tier_policy(JitTierPolicy::Automatic)
                .call_threshold(2)
                .loop_threshold(16)
                .force_optimized_for_test(force_tier2)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| ctx.eval::<(), _>(source)).unwrap();
        Self {
            _runtime: runtime,
            jit,
            context,
        }
    }

    fn eval_string(&self, expression: &str) -> String {
        self.context.with(|ctx| {
            ctx.eval::<String, _>(format!("String({expression})"))
                .unwrap_or_else(|error| {
                    let exception = ctx.catch();
                    panic!("{expression} threw {error}: {exception:?}")
                })
        })
    }

    /// Runs `expression` until its self-recursive calls stop using the
    /// generic CALL bridge: one Tier 2 entry per evaluation and no helper
    /// CALL, so the whole recursion runs as native calls.
    fn warm_native(&self, expression: &str, expected: &str) {
        self.warm_native_from(expression, expected, 0);
    }

    /// Like `warm_native`, but `outside_calls` helper CALLs per evaluation
    /// come from compiled callers outside the recursion (for example a Tier 1
    /// wrapper with an exception region, which Tier 2 leaves on Tier 1): they
    /// enter the recursion's root, and every recursive call is still native.
    fn warm_native_from(&self, expression: &str, expected: &str, outside_calls: u64) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let calls = generic_calls(&self.context);
            let before = self.jit.metrics();
            assert_eq!(self.eval_string(expression), expected);
            let after = self.jit.metrics();
            self.jit.poll();
            if after.tier2_entries > before.tier2_entries
                && after.pending_worker_jobs == 0
                && generic_calls(&self.context) <= calls + outside_calls
                && after.deopts == before.deopts
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "native recursion never engaged for {expression}: {:?}",
                self.jit.metrics()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn pure_self_recursion_publishes_a_helper_free_native_entry() {
    let _serial = serial();
    let fixture = SnapshotFixture::compile(&format!("{FIBONACCI} fib"));
    let verified = fixture.snapshot().verify(VerifyLimits::default()).unwrap();
    let key = FunctionKey::new(
        verified.snapshot().function_id(),
        verified.snapshot().generation(),
    );
    let mut feedback = FeedbackTable::new(64, 2);
    feedback.observe_call(key, &[ObservedType::Int32]);
    for instruction in verified.instructions() {
        match instruction.opcode().name() {
            "add" | "sub" => {
                feedback.observe_binary(
                    key,
                    instruction.pc(),
                    ObservedType::Int32,
                    ObservedType::Int32,
                    ObservedType::Int32,
                    Default::default(),
                );
            }
            "return" => feedback.observe_return(key, instruction.pc(), ObservedType::Int32),
            _ => {}
        }
    }
    let signature = feedback
        .snapshot(7)
        .bounded_specialization(key)
        .expect("Int32 signature");
    let plan = rquickjs_jit::compiler::native_call::plan(&verified, &signature)
        .expect("fibonacci is a pure self-recursive Int32 function");
    assert_eq!(plan.arity(), 1);
    // At least one measured interpreter level plus the value slots.
    assert!(
        plan.frame_charge() >= rquickjs_jit::compiler::native_call::interpreter_frame_bytes() + 16
    );

    // Anything that could run code, allocate or observe the frame is refused.
    for rejected in [
        "function f(n){if(n<2)return n;return f(n-1)+g(n-2);} f",
        "function f(n){if(n<2)return n;return f(n-1)+this.x;} f",
        "function f(n){for(let i=0;i<n;i++){} return n<1?0:f(n-1);} f",
        "function f(n){return n;} f",
        "function f(n){if(n<2)return n;return f(n-1,1);} f",
        "function f(n){if(n<2)return n;return f(n-1)/2;} f",
        "function f(n){try{return n<1?0:f(n-1)}catch(e){return 0}} f",
    ] {
        let fixture = SnapshotFixture::compile(rejected);
        let Ok(verified) = fixture.snapshot().verify(VerifyLimits::default()) else {
            continue;
        };
        let key = FunctionKey::new(
            verified.snapshot().function_id(),
            verified.snapshot().generation(),
        );
        let mut feedback = FeedbackTable::new(64, 2);
        feedback.observe_call(key, &[ObservedType::Int32]);
        for instruction in verified.instructions() {
            if instruction.opcode().name() == "return" {
                feedback.observe_return(key, instruction.pc(), ObservedType::Int32);
            }
        }
        if let Some(signature) = feedback.snapshot(7).bounded_specialization(key) {
            assert!(
                rquickjs_jit::compiler::native_call::plan(&verified, &signature).is_none(),
                "{rejected} must not have a native entry"
            );
        }
    }
}

#[test]
fn automatic_fibonacci_recursion_runs_as_native_calls() {
    let _serial = serial();
    let harness = Harness::new(FIBONACCI, false);
    harness.warm_native("fib(15)", "610");
    let calls = generic_calls(&harness.context);
    let before = harness.jit.metrics();
    for _ in 0..16 {
        assert_eq!(harness.eval_string("fib(20)"), "6765");
    }
    let after = harness.jit.metrics();
    assert_eq!(generic_calls(&harness.context), calls);
    assert_eq!(after.deopts, before.deopts);
    // Only the outermost call enters through QuickJS.
    assert_eq!(after.tier2_entries - before.tier2_entries, 16);
    assert_eq!(after.native_entries, after.native_exits);
}

fn check_after_warmup(source: &str, warm: &[(&str, &str)], probes: &[(&str, &str)]) {
    for force in [false, true] {
        // Every probe gets a fresh, freshly-warmed runtime so an earlier
        // deoptimization cannot hide the path under test.
        for (expression, expected) in probes {
            let harness = Harness::new(source, force);
            for (warm, result) in warm {
                harness.warm_native(warm, result);
            }
            assert_eq!(
                harness.eval_string(expression),
                *expected,
                "{expression} (force_tier2={force})"
            );
            for (warm, result) in warm {
                assert_eq!(
                    harness.eval_string(warm),
                    *result,
                    "{warm} after {expression}"
                );
            }
            let metrics = harness.jit.metrics();
            assert_eq!(metrics.native_entries, metrics.native_exits);
        }
    }
}

#[test]
fn native_recursion_failures_reexecute_exactly_in_the_interpreter() {
    let _serial = serial();
    check_after_warmup(
        "function sum(n){return n<=0?0:n+sum(n-1);}\n\
         function big(n){return n<=0?2147483600:1+big(n-1);}\n\
         function flip(n,k){if(n<=0)return k;return flip(n-1,k)*-1;}",
        &[
            ("sum(20)", "210"),
            ("big(20)", "2147483620"),
            ("flip(20,5)", "5"),
        ],
        &[
            // Int32 overflow deep in the chain produces the Float64 result.
            ("big(100)", "2147483700"),
            ("big(47)+big(48)", "4294967295"),
            // Non-Int32 arguments miss the entry guard.
            ("sum(2.5)", "4.5"),
            ("sum('3')", "33"),
            ("flip(3,2.5)", "-2.5"),
            // -0 cannot be represented by the Int32 result.
            ("Object.is(flip(1,0),-0)", "true"),
            ("Object.is(flip(2,0),0)", "true"),
        ],
    );
}

#[test]
fn native_recursion_observes_rebinding_and_refuses_accessors() {
    let _serial = serial();
    check_after_warmup(
        FIBONACCI,
        &[("fib(12)", "144")],
        &[
            // The callee's global self binding changes between chains.
            (
                "(()=>{const old=fib;fib=function(n){return 100};const r=old(10);fib=old;return r})()",
                "200",
            ),
        ],
    );
    check_after_warmup(
        "globalThis.fib=function(n){if(n<2)return n;return fib(n-1)+fib(n-2);};",
        &[("fib(12)", "144")],
        &[
            // An accessor binding runs code on every lookup; the native path
            // must not skip those getter calls.
            (
                "(()=>{const impl=fib;let hits=0;delete globalThis.fib;\
                 Object.defineProperty(globalThis,'fib',{configurable:true,get(){hits++;return impl}});\
                 const r=impl(6);delete globalThis.fib;globalThis.fib=impl;return r+':'+hits})()",
                "8:24",
            ),
        ],
    );
}

#[test]
fn native_recursion_observes_a_shadowing_lexical_global() {
    let _serial = serial();
    for force in [false, true] {
        let harness = Harness::new(
            "globalThis.fib=function(n){if(n<2)return n;return fib(n-1)+fib(n-2);};",
            force,
        );
        harness.warm_native("fib(12)", "144");
        harness
            .context
            .with(|ctx| {
                ctx.eval::<(), _>("globalThis.impl=fib;")?;
                ctx.eval::<(), _>("let fib=function(n){return 1};")
            })
            .unwrap();
        assert_eq!(harness.eval_string("impl(6)"), "2");
        assert_eq!(harness.eval_string("impl(12)"), "2");
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

#[test]
fn native_recursion_stack_exhaustion_throws_the_interpreter_range_error() {
    let _serial = serial();
    let source = "function depth(n){return n<=0?0:1+depth(n-1);}";
    let harness = Harness::new(source, false);
    harness.warm_native("depth(40)", "40");
    let started = Instant::now();
    assert_eq!(
        harness.eval_string(
            "(()=>{try{return depth(1e6)}catch(e){return e instanceof RangeError}})()"
        ),
        "true"
    );
    // A retry per interpreter level would make exhaustion quadratic.
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(harness.eval_string("depth(100)"), "100");
    let metrics = harness.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
    // The interpreter's own maximum depth is still reachable.
    let interpreter = Runtime::new().unwrap();
    let context = Context::full(&interpreter).unwrap();
    let limit: i32 = context.with(|ctx| {
        ctx.eval(format!(
            "{source} let lo=1,hi=1<<20;while(lo<hi){{const m=(lo+hi+1)>>1;try{{depth(m);lo=m}}catch(e){{hi=m-1}}}} lo"
        ))
        .unwrap()
    });
    // Warm native code never reports a result where the interpreter throws.
    assert_eq!(
        harness.eval_string(&format!(
            "(()=>{{try{{return depth({})}}catch(e){{return e.name}}}})()",
            limit + 64
        )),
        "RangeError"
    );
}

#[test]
fn native_recursion_polls_the_interrupt_handler() {
    let _serial = serial();
    let harness = Harness::new(FIBONACCI, false);
    harness.warm_native("fib(15)", "610");
    let polls = Arc::new(AtomicUsize::new(0));
    let interrupt = Arc::new(AtomicBool::new(false));
    harness._runtime.set_interrupt_handler(Some(Box::new({
        let polls = Arc::clone(&polls);
        let interrupt = Arc::clone(&interrupt);
        move || {
            polls.fetch_add(1, Ordering::SeqCst);
            interrupt.load(Ordering::SeqCst)
        }
    })));
    // A handler that never interrupts must not change the result.
    assert_eq!(harness.eval_string("fib(22)"), "17711");
    assert!(
        polls.load(Ordering::SeqCst) > 0,
        "native recursion never polled"
    );
    interrupt.store(true, Ordering::SeqCst);
    let result = harness.context.with(|ctx| {
        ctx.eval::<i32, _>("fib(25)").map_err(|error| {
            // Clear the uncatchable error so later evaluations start clean.
            let exception = ctx.catch();
            format!("{error}: {exception:?}")
        })
    });
    let error = result.expect_err("interrupt was not delivered");
    assert!(error.contains("interrupted"), "{error}");
    interrupt.store(false, Ordering::SeqCst);
    harness._runtime.set_interrupt_handler(None);
    assert_eq!(harness.eval_string("fib(20)"), "6765");
    let metrics = harness.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn monomorphic_callers_link_to_a_published_native_entry() {
    let _serial = serial();
    let source = format!(
        "{FIBONACCI}\nfunction caller(n){{let s=0;for(let i=0;i<n;i++){{s+=fib(12);}}return s;}}"
    );
    for force in [false, true] {
        let harness = Harness::new(&source, force);
        // Publish the callee's native entry before the caller's Tier 2.
        harness.warm_native("fib(15)", "610");
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let calls = generic_calls(&harness.context);
            let before = harness.jit.metrics();
            assert_eq!(harness.eval_string("caller(64)"), "9216");
            let after = harness.jit.metrics();
            harness.jit.poll();
            // One QuickJS entry (the caller); every fib call is native.
            if after.tier2_entries - before.tier2_entries == 1
                && after.native_entries - before.native_entries == 1
                && generic_calls(&harness.context) == calls
                && after.pending_worker_jobs == 0
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "caller never linked to the native entry (force_tier2={force}): {:?} -> {:?}",
                before,
                after
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // A rebinding makes the callee identity guard miss; the CALL is
        // re-executed by the interpreter against the new binding.
        assert_eq!(
            harness.eval_string(
                "(()=>{const old=fib;fib=function(n){return 2};const r=caller(3);fib=old;return r})()"
            ),
            "6"
        );
        assert_eq!(harness.eval_string("caller(2)"), "288");
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

#[test]
fn shallow_recursion_stays_out_of_native_code() {
    let _serial = serial();
    // Four calls per outside entry cannot amortize QuickJS's native entry.
    let harness = Harness::new(
        "function shallow(n){return n<=0?0:n+shallow(n-1);}\n\
         function drive(k){let s=0;for(let i=0;i<k;i++)s+=shallow(i&3);return s;}",
        false,
    );
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
    while harness.jit.metrics().generic_call_rejections == 0 {
        assert_eq!(harness.eval_string("drive(64)"), "160");
        harness.jit.poll();
        assert!(
            Instant::now() < deadline,
            "shallow recursion was never rejected: {:?}",
            harness.jit.metrics()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = harness.jit.metrics();
    assert_eq!(harness.eval_string("drive(64)"), "160");
    assert_eq!(harness.jit.metrics().tier2_entries, before.tier2_entries);
}

#[test]
fn float64_self_recursion_runs_natively_and_guards_its_signature() {
    let _serial = serial();
    check_after_warmup(
        // Warm-up inputs never produce integral intermediates, which the
        // baseline tier would re-tag as Int32 and make the signature mixed.
        "function tri(x){return x<=0?0.5:x+tri(x-1.25);}\n\
         function halves(x){if(x<1)return x;return halves(x/2)+halves(x/4);}",
        &[
            ("tri(30.3)", "383.0000000000001"),
            ("halves(1500.3)", "138.08913574218752"),
        ],
        &[
            // An Int32 argument misses the Float64 entry guard.
            ("tri(30)", "375.5"),
            ("tri(0.25)", "0.75"),
            ("tri(-Infinity)", "0.5"),
            ("Object.is(tri(-0),0.5)", "true"),
            ("halves(0.75)", "0.75"),
            ("Object.is(halves(-0.5),-0.5)", "true"),
            ("halves(2.5e-324)", "5e-324"),
        ],
    );
}

/// Evaluates `expression` and returns the error text, clearing the pending
/// (possibly uncatchable) exception so later evaluations start clean.
fn eval_error(harness: &Harness, expression: &str) -> Result<i32, String> {
    harness.context.with(|ctx| {
        ctx.eval::<i32, _>(expression).map_err(|error| {
            let exception = ctx.catch();
            format!("{error}: {exception:?}")
        })
    })
}

#[test]
fn native_recursion_delivers_a_one_shot_interrupt() {
    let _serial = serial();
    // A host handler that answers `true` exactly once: the chain's poll
    // consumes that answer, so the retried CALL must deliver it instead of
    // asking the handler again (which would now answer `false`).
    for force in [false, true] {
        let harness = Harness::new(FIBONACCI, force);
        harness.warm_native("fib(15)", "610");
        let armed = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(AtomicUsize::new(0));
        harness._runtime.set_interrupt_handler(Some(Box::new({
            let armed = Arc::clone(&armed);
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                armed.swap(false, Ordering::SeqCst)
            }
        })));
        let error = eval_error(&harness, "fib(27)")
            .expect_err("a one-shot interrupt was lost (force_tier2={force})");
        assert!(error.contains("interrupted"), "{error}");
        assert!(!armed.load(Ordering::SeqCst));
        assert!(calls.load(Ordering::SeqCst) > 0);
        // The delivered interrupt is not delivered a second time.
        assert_eq!(harness.eval_string("fib(20)"), "6765");
        harness._runtime.set_interrupt_handler(None);
        assert_eq!(harness.eval_string("fib(20)"), "6765");
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

/// Like [`check_after_warmup`], with every expectation computed by a plain
/// interpreter runtime without a JIT.
fn check_against_interpreter(source: &str, warm: &[&str], probes: &[&str]) {
    let interpreter = Runtime::new().unwrap();
    let context = Context::full(&interpreter).unwrap();
    let expect = |expression: &str| -> String {
        context.with(|ctx| {
            ctx.eval::<(), _>(source).unwrap();
            ctx.eval::<String, _>(format!("String({expression})"))
                .unwrap()
        })
    };
    let warm: Vec<(&str, String)> = warm.iter().map(|w| (*w, expect(w))).collect();
    let probes: Vec<(&str, String)> = probes.iter().map(|p| (*p, expect(p))).collect();
    let warm: Vec<(&str, &str)> = warm.iter().map(|(e, r)| (*e, r.as_str())).collect();
    let probes: Vec<(&str, &str)> = probes.iter().map(|(e, r)| (*e, r.as_str())).collect();
    check_after_warmup(source, &warm, &probes);
}

#[test]
fn native_tail_self_calls_return_the_callee_result() {
    let _serial = serial();
    check_against_interpreter(
        "function count(n,acc){if(n<=0)return acc;return count(n-1,acc+1);}\n\
         function down(a,b){return a<=b?a:down(a-b,b);}",
        &["count(30,0)", "down(1000,7)"],
        &[
            "count(100,5)",
            // Overflow of the tail-call argument deep in the chain.
            "count(20,2147483640)",
            "down(1071,462)",
            "down(10,10)",
            "down(-2147483648,1)",
        ],
    );
}

#[test]
fn native_recursion_resumes_after_a_caught_range_error() {
    let _serial = serial();
    // Both chains start from exactly the same stack height: the same
    // wrapper, called from the same global code.
    let source = "function depth(n){return n<=0?0:1+depth(n-1);}\n\
                  function run(n){try{return depth(n)}catch(e){return -1}}";
    let harness = Harness::new(source, false);
    // `run` has an exception region, so it stays on Tier 1 (P2c) and enters
    // `depth` through one generic CALL per evaluation; the recursion below
    // that entry must still be entirely native.
    // Depths stay well inside the interpreter's limit in debug and
    // coverage builds, whose C frames are several times larger.
    harness.warm_native_from("run(100)", "100", 1);
    assert_eq!(harness.eval_string("run(1e6)"), "-1");
    // A later chain from that height runs natively again instead of being
    // refused (and deoptimized) at every call.
    harness.warm_native_from("run(120)", "120", 1);
    let metrics = harness.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn native_float64_comparisons_match_the_interpreter() {
    let _serial = serial();
    check_against_interpreter(
        // Results stay non-integral (0.1 plus quarters) so the baseline tier
        // never re-tags them as Int32.
        "function cmp(x){if(x>0.5){if(x>1.5)return 0.5+cmp(x-1.125);return 0.75+cmp(x-1.125);}return 0.1;}",
        &["cmp(20.3)"],
        &[
            // NaN fails every ordered comparison and ends the recursion.
            "cmp(NaN)",
            "cmp(5.5+NaN)",
            "Object.is(cmp(-0),0.1)",
            "cmp(2)",
            "cmp(1.5)",
            "cmp(0.5)",
        ],
    );
}

#[test]
fn native_int32_lnot_matches_the_interpreter() {
    let _serial = serial();
    check_against_interpreter(
        "function inv(n){if(!n)return 0;return 1+inv(n-1);}",
        &["inv(30)"],
        &[
            // `!n` on Int32 zero and non-zero, and on non-Int32 arguments.
            "inv(0)",
            "inv(-0)",
            "inv(3)",
            "inv(true)",
        ],
    );
}

#[test]
fn a_linked_caller_follows_an_aliased_callee_binding() {
    let _serial = serial();
    // The caller reaches the callee through a parameter, not through the
    // callee's own global name, so the chain must still prove that name.
    let source = format!(
        "{FIBONACCI}\nfunction caller(n,g){{let s=0;for(let i=0;i<n;i++){{s+=g(12);}}return s;}}"
    );
    for force in [false, true] {
        let harness = Harness::new(&source, force);
        harness.warm_native("fib(15)", "610");
        for _ in 0..64 {
            assert_eq!(harness.eval_string("caller(8,fib)"), "1152");
            harness.jit.poll();
        }
        // `old` passes the caller's identity guard, but its self calls now
        // resolve to the new global binding: old(12) = 1 + 1.
        assert_eq!(
            harness.eval_string(
                "(()=>{const old=fib;fib=function(n){return 1};const r=caller(2,old);fib=old;return r})()"
            ),
            "4"
        );
        assert_eq!(harness.eval_string("caller(2,fib)"), "288");
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

#[test]
fn a_replaced_callee_invalidates_its_linked_caller() {
    let _serial = serial();
    // The caller guards raw callee identities. When the callee is freed and a
    // different function with the same name takes its place, possibly at the
    // same addresses, the caller must not run the old native body.
    let source = format!(
        "{FIBONACCI}\nfunction caller(n){{let s=0;for(let i=0;i<n;i++){{s+=fib(12);}}return s;}}"
    );
    fn reference(n: i64, k: i64) -> i64 {
        if n < 2 {
            n + k
        } else {
            reference(n - 1, k) + reference(n - 2, k)
        }
    }
    for force in [false, true] {
        let harness = Harness::new(&source, force);
        harness.warm_native("fib(15)", "610");
        for _ in 0..64 {
            assert_eq!(harness.eval_string("caller(8)"), "1152");
            harness.jit.poll();
        }
        for round in 0..16i64 {
            harness
                .context
                .with(|ctx| {
                    ctx.eval::<(), _>(format!(
                        "fib=(0,eval)('(function fib(n){{if(n<2)return n+{round};return fib(n-1)+fib(n-2);}})');"
                    ))
                })
                .unwrap();
            harness._runtime.run_gc();
            harness.jit.poll();
            assert_eq!(
                harness.eval_string("caller(3)"),
                (reference(12, round) * 3).to_string(),
                "round {round} (force_tier2={force})"
            );
        }
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

#[test]
fn native_call_sites_release_the_global_callee_on_every_edge() {
    let _serial = serial();
    // `acc` is loaded by get_var (an owned stack slot). The native site
    // frees that reference after success and hands it to the interpreter on
    // every deopt edge; a leak keeps the replaced callee alive and an extra
    // release frees it early.
    let source = "function acc(n,k){return n<=0?k:1+acc(n-1,k);}\n\
                  function caller(n,x,k){let s=0;for(let i=0;i<n;i++){s+=acc(x,k);}return s;}";
    for force in [false, true] {
        let harness = Harness::new(source, force);
        harness.warm_native("acc(40,1)", "41");
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let calls = generic_calls(&harness.context);
            let before = harness.jit.metrics();
            assert_eq!(harness.eval_string("caller(64,20,1)"), "1344");
            let after = harness.jit.metrics();
            harness.jit.poll();
            if after.native_entries - before.native_entries == 1
                && generic_calls(&harness.context) == calls
                && after.pending_worker_jobs == 0
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "caller never linked (force_tier2={force}): {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // Tag miss, chain retry (Int32 overflow), identity miss, success.
        assert_eq!(harness.eval_string("caller(2,3.5,1)"), "10");
        assert_eq!(harness.eval_string("caller(1,20,2147483640)"), "2147483660");
        assert_eq!(
            harness.eval_string(
                "(()=>{const old=acc;acc=function(n,k){return k};const r=caller(2,5,7);acc=old;return r})()"
            ),
            "14"
        );
        assert_eq!(harness.eval_string("caller(4,20,1)"), "84");
        assert_eq!(
            harness.eval_string(
                "(globalThis.weak=new WeakRef(acc),acc=function(n,k){return 0},caller(3,20,1))"
            ),
            "0"
        );
        harness._runtime.run_gc();
        harness.jit.poll();
        harness._runtime.run_gc();
        assert_eq!(
            harness.eval_string("weak.deref()===undefined"),
            "true",
            "the replaced callee leaked (force_tier2={force})"
        );
        let metrics = harness.jit.metrics();
        assert_eq!(metrics.native_entries, metrics.native_exits);
    }
}

#[test]
fn granted_fast_entries_keep_native_chain_failures_exact() {
    let _serial = serial();
    // P1c x P3a: a native-recursive root has its bounded Tier 2 trial decided
    // on its first optimized exit, so QuickJS may then enter it through
    // granted callback-free entries. A chain failure under such an entry
    // (stack exhaustion, a due interrupt) must still deoptimize exactly,
    // report its unpaired exit, and leave later granted entries working.
    use rquickjs::Function;
    let harness = Harness::new("function depth(n){return n<=0?0:1+depth(n-1);}", false);
    harness.warm_native("depth(200)", "200");
    let call = |n: f64| {
        harness.context.with(|ctx| {
            let depth: Function = ctx.globals().get("depth").unwrap();
            depth.call::<_, i32>((n,)).map_err(|error| {
                // Clear the (possibly uncatchable) error for later calls.
                let exception = ctx.catch();
                format!("{error}: {exception:?}")
            })
        })
    };
    let steady = |n: i32| {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            harness.jit.poll();
            let before = harness.jit.metrics();
            for _ in 0..1024 {
                assert_eq!(call(f64::from(n)), Ok(n));
            }
            let after = harness.jit.metrics();
            let entries = after.native_entries - before.native_entries;
            let acquisitions = after.native_acquisitions - before.native_acquisitions;
            if entries >= 1024 && acquisitions * 64 < entries && after.deopts == before.deopts {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "granted native recursion never became steady: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    steady(64);

    let error = call(1e6).expect_err("stack exhaustion must throw");
    assert!(
        error.contains("RangeError") || error.contains("stack"),
        "{error}"
    );
    steady(96);

    let interrupt = Arc::new(AtomicBool::new(true));
    harness._runtime.set_interrupt_handler(Some(Box::new({
        let interrupt = Arc::clone(&interrupt);
        move || interrupt.load(Ordering::SeqCst)
    })));
    // Each granted chain spends the shared interrupt budget; the chain that
    // exhausts it polls the handler, retries in the interpreter and delivers
    // the interrupt there without polling again.
    let error = (0..1024)
        .find_map(|_| call(200.0).err())
        .expect("interrupt was not delivered");
    assert!(error.contains("interrupted"), "{error}");
    interrupt.store(false, Ordering::SeqCst);
    harness._runtime.set_interrupt_handler(None);
    steady(128);

    let metrics = harness.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits, "{metrics:?}");
}
