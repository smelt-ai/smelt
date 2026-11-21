//! Native semantic controls for moving pure inline target guards to loop entry.
//! Generated-code tests in optimized.rs establish where guards are emitted;
//! these tests establish that the guarded native code preserves JS behavior.
#![cfg(all(
    feature = "compiler",
    feature = "test-support",
    target_endian = "little",
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use rquickjs::{qjs, Context, Function, Runtime};
use rquickjs_jit::{Jit, JitConfig};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn JS_JitGetHelperCount(
        runtime: *mut rquickjs_core::qjs::JSRuntime,
        helper: u32,
        count: *mut u64,
    ) -> i32;
}

struct NativeLoop {
    context: Context,
    jit: Jit,
    runtime: Runtime,
}

impl NativeLoop {
    fn new(caller: &str) -> Self {
        let runtime = Runtime::new().unwrap();
        let jit = Jit::attach(
            &runtime,
            JitConfig::builder()
                .call_threshold(2)
                .loop_threshold(16)
                .force_optimized_for_test(true)
                .stress_gc(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>(format!(
                    "function leaf(value,enabled){{if(enabled)return value+1;return value;}}\n\
             function replacement(value,enabled){{return value+10;}}\n{caller}"
                ))
            })
            .unwrap();
        let test = Self {
            context,
            jit,
            runtime,
        };
        // Retain the callee before the first caller compilation; otherwise a
        // caller may legitimately publish its conservative generic-call body.
        test.ready("leaf(7,true)", 8.0);
        test
    }

    fn number(&self, expression: &str) -> f64 {
        self.context
            .with(|ctx| ctx.eval::<f64, _>(expression))
            .unwrap()
    }

    fn count(&self, helper: u32) -> u64 {
        self.context.with(|ctx| {
            let mut count = 0;
            let runtime = unsafe { rquickjs_core::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) };
            assert_eq!(
                unsafe { JS_JitGetHelperCount(runtime, helper, &mut count) },
                0
            );
            count
        })
    }

    fn ready(&self, expression: &str, expected: f64) {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            let before = self.jit.metrics();
            let calls = self.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL);
            assert_eq!(self.number(expression), expected);
            let after = self.jit.metrics();
            if after.tier2_entries - before.tier2_entries == 1
                && after.native_entries - before.native_entries == 1
                && after.deopts == before.deopts
                && self.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_CALL) == calls
                && after.pending_worker_jobs == 0
            {
                return;
            }
            self.jit.poll();
            assert!(
                Instant::now() < deadline,
                "caller never reached stable Tier2: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn assert_native_result(&self, expression: &str, expected: f64) {
        let before = self.jit.metrics();
        assert_eq!(self.number(expression), expected, "{expression}");
        let after = self.jit.metrics();
        assert!(
            after.tier2_entries > before.tier2_entries,
            "{before:?} -> {after:?}"
        );
        assert_eq!(after.native_entries, after.native_exits);
    }

    fn ready_with_call_fallback(&self, warm: &str, expected: f64, zero_trip: &str) -> bool {
        let deadline =
            Instant::now() + Duration::from_secs(if cfg!(rquickjs_sanitizer) { 300 } else { 60 });
        loop {
            assert_eq!(self.number(warm), expected);
            self.jit.poll();
            // A zero-trip invocation cannot enter the callee. Its Tier2 entry
            // therefore proves that the caller itself compiled, even when a
            // mutable target requires the generic call boundary in the loop.
            let before = self.jit.metrics();
            assert_eq!(self.number(zero_trip), 7.0);
            let after = self.jit.metrics();
            if after.tier2_entries - before.tier2_entries == 1 {
                return true;
            }
            if after.blacklisted > 0 && after.pending_worker_jobs == 0 {
                // Some mutable function-value phi graphs are outside existing
                // admission. Exercise their deliberate interpreter fallback,
                // without reporting a callee entry as an optimized caller.
                assert!(after.unsupported_opcode_failures > 0, "{after:?}");
                return false;
            }
            assert!(
                Instant::now() < deadline,
                "caller never reached Tier2: {after:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

const LOOP: &str = "function caller(target,n,seed){let value=seed;for(let i=0;i<n;i++){value=target(value,true);}return value;}";

#[test]
fn inline_loop_rechecks_replaced_target_on_the_next_invocation() {
    let test = NativeLoop::new(LOOP);
    test.ready("caller(leaf,128,7)", 135.0);
    // The pure inline graph amortizes polls; a merely direct native call still
    // has a reentrant call effect and cannot satisfy this bound.
    let polls = test.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_POLL);
    test.assert_native_result("caller(leaf,4096,7)", 4103.0);
    let polls = test.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_POLL) - polls;
    assert!(
        (64..=80).contains(&polls),
        "inline loop must amortize its polls, got {polls}"
    );
    // Removing the identity guard would execute the old +1 body four times.
    test.assert_native_result("caller(replacement,4,7)", 47.0);
}

#[test]
fn observable_poll_refreshes_an_unused_guard_before_its_first_call() {
    let test = NativeLoop::new(
        "function caller(target,n,seed,callFrom){let value=seed;for(let i=0;i<n;i++){if(i>=callFrom)value=target(value,true);else value++;}return value;}",
    );
    test.ready("caller(leaf,128,7,0)", 135.0);
    let (leaf_object, replacement_object) = test.context.with(|ctx| {
        let leaf: Function<'_> = ctx.globals().get("leaf").unwrap();
        let replacement: Function<'_> = ctx.globals().get("replacement").unwrap();
        unsafe {
            (
                qjs::JS_VALUE_GET_PTR(leaf.as_value().as_raw()) as usize,
                qjs::JS_VALUE_GET_PTR(replacement.as_value().as_raw()) as usize,
            )
        }
    });
    let callbacks = Arc::new(AtomicUsize::new(0));
    let swapped = Arc::new(AtomicBool::new(false));
    let callback_count = Arc::clone(&callbacks);
    let did_swap = Arc::clone(&swapped);
    test.runtime.set_interrupt_handler(Some(Box::new(move || {
        callback_count.fetch_add(1, Ordering::SeqCst);
        if did_swap
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // Both globals keep their bytecode function objects rooted. Swap
            // their owned bytecode pointers so the already-probed target keeps
            // its object identity but acquires replacement's observable body.
            unsafe {
                let leaf_bytecode = (leaf_object as *mut u8).add(48).cast::<*mut u8>();
                let replacement_bytecode =
                    (replacement_object as *mut u8).add(48).cast::<*mut u8>();
                std::ptr::swap(leaf_bytecode, replacement_bytecode);
            }
        }
        false
    })));
    let polls_before = test.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_POLL);
    let before = test.jit.metrics();
    // QuickJS calls the host interrupt callback every 10,000 helper polls.
    // Delay the only target use until after that callback has replaced it.
    let result = test
        .context
        .with(|ctx| ctx.eval::<f64, _>("caller(leaf,1000000,7,999999)"));
    let after = test.jit.metrics();
    let polls = test.count(rquickjs_core::qjs::JSJitHelperId_JS_JIT_HELPER_POLL) - polls_before;
    test.runtime.set_interrupt_handler(None);
    if swapped.load(Ordering::SeqCst) {
        unsafe {
            let leaf_bytecode = (leaf_object as *mut u8).add(48).cast::<*mut u8>();
            let replacement_bytecode = (replacement_object as *mut u8).add(48).cast::<*mut u8>();
            std::ptr::swap(leaf_bytecode, replacement_bytecode);
        }
    }
    assert!(
        polls >= 64,
        "expected at least 64 native polls, got {polls}"
    );
    assert!(
        callbacks.load(Ordering::SeqCst) > 0,
        "callback was not observed"
    );
    assert!(
        swapped.load(Ordering::SeqCst),
        "callback did not replace target"
    );
    assert!(
        after.tier2_entries > before.tier2_entries,
        "{before:?} -> {after:?}"
    );
    assert!(after.deopts > before.deopts, "{before:?} -> {after:?}");
    assert_eq!(after.native_entries, after.native_exits);
    assert_eq!(result.unwrap(), 1_000_016.0);
}

#[test]
fn zero_trip_noncallable_target_does_not_throw() {
    let test = NativeLoop::new(LOOP);
    test.ready("caller(leaf,128,7)", 135.0);
    // A same-representation target mismatch isolates the moved identity guard
    // from the caller's independent entry type specialization.
    let before = test.jit.metrics();
    test.assert_native_result("caller(replacement,0,19)", 19.0);
    let after_unused_identity = test.jit.metrics();
    assert_eq!(
        after_unused_identity.deopts, before.deopts,
        "an unused target must not trigger the moved identity guard"
    );
    test.assert_native_result("caller(42,0,19)", 19.0);
    // The same target must still throw when a call is actually reached.
    assert!(test
        .context
        .with(|ctx| {
            ctx.eval::<bool, _>(
        "(()=>{try{caller(42,1,19);return false}catch(error){return error instanceof TypeError}})()"
    )
        })
        .unwrap());
    let metrics = test.jit.metrics();
    assert_eq!(metrics.native_entries, metrics.native_exits);
}

#[test]
fn changed_argument_and_phi_local_targets_preserve_conservative_fallback() {
    for caller in [
        "function caller(target,other,n,seed,split){let current=target,value=seed;for(let i=0;i<n;i++){if(i>=split)current=other;value=current(value,true);}return value;}",
        "function caller(target,other,n,seed,split){let value=seed;for(let i=0;i<n;i++){if(i>=split)target=other;value=target(value,true);}return value;}",
    ] {
        let test = NativeLoop::new(caller);
        let tier2 = test.ready_with_call_fallback(
            "caller(leaf,leaf,128,7,3)",
            135.0,
            "caller(leaf,leaf,0,7,3)",
        );
        // Three +1 calls precede three +10 calls, even though feedback saw
        // the same function on both incoming phi edges during compilation.
        if tier2 {
            test.assert_native_result("caller(leaf,replacement,6,7,3)", 40.0);
        } else {
            assert_eq!(test.number("caller(leaf,replacement,6,7,3)"), 40.0);
            let metrics = test.jit.metrics();
            assert_eq!(metrics.native_entries, metrics.native_exits);
        }
    }
}

#[test]
fn inline_overflow_after_a_poll_recovers_the_exact_iteration() {
    let test = NativeLoop::new(LOOP);
    test.ready("caller(leaf,128,7)", 135.0);
    let before = test.jit.metrics();
    // Overflow on iteration 101 follows a poll and must neither replay nor
    // skip a completed inline increment when restoring caller scalar locals.
    test.assert_native_result("caller(leaf,128,2147483547)", 2147483675.0);
    assert!(test.jit.metrics().deopts > before.deopts);
}

#[test]
fn nested_loop_and_skipped_branch_preserve_target_guards() {
    let test = NativeLoop::new(
        "function caller(target,n,m,seed,enabled){let value=seed;for(let i=0;i<n;i++){for(let j=0;j<m;j++){if(enabled)value=target(value,true);else {value++;value++;}}}return value;}",
    );
    assert!(test.ready_with_call_fallback(
        "caller(leaf,8,16,7,true)",
        135.0,
        "caller(leaf,0,16,7,true)",
    ));
    test.assert_native_result("caller(leaf,3,4,7,false)", 31.0);
    test.assert_native_result("caller(42,3,0,7,true)", 7.0);
    test.assert_native_result("caller(42,2,3,7,false)", 19.0);
    test.assert_native_result("caller(replacement,2,3,7,true)", 67.0);
}

#[test]
fn inline_loop_still_polls_and_unwinds_on_interrupt() {
    let test = NativeLoop::new(LOOP);
    test.ready("caller(leaf,128,7)", 135.0);
    let interrupts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&interrupts);
    test.runtime.set_interrupt_handler(Some(Box::new(move || {
        seen.fetch_add(1, Ordering::SeqCst) >= 2
    })));
    let before = test.jit.metrics();
    test.context.with(|ctx| {
        assert!(ctx.eval::<f64, _>("caller(leaf,1000000000,7)").is_err());
        // Consume QuickJS's pending uncatchable interrupt exception before
        // testing that the same runtime can execute another invocation.
        drop(ctx.catch());
    });
    test.runtime.set_interrupt_handler(None);
    let after = test.jit.metrics();
    assert!(after.tier2_entries > before.tier2_entries);
    assert!(interrupts.load(Ordering::SeqCst) >= 3);
    assert_eq!(after.native_entries, after.native_exits);
    assert_eq!(test.number("caller(leaf,4,7)"), 11.0);
}
